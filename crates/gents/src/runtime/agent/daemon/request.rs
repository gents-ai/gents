use anyhow::Result;
use tracing::Instrument;

use super::{AgentDaemon, HandleRequestOutcome, RequestInterruptionSource};
use gents_inference::admission::{self, AdmissionCallContext};
use gents_loop::loop_stream::TaggedMessage;
use gents_observe::observe::runtime_trace::RequestTraceAttrs;

mod prepare;

const CANCELLATION_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);

/// Callers must flush received auxiliary output after interruption. The future
/// must not own a recorded host command (task hook, tool call) or a terminal
/// transaction, whose completion has separate drain and fencing obligations;
/// an unrecorded managed process is killed with its group when dropped.
pub(super) async fn prepare_unless_interrupted<T>(
    interrupt_rx: &mut tokio::sync::watch::Receiver<
        Option<gents_session::request::interrupt::InterruptIntent>,
    >,
    preparation: impl std::future::Future<Output = T>,
) -> Option<T> {
    if interrupt_rx.borrow_and_update().is_some() {
        return None;
    }
    tokio::pin!(preparation);
    loop {
        tokio::select! {
            biased;
            changed = interrupt_rx.changed() => {
                if changed.is_err() {
                    return Some(preparation.await);
                }
                if interrupt_rx.borrow_and_update().is_some() {
                    return None;
                }
            }
            result = &mut preparation => return Some(result),
        }
    }
}

pub(super) async fn flush_received_preparation_partial(context: &str) -> Result<()> {
    match tokio::time::timeout(
        gents_store::config_client::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
        gents_inference::rendered_request::scope::flush_received_auxiliary_partial(),
    ).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(super::ShutdownDrainFailure {
            reason: format!(
                "{context} partial output persistence failed: {}",
                gents_protocol::error_chain::ErrorChain(&error)
            ),
        }.into()),
        Err(_) => Err(super::ShutdownDrainFailure {
            reason: format!("{context} partial output persistence exceeded the embedded storage-step deadline; commit outcome must be recovered from durable state"),
        }.into()),
    }
}

