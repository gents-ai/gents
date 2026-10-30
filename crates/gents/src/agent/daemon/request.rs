use anyhow::{Context, Result};
use tracing::Instrument;

use super::{AgentDaemon, HandleRequestOutcome, RequestInterruptionSource};
use crate::inference::admission::{self, AdmissionCallContext, CallKind};
use crate::inference::compaction;
use crate::prompt::PromptBuilder;
use crate::runtime_trace::RequestTraceAttrs;
use crate::session;
use gents_loop::loop_stream::{provider_messages, provider_view_tagged, TaggedMessage};

const CANCELLATION_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);

/// Callers must flush received auxiliary output after interruption. The future
/// must not own a recorded host command (task hook, tool call) or a terminal
/// transaction, whose completion has separate drain and fencing obligations;
/// an unrecorded managed process is killed with its group when dropped.
pub(super) async fn prepare_unless_interrupted<T>(
    interrupt_rx: &mut tokio::sync::watch::Receiver<
        Option<crate::request::interrupt::InterruptIntent>,
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
        crate::config_client::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
        crate::inference::rendered_request::scope::flush_received_auxiliary_partial(),
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
    M: crate::inference::llm::rig_compat::ProviderModel,
{
    /// Size the exact first-turn provider request for session-compaction
    /// admission through the owned loop's assembly owner. The loop remains the
    /// sole final legality authority.
    #[allow(clippy::too_many_arguments)]
    async fn estimate_initial_provider_input(
        &self,
        request: &crate::request::watcher::AgentRequest,
        preamble: String,
        history: &[TaggedMessage],
        skill_reminders: &[crate::inference::llm::message::Message],
        request_context_message: Option<&crate::inference::llm::message::Message>,
        folded_prompts: &[crate::agent::loop_stream::FoldedPrompt],
        aggregate_token_budget: Option<crate::agent::loop_stream::AggregateTokenBudget>,
        replay: &gents_loop::loop_stream::LoopReplayInput,
        resume_from_history: bool,
        wake_notifications: &[TaggedMessage],
    ) -> Result<usize> {
        let config = crate::completion_factory::loop_config_for_request(
            &self.agent_config,
            preamble,
            request,
            aggregate_token_budget,
            self.loop_tools.len(),
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
        lifecycle: &mut crate::request::lifecycle::RequestLifecycle,
        stream_writer: &crate::request::streaming::DefraStreamWriter,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        mut interrupt_rx: tokio::sync::watch::Receiver<
            Option<crate::request::interrupt::InterruptIntent>,
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
        let admitted = async {
            if let Some(guard) = &self.root_execution_guard {
                guard
                    .validate(&self.node)
                    .await
                    .context("validating current WorkspaceRoot policy for fresh request")?;
            }
            let resume_from_history =
                session::retry_has_published_input(&self.node, &request).await?;
            let aggregate_token_budget =
                crate::completion_factory::aggregate_token_budget_for_request(
                    self.node.as_ref(),
                    &request,
                )
                .await?;
            let context_calibration = crate::completion_factory::context_calibration_for_session(
                self.node.as_ref(),
                &self.agent_config,
                &request.node_did,
                &request.session_id,
            )
            .await;
            Ok::<_, anyhow::Error>((
                resume_from_history,
                aggregate_token_budget,
                context_calibration,
            ))
        };
        let Some(admitted) = prepare_unless_interrupted(&mut interrupt_rx, admitted).await else {
            return Ok(HandleRequestOutcome::Interrupted(
                RequestInterruptionSource::Claimed,
            ));
        };
        let (resume_from_history, aggregate_token_budget, context_calibration) = admitted?;
        let trace_attrs = RequestTraceAttrs::from_request(&request);
        let agent_name = self.agent_config.agent_id.clone();
        let admission_context = AdmissionCallContext::for_request(
            &request,
            lifecycle.agent_id(),
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
        let capture_context = crate::inference::rendered_request::context_for_claimed_request(
            &request,
            &request_commit_cid,
            self.agent_config.model_name.clone(),
            self.provider_family.clone(),
        );
        let mut capture_scope = crate::inference::rendered_request::scope_from_factory(
            capture_context.clone(),
            self.rendered_request_capture_factory.as_ref(),
        );
        if let Some(scope) = capture_scope.as_mut() {
            let scope = std::sync::Arc::get_mut(scope)
                .context("fresh request capture scope unexpectedly shared")?;
            if let Some(family) = &self.compaction_provider_family {
                scope.set_compaction_provider_family(family.clone());
            }
            scope.set_auxiliary_output_sink(stream_writer.auxiliary_output_sink(
                request.clone(),
                lifecycle.execution_generation()?.to_owned(),
                crate::provider_input::ProviderInputProfile::resolve(
                    self.agent_config.backend_provider_kind,
                    self.agent_config.openai_wire_api,
                ),
            ));
        }
        let handled = admission::scope_request(admission_context, async {
            let response_agent_id = lifecycle.agent_id().to_string();
            lifecycle
                .begin_owned_execution(stream_writer)
                .instrument(tracing::info_span!(
                    "request.begin_response",
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    node_did = %request.node_did,
                    agent_id = %response_agent_id,
                    request_hop = trace_attrs.request_hop,
                ))
                .await?;
            let preparation = async {
                self.spawn_conversation_title_generation(&request);
                let selected_skill_ids = &request.input.selected_skill_ids;
                let skill_reminders = self
                    .prompt_builder
                    .selected_skill_reminders(selected_skill_ids);
                let overlay = crate::workspace::resolve_request_workspace_overlay(
                    &self.node,
                    &request,
                    lifecycle.execution_generation()?,
                    self.agent_config.tools.static_policy().bash.execution_mode
                        == crate::toolset::CommandExecutionMode::ArtifactWrite,
                    self.operator_tool_root.as_deref(),
                )
                .await?;
                let frozen_instruction_manifest =
                    crate::workspace::frozen_instruction_manifest_from_overlay(overlay.as_ref())
                        .map(str::to_owned);
                let request_context_message = super::inference::render_request_context_message(
                    self.node.as_ref(),
                    &self.agent_config,
                    &request,
                    frozen_instruction_manifest.as_deref(),
                )?;
                let prompt = |folded: crate::request::lifecycle::queue::FoldedInput| -> Result<_> {
                    Ok(crate::agent::loop_stream::FoldedPrompt {
                        key: folded.key(),
                        message: folded.message()?,
                    })
                };
                let consumed_prompts =
                    crate::request::lifecycle::queue::load_consumed_folded_inputs(
                        &self.node, &request,
                    )
                    .await?
                    .into_iter()
                    .map(prompt)
                    .collect::<Result<Vec<_>>>()?;
                let selection = if resume_from_history {
                    &[][..]
                } else {
                    lifecycle.folded_selection()
                };
                let mut folded_prompts = consumed_prompts.clone();
                folded_prompts.extend(
                    selection
                        .iter()
                        .cloned()
                        .map(prompt)
                        .collect::<Result<Vec<_>>>()?,
                );
                let workspace = match overlay {
                    Some(overlay) => crate::tools::call::lifecycle::runtime::ToolWorkspaceScope {
                        runtime_home: None,
                        workspace_cwd: Some(overlay.cwd),
                        workspace_root: Some(overlay.root),
                        workspace_authority: Some(overlay.authority),
                        workspace_artifact: overlay.workspace_artifact,
                    },
                    None => crate::tools::call::lifecycle::runtime::ToolWorkspaceScope::cwd_only(
                        crate::workspace::request_workspace_cwd(&request),
                    ),
                };
                let wake_notifications = if resume_from_history {
                    Vec::new()
                } else {
                    super::wake_input::load(
                        &self.node,
                        &request,
                        lifecycle.background_completion_input_through_sequence(),
                    )
                    .await?
                };
                let (mut built, replay) = async {
                for assembly_attempt in 0..=1 {
                    let background_cutoff =
                        lifecycle.background_completion_input_through_sequence();
                    let compaction_state =
                        session::load_prompt_compaction_state(
                            &self.node,
                            &request.session_id,
                            &request.node_did,
                            request.requester_did.as_deref(),
                            background_cutoff,
                        )
                            .instrument(tracing::info_span!(
                                "request.load_prompt_compaction_state",
                                request_id = %request.request_id,
                                session_id = %request.session_id,
                                agent_id = %agent_name,
                                compaction_entry_count = tracing::field::Empty,
                                compacted_message_count = tracing::field::Empty,
                                summary_count = tracing::field::Empty,
                            ))
                            .await?;
                    let prior_cursor = compaction_state.compacted_through_sequence;
                    let total_compacted_messages = compaction_state.total_messages_compacted;
                    let compaction_generation = compaction_state.generation.clone();
                    let compaction_generation_is_latest = compaction_state.is_latest_generation;
                    let provider_profile = crate::provider_input::ProviderInputProfile::resolve(
                        self.agent_config.backend_provider_kind,
                        self.agent_config.openai_wire_api,
                    );
                    let sequenced_history = session::load_sequenced_history_for_request(
                        &self.node,
                        &request,
                        background_cutoff,
                        prior_cursor,
                        provider_profile,
                    )
                    .instrument(tracing::info_span!(
                        "request.load_active_history",
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        agent_id = %agent_name,
                        compacted_through_sequence = prior_cursor.map(i64::from),
                        compacted_provider_messages_skipped = total_compacted_messages as i64,
                        cursor_hit = prior_cursor.is_some(),
                        history_message_count = tracing::field::Empty,
                    ))
                    .await?;
                    let durable_history = sequenced_history
                        .iter()
                        .map(|row| row.message.clone())
                        .collect::<Vec<_>>();
                    let canonical_prefix_is_stable =
                        compaction::session_prefix_is_stable(&durable_history);
                    let file_activity = compaction::history::extract_file_activity(&durable_history);
                    if !file_activity.is_empty() {
                        tracing::debug!(
                            agent_id = %self.agent_config.agent_id,
                            session_id = %request.session_id,
                            files_read = ?file_activity.files_read,
                            files_modified = ?file_activity.files_modified,
                            "files referenced in stripped history"
                        );
                    }
                    let replay = crate::provider_input::replay::owned_replay_input(
                        self.node.clone(), request.clone(), request_commit_cid.clone(),
                        gents_protocol::rendered_request::CaptureScopeKind::Inference,
                        self.replay_issuer.clone(), provider_profile,
                    );
                    let tagged = crate::provider_input::replay::tag_canonical_history(
                        &sequenced_history,
                    );
                    let provider_history = provider_view_tagged(tagged)?;

                    let prior_provider_prefix = 0;
                    let mut history = provider_history;
                    let mut summaries = compaction_state
                        .summaries
                        .into_iter()
                        .map(compaction::bounded_summary)
                        .collect::<Vec<_>>();

                    let mut built = self
                        .prompt_builder
                        .build(&history, &summaries)
                        .instrument(tracing::info_span!(
                            "request.build_prompt",
                            request_id = %request.request_id,
                            session_id = %request.session_id,
                            agent_id = %agent_name,
                            history_messages = history.len(),
                            summary_count = summaries.len(),
                        ))
                        .await?;
                    let complete_input_tokens = self
                        .estimate_initial_provider_input(
                            &request,
                            built.preamble.clone(),
                            &built.messages,
                            &skill_reminders,
                            request_context_message.as_ref(),
                            &folded_prompts,
                            aggregate_token_budget.clone(),
                            &replay,
                            resume_from_history,
                            &wake_notifications,
                        )
                        .await?;
                    let provider_budget = crate::provider_input::budget::compaction_input_budget(
                        self.agent_config.context_window,
                        self.agent_config.compaction_threshold(),
                        crate::provider_input::budget::configured_output_ceiling(
                            crate::completion_factory::effective_max_tokens(
                                self.agent_config.max_output_tokens,
                                self.agent_config.sampling.max_tokens,
                            ),
                        ),
                    );
                    let context = context_calibration.context(complete_input_tokens);
                    let reduction_admission =
                        compaction::ReductionAdmission::for_context(context, provider_budget);
                    let over_threshold = reduction_admission.is_some();
                    let may_reduce = if over_threshold {
                        let gate_open = canonical_prefix_is_stable;
                        if !gate_open {
                            tracing::info!(
                                request_id = %request.request_id,
                                session_id = %request.session_id,
                                agent_id = %agent_name,
                                "compaction skipped: canonical provider prefix is not a stable turn boundary"
                            );
                        }
                        gate_open
                    } else {
                        false
                    };
                    if may_reduce {
                        let admission = reduction_admission
                            .expect("may_reduce is true only for an admitted reduction");
                        let mut options = self.compaction_options_for_request(
                            lifecycle.claimed_deadline_at(),
                            aggregate_token_budget.clone(),
                            effective_seed,
                            context_calibration.clone(),
                        );
                        let projected = provider_view_tagged(history)?;
                        options.max_compacted_prefix_messages =
                            crate::agent::loop_stream::replay_compaction_prefix_bound(
                                &projected, &replay.required,
                            )?;
                        let projected_native = provider_messages(provider_profile, &projected);
                        options.keep_recent_tokens = self.compactor.retention_target(
                            options.keep_recent_tokens,
                            &projected_native,
                            admission,
                            &options,
                            self.agent_config.context_window,
                        )?;
                        let result = admission::scope_call(
                            CallKind::Compaction,
                            1,
                            self.compactor.reduce(
                                projected_native.clone(),
                                self.agent_config.context_window,
                                &options,
                                admission,
                            ),
                        )
                        .await?;

                        // The seeded anchor described the unreduced history.
                        context_calibration.invalidate_anchor();
                        history = if let Some(exact) = result.exact_reduction() {
                            anyhow::ensure!(projected_native == exact.compacted_prefix.iter().chain(exact.retained_suffix).cloned().collect::<Vec<_>>(),
                                "session reduction changed its exact provider source");
                            let (prefix, suffix) = projected.split_at(exact.compacted_prefix.len());
                            let associations = crate::provider_context_reduction::ReplayAssociations::from_tagged_split(
                                replay.required.clone(), prefix, suffix,
                            );
                            let native = crate::agent::loop_stream::association_reduction(
                                provider_profile, &projected, exact,
                            )?;
                            crate::provider_context_reduction::validate_replay_associations(
                                &associations, native.exact().compacted_prefix, native.exact().retained_suffix, &request.doc_id,
                            )?;
                            suffix.to_vec()
                        } else {
                            anyhow::ensure!(projected_native == result.provider_messages()?,
                                "provider repair changed its sourced projection");
                            projected
                        };
                        if let Some(exact) = result.exact_reduction() {
                            let summary = exact.checkpoint;
                            let sequence_rows = sequenced_history
                                .iter()
                                .map(|row| (row.sequence, row.message.clone()))
                                .collect::<Vec<_>>();
                            let compacted_through_sequence =
                                compaction::session_cursor_for_reduction(
                                    provider_profile,
                                    &sequence_rows,
                                    prior_provider_prefix,
                                    exact,
                                )?;
                            if let (true, Some(compacted_through_sequence)) =
                                (compaction_generation_is_latest, compacted_through_sequence)
                            {
                                match session::save_exact_compaction_entry(
                                    &self.node,
                                    session::NewExactSessionCompaction {
                                        session_id: &request.session_id,
                                        node_did: &request.node_did,
                                        requester_did: request.requester_did.as_deref(),
                                        request_id: &request.request_id,
                                        request_doc_id: &request.doc_id,
                                        files_read: &result.files_read,
                                        files_modified: &result.files_modified,
                                        compacted_through_sequence,
                                        original_tokens: result.original_token_estimate,
                                        compacted_tokens: result.compacted_token_estimate,
                                        expected_generation: &compaction_generation,
                                    },
                                    exact,
                                )
                                .await
                                {
                                    Ok(entry) => {
                                        summaries.push(compaction::bounded_summary(entry.summary));
                                    }
                                    Err(error)
                                        if is_stale_compaction_generation(&error)
                                            && assembly_attempt == 0 =>
                                    {
                                        tracing::info!(
                                            request_id = %request.request_id,
                                            session_id = %request.session_id,
                                            "compaction generation advanced during prompt assembly; rebuilding from the winner"
                                        );
                                        continue;
                                    }
                                    Err(error) if is_stale_compaction_generation(&error) => {
                                        tracing::warn!(
                                            request_id = %request.request_id,
                                            session_id = %request.session_id,
                                            "compaction generation advanced twice; using the verified request-local compaction without appending it"
                                        );
                                        summaries.push(compaction::bounded_summary(summary.to_string()));
                                    }
                                    Err(error) => return Err(error),
                                }
                            } else if compacted_through_sequence.is_none() {
                                tracing::warn!(
                                    request_id = %request.request_id,
                                    session_id = %request.session_id,
                                    provider_prefix = prior_provider_prefix
                                        .saturating_add(exact.compacted_prefix.len()),
                                    "using non-durable request-local compaction because no canonical session cursor could be proven"
                                );
                                summaries.push(compaction::bounded_summary(summary.to_string()));
                            } else {
                                tracing::info!(
                                    request_id = %request.request_id,
                                    session_id = %request.session_id,
                                    "using request-local compaction for an older background snapshot"
                                );
                                summaries.push(compaction::bounded_summary(summary.to_string()));
                            }
                        }

                        built = self
                            .prompt_builder
                            .build(&history, &summaries)
                            .instrument(tracing::info_span!(
                                "request.build_prompt",
                                request_id = %request.request_id,
                                session_id = %request.session_id,
                                agent_id = %agent_name,
                                history_messages = history.len(),
                                summary_count = summaries.len(),
                                compacted = true,
                            ))
                            .await?;
                    }

                    return Ok::<_, anyhow::Error>((built, replay));
                }
                unreachable!("bounded prompt assembly retry returns")
            }
            .instrument(tracing::info_span!(
                "request.prepare_prompt",
                request_id = %request.request_id,
                session_id = %request.session_id,
                node_did = %request.node_did,
                agent_id = %agent_name,
                deadline_at = %trace_attrs.deadline_at,
                has_deadline = trace_attrs.has_deadline,
                request_hop = trace_attrs.request_hop,
                selected_skill_count = trace_attrs.selected_skill_count,
                workspace_cwd_set = trace_attrs.workspace_cwd_set,
            ))
            .await?;

                if !skill_reminders.is_empty() {
                    let mut reminders = skill_reminders
                        .into_iter()
                        .map(TaggedMessage::unassociated)
                        .collect::<Vec<_>>();
                    reminders.append(&mut built.messages);
                    built.messages = reminders;
                }

                Ok::<_, anyhow::Error>((
                    workspace,
                    request_context_message,
                    built,
                    replay,
                    consumed_prompts,
                    folded_prompts,
                    wake_notifications,
                ))
            };
            let Some(prepared) = prepare_unless_interrupted(&mut interrupt_rx, preparation).await
            else {
                flush_received_preparation_partial("interrupted request preparation").await?;
                return Ok(HandleRequestOutcome::Interrupted(
                    RequestInterruptionSource::Preparation,
                ));
            };
            let (
                workspace,
                request_context_message,
                built,
                replay,
                consumed_prompts,
                folded_prompts,
                wake_notifications,
            ) = prepared?;

            let doc_id = lifecycle.request().doc_id.clone();

            let inference_agent_id = lifecycle.agent_id().to_string();
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
                    agent_id = %inference_agent_id,
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

            let token_was_cancelled = request_token.is_cancelled();
            let watched_interrupt = { interrupt_rx.borrow().clone() };
            let interrupt_at = if token_was_cancelled {
                if let Some(intent) = watched_interrupt {
                    Some(intent.at.to_rfc3339())
                } else {
                    crate::request::interrupt::fetch_interrupt_requested_at_by_doc_id(
                        &self.node,
                        &request.doc_id,
                    )
                    .await?
                }
            } else if let Some(intent) = watched_interrupt {
                request_token.cancel();
                Some(intent.at.to_rfc3339())
            } else {
                let persisted = crate::request::interrupt::fetch_interrupt_requested_at_by_doc_id(
                    &self.node,
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
        });

        match capture_scope {
            Some(scope) => {
                crate::inference::rendered_request::scope::scope_request(scope, handled).await
            }
            None => handled.await,
        }
    }
}

fn is_stale_compaction_generation(error: &anyhow::Error) -> bool {
    gents_protocol::error_chain::find_source::<crate::session::StaleCompactionGeneration>(
        error.as_ref(),
    )
    .is_some()
}