impl<M> AgentDaemon<M>
where
    M: gents_inference::llm::rig_compat::ProviderModel,
{
    /// Size the exact first-turn provider request for session-compaction
    /// admission through the owned loop's assembly owner. The loop remains the
    /// sole final legality authority.
    #[allow(clippy::too_many_arguments)]
    async fn estimate_initial_provider_input(
        &self,
        request: &gents_protocol::row::AgentRequest,
        preamble: String,
        history: &[TaggedMessage],
        skill_reminders: &[gents_inference::llm::message::Message],
        request_context_message: Option<&gents_inference::llm::message::Message>,
        folded_prompts: &[crate::runtime::agent::loop_stream::FoldedPrompt],
        aggregate_token_budget: Option<crate::runtime::agent::loop_stream::AggregateTokenBudget>,
        replay: &gents_loop::loop_stream::LoopReplayInput,
        resume_from_history: bool,
        wake_notifications: &[TaggedMessage],
    ) -> Result<usize> {
        let config = crate::turn::completion_factory::loop_config_for_request(
            &self.agent_config,
            preamble,
            request,
            aggregate_token_budget,
            self.loop_tools.len(),
            self.account_hops,
        )?;
        let provider_history = skill_reminders
            .iter()
            .cloned()
            .map(TaggedMessage::unassociated)
            .chain(history.iter().cloned())
            .collect::<Vec<_>>();
        let (mut provider_history, prompt) = super::inference::request_entry(
            provider_history,
            &request.input.authored_message(&request.content)?,
            resume_from_history,
            request_context_message,
            wake_notifications,
        )?;
        let mut new_messages = gents_loop::loop_stream::assemble_new_messages(
            if resume_from_history {
                None
            } else {
                request_context_message.cloned()
            },
            prompt,
        );
        new_messages.extend(
            folded_prompts
                .iter()
                .map(|folded| TaggedMessage::unassociated(folded.message.clone())),
        );
        let assembled = {
            let _estimate_seed = replay.arm_estimate_seed();
            gents_loop::loop_stream::assemble_provider_request(
                self.model.as_ref(),
                &mut provider_history,
                &mut new_messages,
                self.loop_tools.as_ref(),
                &config,
                replay,
            )
            .await
        };
        let provider_request = assembled.map_err(anyhow::Error::new)?;
        Ok(config
            .provider_input_counter
            .estimate_request(&provider_request)?)
    }

    pub(super) async fn handle_request(
        &mut self,
        lifecycle: &mut gents_session::request::lifecycle::RequestLifecycle,
        stream_writer: &gents_session::request::streaming::DefraStreamWriter,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        mut interrupt_rx: tokio::sync::watch::Receiver<
            Option<gents_session::request::interrupt::InterruptIntent>,
        >,
    ) -> Result<HandleRequestOutcome> {
        let request_token = tokio_util::sync::CancellationToken::new();
        let request = lifecycle.request().clone();
        let effective_sampling = self.agent_config.sampling;
        effective_sampling.validate_for_provider(
            self.agent_config.backend_provider_kind,
            self.agent_config.openai_wire_api,
        )?;
        let effective_seed = effective_sampling.seed;
        let admitted = self.load_admission(&request, lifecycle.resumed_from_backend());
        let Some(admitted) = prepare_unless_interrupted(&mut interrupt_rx, admitted).await else {
            return Ok(HandleRequestOutcome::Interrupted(
                RequestInterruptionSource::Claimed,
            ));
        };
        let (resume_from_history, resume_same_request, aggregate_token_budget, context_calibration) =
            admitted?;
        let trace_attrs = RequestTraceAttrs::from_request(&request);
        let agent_name = self.agent_config.agent_did.clone();
        let admission_context = AdmissionCallContext::for_request(
            &request,
            lifecycle.agent_did(),
            lifecycle.backend_id(),
        );
        // One capture scope for the whole request: the pre-request compaction
        // summarizer issues real provider calls whose input is the entire
        // pre-truncated transcript, so scoping only the inference block left
        // those calls with no ambient scope and no durable record. It must
        // stay a single scope instance: the per-kind sequence keeping the
        // inference loop, the summarizer, and its JSON fallback from colliding
        // on `(turn 0, attempt 0)` lives in the scope.
        let request_commit_cid = lifecycle
            .request_commit_cid()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "claimed AgentRequest {} has no exact DefraDB commit CID",
                    request.doc_id
                )
            })?
            .to_string();
        let capture_scope = self
            .request_capture_scope(&request, lifecycle, stream_writer, &request_commit_cid)
            .await?;
        let handled = admission::scope_request(admission_context, async {
            let response_agent_did = lifecycle.agent_did().to_string();
            lifecycle
                .begin_owned_execution(stream_writer)
                .instrument(tracing::info_span!(
                    "request.begin_response",
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    node_did = %request.node_did,
                    agent_did = %response_agent_did,
                    request_hop = trace_attrs.request_hop,
                ))
                .await?;
            let preparation = self.prepare_request_input(prepare::RequestInputs {
                request: &request,
                lifecycle: &*lifecycle,
                request_commit_cid: &request_commit_cid,
                agent_name: &agent_name,
                trace_attrs: &trace_attrs,
                aggregate_token_budget: &aggregate_token_budget,
                context_calibration: &context_calibration,
                effective_seed,
                resume_from_history,
                resume_same_request,
            });
            let Some(prepared) = prepare_unless_interrupted(&mut interrupt_rx, preparation).await
            else {
                flush_received_preparation_partial("interrupted request preparation").await?;
                return Ok(HandleRequestOutcome::Interrupted(
                    RequestInterruptionSource::Preparation,
                ));
            };
            let prepare::PreparedRequestInput {
                workspace,
                request_context_message,
                built,
                replay,
                consumed_prompts,
                folded_prompts,
                wake_notifications,
            } = prepared?;

            let doc_id = lifecycle.request().doc_id.clone();

            let inference_agent_did = lifecycle.agent_did().to_string();
            let inference_backend_id = lifecycle.backend_id().to_string();
            let result = self
                .run_inference(
                    &request,
                    &doc_id,
                    &built.messages,
                    &wake_notifications,
                    replay,
                    lifecycle,
                    stream_writer,
                    &mut shutdown,
                    &mut interrupt_rx,
                    &request_token,
                    aggregate_token_budget,
                    effective_seed,
                    workspace,
                    request_context_message,
                    resume_from_history,
                    consumed_prompts,
                    folded_prompts,
                    context_calibration,
                )
                .instrument(tracing::info_span!(
                    "request.run_inference",
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    node_did = %request.node_did,
                    agent_did = %inference_agent_did,
                    backend_id = %inference_backend_id,
                    deadline_at = %trace_attrs.deadline_at,
                    has_deadline = trace_attrs.has_deadline,
                    request_hop = trace_attrs.request_hop,
                    selected_skill_count = trace_attrs.selected_skill_count,
                    workspace_cwd_set = trace_attrs.workspace_cwd_set,
                ))
                .await;

            // A failed or timed-out graceful drain has an uncertain durable
            // extent: do not claim the received bytes were saved.
            if result
                .as_ref()
                .err()
                .is_some_and(super::ShutdownDrainFailure::occurs_in)
            {
                return result;
            }

            resolve_inference_outcome(lifecycle, &request, &request_token, &interrupt_rx, result)
                .await
        });

        match capture_scope {
            Some(scope) => {
                gents_inference::rendered_request::scope::scope_request(scope, handled).await
            }
            None => handled.await,
        }
    }
}

async fn resolve_inference_outcome(
    lifecycle: &gents_session::request::lifecycle::RequestLifecycle,
    request: &gents_protocol::row::AgentRequest,
    request_token: &tokio_util::sync::CancellationToken,
    interrupt_rx: &tokio::sync::watch::Receiver<
        Option<gents_session::request::interrupt::InterruptIntent>,
    >,
    result: Result<HandleRequestOutcome>,
) -> Result<HandleRequestOutcome> {
    let interrupt_access = lifecycle.access()?;
    let token_was_cancelled = request_token.is_cancelled();
    let watched_interrupt = { interrupt_rx.borrow().clone() };
    let interrupt_at = if token_was_cancelled {
        if let Some(intent) = watched_interrupt {
            Some(intent.at.to_rfc3339())
        } else {
            gents_session::request::interrupt::fetch_interrupt_requested_at_by_doc_id_with_access(
                &interrupt_access,
                &request.doc_id,
            )
            .await?
        }
    } else if let Some(intent) = watched_interrupt {
        request_token.cancel();
        Some(intent.at.to_rfc3339())
    } else {
        let persisted =
            gents_session::request::interrupt::fetch_interrupt_requested_at_by_doc_id_with_access(
                &interrupt_access,
                &request.doc_id,
            )
            .await?;
        if persisted.is_some() {
            request_token.cancel();
        }
        persisted
    };

    if token_was_cancelled && interrupt_at.is_none() {
        tracing::warn!(
            request_id = %lifecycle.request().request_id,
            "request_token was cancelled without an interrupt latch; \
             treating as failure rather than interrupt"
        );
        return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow::anyhow!(
            "request_token cancelled without interrupt latch"
        )));
    }

    if let Some(interrupt_at) = interrupt_at {
        if !request_token.is_cancelled() {
            request_token.cancel();
        }
        if interrupt_at.trim().is_empty() {
            tracing::warn!(
                request_id = %lifecycle.request().request_id,
                "request_token was cancelled without an interrupt latch; \
                 treating as failure rather than interrupt"
            );
            return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow::anyhow!(
                "request_token cancelled without interrupt latch"
            )));
        }

        let flow_span = tracing::info_span!(
            "interrupt.flow",
            request_id = %lifecycle.request().request_id,
            interrupt_at = %interrupt_at,
        );
        async {
            tokio::time::sleep(CANCELLATION_GRACE_PERIOD).await;
            Ok::<_, anyhow::Error>(())
        }
        .instrument(flow_span)
        .await?;
        return Ok(HandleRequestOutcome::Interrupted(
            RequestInterruptionSource::Inference,
        ));
    }

    result
}

fn is_stale_compaction_generation(error: &anyhow::Error) -> bool {
    gents_protocol::error_chain::find_source::<gents_session::session::StaleCompactionGeneration>(
        error.as_ref(),
    )
    .is_some()
}
