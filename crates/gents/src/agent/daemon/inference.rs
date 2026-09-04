use std::future::IntoFuture;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use tracing::Instrument;

use super::{BehaviorDaemon, HandleRequestOutcome};
use crate::admission::{self, CallKind};
use crate::compaction::{ReductionEngine, ReductionOptions};
use crate::config::AgentBehavior;
use crate::hook::DefraSessionHook;
use crate::llm::message::Message;
use crate::streaming::StreamWriter;
use crate::watcher::AgentRequest;

type RequestDeadline = Option<DateTime<Utc>>;

fn terminal_response_has_visible_output(streamed_text: &str, final_text: Option<&str>) -> bool {
    !streamed_text.trim().is_empty() || final_text.is_some_and(|text| !text.trim().is_empty())
}

fn request_deadline_remaining(deadline: RequestDeadline) -> Option<Duration> {
    let deadline = deadline?;
    let now = Utc::now();
    if now >= deadline {
        return Some(Duration::ZERO);
    }
    Some((deadline - now).to_std().unwrap_or(Duration::ZERO))
}

fn request_deadline_error(deadline: RequestDeadline, context: &str) -> anyhow::Error {
    match deadline {
        Some(deadline) => anyhow!(
            "request deadline exceeded while {}; deadline={}",
            context,
            deadline.to_rfc3339()
        ),
        None => anyhow!("request deadline exceeded while {}", context),
    }
}

fn ensure_request_deadline_open(deadline: RequestDeadline, context: &str) -> Result<()> {
    if request_deadline_remaining(deadline).is_some_and(|remaining| remaining.is_zero()) {
        return Err(request_deadline_error(deadline, context));
    }
    Ok(())
}

pub(super) fn render_request_context_message(
    node: &defra_node::EmbeddedNode,
    behavior: &AgentBehavior,
    request: &AgentRequest,
    frozen_instruction_manifest: Option<&str>,
) -> Result<Option<Message>> {
    let template_body = match behavior.request_context_template.as_deref() {
        Some(template) if !template.trim().is_empty() => {
            let mut ctx = serde_json::Map::new();
            ctx.insert(
                "now".to_string(),
                serde_json::json!(Utc::now().to_rfc3339()),
            );
            if template.contains("collection_summary") {
                ctx.insert(
                    "collection_summary".to_string(),
                    serde_json::json!(crate::template::collection_summary(node)?),
                );
            }

            let rendered = crate::template::render_request_context_template(
                template,
                serde_json::json!({
                    "node_did": behavior.agent_did(),
                    "behavior_id": behavior.behavior_id.as_str(),
                }),
                serde_json::Value::Object(ctx),
                &crate::template::catalog::default_catalog(),
            )
            .map_err(|error| anyhow!("request_context_template render failed: {error}"))?;
            tracing::debug!(
                request_id = %request.request_id,
                behavior_id = %behavior.behavior_id,
                "rendered request context template"
            );
            Some(rendered)
        }
        _ => None,
    };
    Ok(assemble_request_context_message(
        template_body,
        frozen_instruction_manifest,
        crate::workspace::request_workspace_cwd(request).as_deref(),
        behavior.tools.host_tools().read_root(),
    ))
}

fn assemble_request_context_message(
    template_body: Option<String>,
    frozen_instruction_manifest: Option<&str>,
    live_cwd: Option<&std::path::Path>,
    live_tool_root: Option<&std::path::Path>,
) -> Option<Message> {
    // Bound requests keep frozen base_sha provenance; unbound walks live cwd→tool-root.
    let instruction_body = crate::workspace::instruction_body_for_request(
        frozen_instruction_manifest,
        live_cwd,
        live_tool_root,
    );
    match (template_body, instruction_body) {
        (None, None) => None,
        (template, instructions) => {
            let mut body = String::new();
            if let Some(template) = template {
                body.push_str(&template);
            }
            if let Some(instructions) = instructions {
                if !body.is_empty() {
                    body.push_str("\n\n");
                }
                body.push_str(&instructions);
            }
            Some(Message::user(format!("<context>\n{body}\n</context>")))
        }
    }
}

async fn await_with_request_deadline<F, T>(
    deadline: RequestDeadline,
    future: F,
    context: &str,
) -> Result<T>
where
    F: IntoFuture<Output = T>,
{
    let future = future.into_future();
    match request_deadline_remaining(deadline) {
        None => Ok(future.await),
        Some(remaining) if remaining.is_zero() => Err(request_deadline_error(deadline, context)),
        Some(remaining) => tokio::time::timeout(remaining, future)
            .await
            .map_err(|_| request_deadline_error(deadline, context)),
    }
}

impl<M: rig::completion::CompletionModel + 'static> BehaviorDaemon<M> {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_inference(
        &mut self,
        request: &crate::watcher::AgentRequest,
        doc_id: &str,
        history: &[crate::llm::message::Message],
        lifecycle: &mut crate::lifecycle::RequestLifecycle,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
        interrupt_rx: &mut tokio::sync::watch::Receiver<Option<crate::interrupt::InterruptIntent>>,
        request_token: &tokio_util::sync::CancellationToken,
        aggregate_token_budget: Option<crate::agent::loop_stream::AggregateTokenBudget>,
        effective_seed: Option<i64>,
        workspace: crate::tool_call_lifecycle::runtime::ToolWorkspaceScope,
        request_context_message: Option<crate::llm::message::Message>,
    ) -> Result<HandleRequestOutcome> {
        let request_deadline = lifecycle.claimed_deadline_at();
        let trigger_context = crate::lifecycle::TriggerExecutionContext::parse(
            request.caused_by_trigger_context.as_deref(),
        )?;
        let trigger_correlation = request.caused_by_correlation.clone();
        let deadline_at = request
            .deadline
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("")
            .to_string();
        let has_deadline = !deadline_at.is_empty();
        let workspace_cwd_set = workspace.workspace_cwd.is_some();

        ensure_request_deadline_open(request_deadline, "starting inference")?;
        if *shutdown.borrow() {
            return Err(anyhow!("shutdown requested during inference"));
        }
        if interrupt_rx.borrow().is_some() {
            request_token.cancel();
            return Err(anyhow!("request interrupted during inference"));
        }

        let attempt_index = 1_i64;
        let request_id = request.request_id.clone();
        let session_id = request.session_id.clone();
        let behavior_id = self.behavior.behavior_id.clone();
        let backend_id = lifecycle.backend_id().to_string();
        let model_name = self.behavior.model_name.clone();
        // The rendered-request capture scope is installed by `handle_request`,
        // outside every completion loop the request contains — including the
        // pre-request compaction summarizer, which runs before this function is
        // ever called. Do not install one here: a second scope would restart
        // the per-kind label sequence and hand the inference loop a label the
        // summarizer's scope had already used.
        let inference = Box::pin(async {
                let hook = DefraSessionHook::resume_with_identity_policy(
                    self.node.clone(),
                    &request.session_id,
                    &self.behavior.behavior_id,
                    self.behavior.agent_did(),
                    self.hook_failure_policy,
                )
                .await?
                .with_background_tool_registry(self.background_tool_registry.clone())
                .with_background_execution_registry(self.background_execution_registry.clone())
                .with_operator_tool_root(self.operator_tool_root.clone())
                .with_goal_tool_authority(
                    self.behavior.tools.goal_tools_requested(),
                    self.behavior.tools.goal_creation_requested(),
                );
                hook.set_active_request_binding(
                    Some(request.request_id.clone()),
                    Some(request.doc_id.clone()),
                    request.requester_did.clone(),
                )
                .await;
                hook.set_request_deadline_at(request_deadline).await;
                hook.set_approval_required_tools(self.approval_required_tools.as_ref().clone())
                    .await;
                let persistence_hook = hook.clone();

                let model = (*self.model).clone();
                let mut loop_config = crate::completion_factory::loop_config_for_request(
                    &self.behavior,
                    self.preamble.clone(),
                    request,
                    aggregate_token_budget.clone(),
                    self.loop_tools.len(),
                )?;
                loop_config.deadline = request_deadline;
                let active_obligations = crate::agent::output_obligation::active_for_request(
                    self.output_obligations.as_ref(),
                    request.has_automated_trigger_lineage(),
                );
                if !active_obligations.is_empty() {
                    loop_config.output_obligation_gate = Some(
                        crate::agent::output_obligation::OutputObligationGate::new(
                            self.node.clone(),
                            request.doc_id.clone(),
                            active_obligations,
                        ),
                    );
                }
                let turn_compactor = self.compactor.clone();
                let turn_context_window = self.behavior.context_window;
                let turn_compaction_options = self.compaction_options_for_request(
                    request_deadline,
                    aggregate_token_budget,
                    effective_seed,
                );
                let turn_node = self.node.clone();
                let turn_request = request.clone();
                let turn_request_commit_cid = lifecycle
                    .request_commit_cid()
                    .context("claimed request has no exact commit CID for per-turn reduction")?
                    .to_string();
                let turn_compactor_callback = move |
                    compaction_request: crate::agent::loop_stream::TurnCompactionRequest,
                | -> std::pin::Pin<
                    Box<
                        dyn std::future::Future<
                                Output = anyhow::Result<
                                    crate::agent::loop_stream::TurnCompactionOutcome,
                                >,
                            > + Send,
                    >,
                > {
                    let compactor = turn_compactor.clone();
                    let mut options: ReductionOptions = turn_compaction_options.clone();
                    let node = turn_node.clone();
                    let request = turn_request.clone();
                    let request_commit_cid = turn_request_commit_cid.clone();
                    Box::pin(async move {
                        options.keep_recent_tokens = compactor.retention_target(
                            options.keep_recent_tokens,
                            &compaction_request.messages,
                            compaction_request.admission,
                        )?;
                        if crate::session::session_has_other_live_response(
                            node.as_ref(),
                            &request.session_id,
                            Some(&request.request_id),
                        )
                        .await?
                        {
                            anyhow::bail!(
                                "per-turn compaction refused while another response in the \
                                 session is streaming"
                            );
                        }
                        if !crate::compaction::has_unique_call_ids(&compaction_request.messages) {
                            anyhow::bail!(
                                "per-turn compaction refused because tool-call ids are not unique"
                            );
                        }
                        let source_boundary =
                            crate::provider_context_reduction::capture_source_boundary(
                                node.as_ref(),
                                &request.session_id,
                                &request.doc_id,
                                &request_commit_cid,
                            )
                            .await?;
                        let (result, producer_join) = admission::scope_call_with_join(
                            CallKind::Compaction,
                            1,
                            compactor.reduce(
                                compaction_request.messages,
                                turn_context_window,
                                &options,
                                compaction_request.admission,
                            ),
                        )
                        .await;
                        let result = result?;
                        let producer_call = producer_join
                            .filter(|join| matches!(join.call_kind, CallKind::Compaction))
                            .map(|join| crate::provider_context_reduction::ProducerCallRef {
                                call_id: join.call_id,
                                call_seq: join.call_seq,
                            });
                        let Some(exact) = result.exact_reduction() else {
                            if result.cannot_fit() {
                                return Ok(
                                    crate::agent::loop_stream::TurnCompactionOutcome::CannotFit,
                                );
                            }
                            return Ok(
                                crate::agent::loop_stream::TurnCompactionOutcome::ProviderViewRepaired {
                                    messages: result.provider_messages()?.to_vec(),
                                },
                            );
                        };
                        let reduction_index = compaction_request.prior_reduction_keys.len() + 1;
                        let (row, provider_messages) =
                            crate::provider_context_reduction::persist_exact(
                            node.as_ref(),
                            crate::provider_context_reduction::NewExactProviderContextReduction {
                                agent_did: &request.agent_did,
                                requester_did: request.requester_did.as_deref(),
                                session_id: &request.session_id,
                                request_id: &request.request_id,
                                request_doc_id: &request.doc_id,
                                request_commit_cid: &request_commit_cid,
                                reduction_index,
                                turn_index: compaction_request.turn_index,
                                parent_reduction_key: compaction_request
                                    .prior_reduction_keys
                                    .last()
                                    .map(String::as_str),
                                producer_call: producer_call.as_ref(),
                                source_boundary: &source_boundary,
                                original_tokens: result.original_token_estimate,
                                compacted_tokens: result.compacted_token_estimate,
                            },
                            exact,
                        )
                        .await?;
                        Ok(crate::agent::loop_stream::TurnCompactionOutcome::Reduced {
                            messages: provider_messages,
                            reduction_key: row.reduction_key,
                        })
                    })
                };
                loop_config.turn_compactor =
                    Some(std::sync::Arc::new(turn_compactor_callback));
                loop_config.context_message = request_context_message.clone();
                let restored = crate::provider_context_reduction::load_unconsumed_for_request(
                    self.node.as_ref(),
                    &request.doc_id,
                )
                .await?;
                let (loop_history, loop_prompt) = if let Some((row, lineage_keys)) = restored {
                    let mut messages = row.checkpoint_messages()?;
                    let prompt = messages.pop().context(
                        "durable provider-context checkpoint has no current prompt",
                    )?;
                    loop_config.context_message = None;
                    loop_config.active_reduction_keys = row.active_reduction_keys();
                    loop_config.reduction_chain_keys = lineage_keys;
                    loop_config.initial_turn_index = usize::try_from(row.turn_index)
                        .context("durable provider-context checkpoint has invalid turn index")?;
                    tracing::info!(
                        request_id = %request.request_id,
                        reduction_key = %row.reduction_key,
                        reduction_index = row.reduction_index,
                        "restored unconsumed durable provider-context checkpoint"
                    );
                    (messages, prompt)
                } else {
                    loop_config.reduction_chain_keys =
                        crate::provider_context_reduction::load_for_request(
                            self.node.as_ref(),
                            &request.doc_id,
                        )
                        .await?
                        .into_iter()
                        .map(|row| row.reduction_key)
                        .collect();
                    (
                        history.to_vec(),
                        crate::llm::message::Message::user(request.content.clone()),
                    )
                };
                let loop_tools = self.loop_tools.clone();
                let inference_token = request_token.child_token();
                let inference_token_for_start = inference_token.clone();
                let terminal_failure_reason = admission::terminal_failure_reason_observer();
                let hook_for_start_interrupt = persistence_hook.clone();
                let mut stream = admission::scope_call_with_token_and_failure_reason(
                    CallKind::Inference,
                    attempt_index,
                    inference_token.clone(),
                    terminal_failure_reason.clone(),
                    async {
                        tokio::select! {
                            biased;
                            _ = shutdown.changed() => {
                                Err(anyhow!("shutdown requested before inference stream started"))
                            }
                            _ = interrupt_rx.changed() => {
                                request_token.cancel();
                                inference_token_for_start.cancel();
                                if let Err(error) = hook_for_start_interrupt.cancel_in_flight_tool_calls().await {
                                    tracing::warn!(
                                        request_id = %request_id,
                                        session_id = %session_id,
                                        error = %error,
                                        "failed to cancel in-flight tool calls before inference stream started"
                                    );
                                }
                                Err(anyhow!("request interrupted during inference"))
                            }
                            stream = std::future::ready(Box::pin(crate::agent::loop_stream::run_loop_stream(
                                model,
                                Some(hook),
                                loop_prompt,
                                loop_history,
                                loop_tools,
                                loop_config,
                            ))) => Ok(stream)
                        }
                    },
                )
                .await?;

                admission::scope_call_with_token_and_failure_reason(
                    CallKind::Inference,
                    attempt_index,
                    inference_token.clone(),
                    terminal_failure_reason.clone(),
                    async {
                        let mut processor = crate::agent::stream_processor::StreamProcessor::new(
                            &persistence_hook,
                            &self.stream_writer,
                            lifecycle,
                            doc_id,
                        );
                        let mut lease_poll = tokio::time::interval(Duration::from_secs(1));
                        let mut stream_error = None;

                        loop {
                            let item = match tokio::select! {
                                biased;
                                _ = lease_poll.tick() => {
                                    if let Err(error) = processor.validate_execution().await {
                                        let reason = format!("request execution lease: {error:#}");
                                        admission::set_terminal_failure_reason(
                                            &terminal_failure_reason,
                                            reason.clone(),
                                        );
                                        if let Err(tool_error) = persistence_hook
                                            .fail_in_flight_tool_calls(
                                                &reason,
                                                crate::tool_call_lifecycle::FailureClass::External,
                                            )
                                            .await
                                        {
                                            tracing::warn!(
                                                %request_id,
                                                error = %tool_error,
                                                "failed to close in-flight tool calls after losing execution lease"
                                            );
                                        }
                                        return Err(error);
                                    }
                                    continue;
                                }
                                _ = shutdown.changed() => {
                                    return Err(anyhow!("shutdown requested during inference stream"));
                                }
                                _ = interrupt_rx.changed() => {
                                    request_token.cancel();
                                    inference_token.cancel();
                                    if let Err(error) =
                                        persistence_hook.cancel_in_flight_tool_calls().await
                                    {
                                        tracing::warn!(
                                            request_id = %request_id,
                                            session_id = %session_id,
                                            error = %error,
                                            "failed to cancel in-flight tool calls during request interrupt"
                                        );
                                    }
                                    if let Err(error) = processor
                                        .persist_partial_turn("persist interrupted assistant turn")
                                        .await
                                    {
                                        tracing::warn!(
                                            request_id = %request_id,
                                            session_id = %session_id,
                                            error = %error,
                                            "failed to persist interrupted assistant turn before terminal transition"
                                        );
                                    }
                                    if let Err(error) = persistence_hook
                                        .backfill_completed_tool_results()
                                        .await
                                    {
                                        tracing::warn!(
                                            request_id = %request_id,
                                            session_id = %session_id,
                                            error = %error,
                                            "failed to backfill completed tool-result messages on interrupt"
                                        );
                                    }
                                    return Err(anyhow!("request interrupted during inference"));
                                }
                                result = await_with_request_deadline(
                                    request_deadline,
                                    crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
                                        request_deadline,
                                        request_token.clone(),
                                        None,
                                        None,
                                        Some(session_id.clone()),
                                        trigger_correlation.clone(),
                                        trigger_context.source_fields.clone(),
                                        false,
                                        stream.next(),
                                    ),
                                    "waiting for inference stream item",
                                ) => {
                                    match result {
                                        Ok(item) => item,
                                        Err(error) => {
                                            if let Err(sweep_error) =
                                                persistence_hook.timeout_expired_tool_calls().await
                                            {
                                                tracing::warn!(
                                                    request_id = %request_id,
                                                    session_id = %session_id,
                                                    error = %sweep_error,
                                                    "failed to sweep expired in-flight tool calls after request deadline"
                                                );
                                            }
                                            return Err(error);
                                        }
                                    }
                                }
                            } {
                                Some(item) => item,
                                None => break,
                            };
                            match processor.process_item(item).await {
                                Ok(crate::agent::stream_processor::StreamAction::Continue) => {}
                                Ok(crate::agent::stream_processor::StreamAction::Done) => break,
                                Ok(crate::agent::stream_processor::StreamAction::Error(error)) => {
                                    stream_error = Some(error);
                                    break;
                                }
                                Err(error) => return Err(error),
                            }
                        }

                        if let Some(error) = stream_error {
                            let _ = processor
                                .persist_partial_turn("persist errored assistant turn")
                                .await?;
                            if let Err(error) = persistence_hook
                                .backfill_completed_tool_results()
                                .await
                            {
                                tracing::warn!(
                                    request_id = %request_id,
                                    session_id = %session_id,
                                    error = %error,
                                    "failed to backfill completed tool-result messages after stream error"
                                );
                            }

                            let error_reason = format!("agent stream failed: {}", error);
                            return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
                                error_reason
                            )));
                        }

                        if processor.final_text.is_none() {
                            return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
                                "provider stream ended without an explicit terminal response"
                            )));
                        }

                        let mut streamed_text = std::mem::take(&mut processor.streamed_text);
                        let final_text = processor.final_text.take();

                        if let Some(text) = final_text.as_deref() {
                            if streamed_text.is_empty() {
                                let _ = self.stream_writer.write_tokens(doc_id, text).await?;
                                streamed_text.push_str(text);
                            } else if let Some(remainder) = text.strip_prefix(&streamed_text) {
                                if !remainder.is_empty() {
                                    let _ =
                                        self.stream_writer.write_tokens(doc_id, remainder).await?;
                                    streamed_text.push_str(remainder);
                                }
                            }
                        }

                        if !terminal_response_has_visible_output(
                            &streamed_text,
                            final_text.as_deref(),
                        ) {
                            let error_reason =
                                "agent stream completed without producing any visible response content";
                            return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
                                error_reason
                            )));
                        }

                        ensure_request_deadline_open(
                            request_deadline,
                            "finalizing inference response",
                        )?;
                        Ok(HandleRequestOutcome::Completed)
                    },
                )
                .await
            }
            .instrument(tracing::info_span!(
                "inference.attempt",
                request_id = %request_id,
                session_id = %session_id,
                agent_did = %request.agent_did,
                behavior_id = %behavior_id,
                backend_id = %backend_id,
                model_name = %model_name,
                deadline_at = %deadline_at,
                has_deadline,
                subagent_depth = request.subagent_depth,
                is_subagent = request.subagent_depth > 0
                    || request.caused_by_parent_request_id.is_some()
                    || request.caused_by_parent_tool_call_id.is_some(),
                workspace_cwd_set,
                attempt = attempt_index,
                retry_attempt = false,
            )));

        // The capture scope installed by `handle_request` spans both the
        // stream's construction and its drain loop: the SSE transports connect
        // lazily on first poll, so the HTTP send that the capturing transport
        // intercepts usually happens during polling.
        let outcome = crate::tool_call_lifecycle::runtime::scope_tool_request_identity(
            request.requester_did.clone(),
            Some(request.agent_did.clone()),
            Some(behavior_id.to_string()),
            Some(request.request_id.clone()),
            crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_workspace_overlay(
                request_deadline,
                request_token.clone(),
                workspace,
                None,
                Some(session_id.clone()),
                trigger_correlation.clone(),
                trigger_context.source_fields.clone(),
                false,
                inference,
            ),
        )
        .await?;

        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        assemble_request_context_message, await_with_request_deadline,
        ensure_request_deadline_open, request_deadline_remaining,
        terminal_response_has_visible_output, BehaviorDaemon,
    };
    use crate::agent::completion_retry::CompletionRetryProfileFields;
    use crate::agent::runtime::StartupBarrier;
    use crate::backend_provider::BackendProviderKind;
    use crate::compaction::CompactionStrategy;
    use crate::config::{AgentBehavior, SamplingConfig};
    use crate::hook::{BackgroundExecutionRegistry, BackgroundToolRegistry, FailurePolicy};
    use crate::identity::{AgentIdentity, AgentPrincipal, KeyIdentity};
    use crate::llm::tool::ToolDyn;
    use crate::prompt::LayeredPromptBuilder;
    use crate::tool_surface::BehaviorToolConfig;
    use crate::watcher::AgentRequest;
    use futures::stream;
    use rig::completion::{
        CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
    };
    use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;

    #[derive(Clone)]
    struct RoutedReplyModel;

    #[allow(refining_impl_trait)]
    impl CompletionModel for RoutedReplyModel {
        type Response = ();
        type StreamingResponse = ();
        type Client = ();

        fn make(_: &Self::Client, _: impl Into<String>) -> Self {
            Self
        }

        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            Err(CompletionError::ProviderError(
                "completion is unused in daemon lineage test".to_string(),
            ))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
            let items = vec![
                Ok(RawStreamingChoice::Message("routed reply".to_string())),
                Ok(RawStreamingChoice::FinalResponse(())),
            ];
            let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::iter(items));
            Ok(StreamingCompletionResponse::stream(inner))
        }
    }

    #[derive(Clone)]
    struct CountingReplyModel(Arc<AtomicUsize>);

    #[allow(refining_impl_trait)]
    impl CompletionModel for CountingReplyModel {
        type Response = ();
        type StreamingResponse = ();
        type Client = ();
        fn make(_: &Self::Client, _: impl Into<String>) -> Self {
            Self(Arc::new(AtomicUsize::new(0)))
        }
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse<()>, CompletionError> {
            Err(CompletionError::ProviderError("unused".into()))
        }
        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::iter(vec![
                Ok(RawStreamingChoice::Message("admitted reply".to_string())),
                Ok(RawStreamingChoice::FinalResponse(())),
            ]));
            Ok(StreamingCompletionResponse::stream(inner))
        }
    }

    fn test_behavior() -> Arc<AgentBehavior> {
        let identity: Arc<dyn AgentIdentity> = Arc::new(
            KeyIdentity::load_or_create(
                std::env::temp_dir().join(format!("daemon-lineage-{}.key", uuid::Uuid::new_v4())),
                None,
            )
            .expect("test identity"),
        );
        let principal = Arc::new(AgentPrincipal {
            agent_did: identity.did().to_string(),
            identity,
            default_behavior_id: "general".to_string(),
            display_name: None,
            enabled: true,
        });

        Arc::new(AgentBehavior {
            behavior_id: "general".to_string(),
            principal,
            backend_id: Some("backend-general".to_string()),
            backend_provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
            backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
            backend_api_key: None,
            backend_api_key_env_var: None,
            model_name: "scripted".to_string(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            max_turns: 2,
            system_prompt: "system".to_string(),
            request_context_template: None,
            tools: BehaviorToolConfig::meta_only(),
            compaction_threshold: 0.75,
            compaction_strategy: CompactionStrategy::StripThenSummarize,
            stream_batch_ms: 0,
            stream_liveness_timeout: Duration::from_secs(5),
            deadline_duration: Duration::from_secs(30),
            completion_retry: CompletionRetryProfileFields::default(),
            sampling: SamplingConfig::default(),
            skills: Vec::new(),
        })
    }

    async fn create_routed_request(
        node: &defra_node::EmbeddedNode,
        behavior: &AgentBehavior,
        requester_did: &str,
    ) -> AgentRequest {
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            request_id,
            behavior.agent_did(),
            requester_did,
            &behavior.behavior_id,
            session_id,
            "route this reply",
            "interactive",
            created_at,
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                behavior.agent_did(),
            ),
        );
        create.backend_id = Some("backend-general".into());
        create.subagent_depth = 1;
        create.caused_by_parent_request_id = Some("parent-request".into());
        create.caused_by_parent_request_doc_id = Some("parent-request-doc".into());
        create.caused_by_parent_tool_call_id = Some("parent-tool-call".into());
        create.caused_by_parent_tool_call_doc_id = Some("parent-tool-call-doc".into());
        crate::sign_agent_request_create(behavior.principal_identity().as_ref(), &mut create)
            .await
            .unwrap();
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(
            !response.has_errors(),
            "create routed AgentRequest failed: {:?}",
            response.errors
        );
        let doc_id = response
            .data
            .as_ref()
            .and_then(|data| data.get("create_AgentRequest"))
            .and_then(|value| value.get("_docID"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        let doc_id = match doc_id {
            Some(doc_id) => doc_id,
            None => {
                let query = format!(
                    r#"{{
                        AgentRequest(
                            filter: {{ request_id: {{ _eq: "{}" }} }},
                            limit: 1
                        ) {{ _docID }}
                    }}"#,
                    crate::graphql::escape_graphql_string(&create.request_id),
                );
                let response = node.execute(&query).await;
                assert!(
                    !response.has_errors(),
                    "query created AgentRequest failed: {:?}",
                    response.errors
                );
                response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(serde_json::Value::as_array)
                    .and_then(|rows| rows.first())
                    .and_then(|row| row.get("_docID"))
                    .and_then(serde_json::Value::as_str)
                    .expect("created request _docID")
                    .to_string()
            }
        };

        crate::request_admission::load_request_for_admission_test(node, &doc_id)
            .await
            .unwrap()
    }

    async fn create_enrollment_daemon_request(
        node: &defra_node::EmbeddedNode,
        behavior: &AgentBehavior,
        member: &dyn AgentIdentity,
        fence: &crate::agent::p2p_reconcile::enrollment_reconcile::EnrollmentAuthorizationFence,
        suffix: &str,
    ) -> AgentRequest {
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            format!("request-{suffix}"),
            behavior.agent_did(),
            member.did(),
            &behavior.behavior_id,
            format!("session-{suffix}"),
            "run enrolled request",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            gents_protocol::request_admission::AgentRequestAdmissionRecord::enrollment(
                member.did(),
                &fence.request_id,
                &fence.request_digest,
                &fence.admin_did,
                fence.authorization_sequence,
                &fence.authorization_expires_at,
            ),
        );
        create.backend_id = behavior.backend_id.clone();
        crate::sign_agent_request_create(member, &mut create)
            .await
            .unwrap();
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(
            !response.has_errors(),
            "create enrollment request: {:?}",
            response.errors
        );
        let query = format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&create.request_id)
        );
        let response = node.execute(&query).await;
        let doc_id = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(|value| value.as_str())
            .expect("enrollment request doc id");
        crate::request_admission::load_request_for_admission_test(node, doc_id)
            .await
            .unwrap()
    }

    fn live_instruction_tree() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("AGENTS.md"), "root-live-instructions\n").unwrap();
        std::fs::write(nested.join("AGENTS.md"), "nested-live-instructions\n").unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let nested = std::fs::canonicalize(&nested).unwrap();
        (tmp, root, nested)
    }

    #[test]
    fn render_request_context_message_uses_frozen_agents_not_live_tree() {
        let (_tmp, root, nested) = live_instruction_tree();
        std::fs::write(root.join("AGENTS.md"), "live-writer-instructions\n").unwrap();
        let manifest = crate::workspace::InstructionManifest::new(
            "abc",
            vec![crate::workspace::InstructionFile::from_bytes(
                "AGENTS.md",
                b"frozen-base-instructions\n",
            )],
        );
        let message = assemble_request_context_message(
            None,
            Some(&manifest.to_json_string()),
            Some(&nested),
            Some(&root),
        )
        .expect("context");
        let encoded = serde_json::to_string(&message).expect("serialize");
        assert!(encoded.contains("frozen-base-instructions"));
        assert!(!encoded.contains("live-writer-instructions"));
        assert!(!encoded.contains("nested-live-instructions"));
        assert!(encoded.contains("<context>"));
    }

    #[test]
    fn bound_empty_manifest_does_not_include_live_agents_md() {
        let (_tmp, root, nested) = live_instruction_tree();
        assert!(
            assemble_request_context_message(None, Some("{}"), Some(&nested), Some(&root))
                .is_none()
        );
        assert!(
            assemble_request_context_message(None, Some(""), Some(&nested), Some(&root)).is_none()
        );
        let live = assemble_request_context_message(None, None, Some(&nested), Some(&root))
            .expect("unbound live");
        let encoded = serde_json::to_string(&live).expect("serialize");
        assert!(encoded.contains("nested-live-instructions"));
    }

    #[test]
    fn unbound_request_includes_live_agents_md() {
        let (_tmp, root, nested) = live_instruction_tree();
        let message = assemble_request_context_message(None, None, Some(&nested), Some(&root))
            .expect("context");
        let encoded = serde_json::to_string(&message).expect("serialize");
        assert!(encoded.contains("root-live-instructions"));
        assert!(encoded.contains("nested-live-instructions"));
        assert!(encoded.contains("<context>"));
        assert!(!encoded.contains("frozen-base-instructions"));
        let root_at = encoded.find("root-live-instructions").unwrap();
        let nested_at = encoded.find("nested-live-instructions").unwrap();
        assert!(root_at < nested_at);
    }

    #[test]
    fn bound_request_keeps_frozen_manifest_when_live_file_changed() {
        let (_tmp, root, nested) = live_instruction_tree();
        std::fs::write(nested.join("AGENTS.md"), "live-writer-instructions\n").unwrap();
        let manifest = crate::workspace::InstructionManifest::new(
            "abc",
            vec![crate::workspace::InstructionFile::from_bytes(
                "AGENTS.md",
                b"frozen-base-instructions\n",
            )],
        );
        let message = assemble_request_context_message(
            None,
            Some(&manifest.to_json_string()),
            Some(&nested),
            Some(&root),
        )
        .expect("context");
        let encoded = serde_json::to_string(&message).expect("serialize");
        assert!(encoded.contains("frozen-base-instructions"));
        assert!(!encoded.contains("live-writer-instructions"));
        assert!(!encoded.contains("nested-live-instructions"));
        assert!(!encoded.contains("root-live-instructions"));
    }

    #[test]
    fn terminal_response_requires_visible_output() {
        assert!(!terminal_response_has_visible_output("", None));
        assert!(!terminal_response_has_visible_output("   ", Some("")));
        assert!(!terminal_response_has_visible_output("", Some("   ")));
        assert!(terminal_response_has_visible_output("hello", None));
        assert!(terminal_response_has_visible_output("", Some("hello")));
    }

    #[test]
    fn request_deadline_remaining_reports_expired_deadline() {
        let deadline = chrono::Utc::now() - chrono::Duration::milliseconds(1);

        assert_eq!(
            request_deadline_remaining(Some(deadline)),
            Some(Duration::ZERO)
        );
        assert!(ensure_request_deadline_open(Some(deadline), "test").is_err());
    }

    #[tokio::test]
    async fn await_with_request_deadline_bounds_waits() {
        let deadline = chrono::Utc::now() + chrono::Duration::milliseconds(10);

        let result = await_with_request_deadline(
            Some(deadline),
            tokio::time::sleep(Duration::from_secs(5)),
            "test wait",
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn await_with_request_deadline_allows_fast_work() {
        let deadline = chrono::Utc::now() + chrono::Duration::seconds(1);

        let result = await_with_request_deadline(Some(deadline), async { 42 }, "test wait")
            .await
            .expect("fast work should finish before deadline");

        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn daemon_request_path_stamps_requester_lineage_on_hook_messages() {
        let data_path =
            std::env::temp_dir().join(format!("daemon-requester-lineage-{}", uuid::Uuid::new_v4()));
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(&data_path)
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("runtime schemas");

        let behavior = test_behavior();
        let requester_did = behavior.agent_did().to_string();
        let request = create_routed_request(node.as_ref(), &behavior, &requester_did).await;
        let prompt_builder = LayeredPromptBuilder::for_behavior(
            &behavior.system_prompt,
            &behavior.behavior_id,
            &[],
            false,
            &[],
        );
        let preamble = prompt_builder.preamble().to_string();
        let loop_tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(Vec::new());
        let runtime_status = crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            behavior.agent_did().to_string(),
        );
        let request_identity = behavior.principal_identity().clone();
        let mut daemon = BehaviorDaemon::new(
            node.clone(),
            behavior,
            Arc::new(RoutedReplyModel),
            preamble,
            loop_tools,
            prompt_builder,
            FailurePolicy::default(),
            None,
            BackgroundToolRegistry::default(),
            BackgroundExecutionRegistry::default(),
            Arc::new(StartupBarrier::ready_for_test()),
            runtime_status,
            1,
            crate::request_admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                crate::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
        );
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        daemon.process_request(request.clone(), shutdown_rx).await;

        let escaped_session_id = crate::graphql::escape_graphql_string(&request.session_id);
        let query = format!(
            r#"{{
                AgentMessage(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}
                ) {{
                    role
                    content
                    requester_did
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "query routed AgentMessage failed: {:?}",
            response.errors
        );
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(serde_json::Value::as_array)
            .expect("AgentMessage rows");
        assert!(
            rows.iter().any(|row| {
                row.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
                    && row
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|content| content.contains("routed reply"))
                    && row.get("requester_did").and_then(serde_json::Value::as_str)
                        == Some(requester_did.as_str())
            }),
            "daemon-persisted assistant message must carry requester lineage; rows={rows:?}"
        );

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(data_path);
    }

    #[tokio::test]
    async fn daemon_final_claim_rechecks_revocation_and_accepts_exact_replacement() {
        let data_path = std::env::temp_dir().join(format!(
            "daemon-enrollment-final-claim-{}",
            uuid::Uuid::new_v4()
        ));
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(&data_path)
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let behavior = test_behavior();
        let member_dir = tempfile::tempdir().unwrap();
        let member: Arc<dyn AgentIdentity> = Arc::new(
            KeyIdentity::load_or_create(member_dir.path().join("member.key"), None).unwrap(),
        );
        let fence = |sequence: u64, request_id: &str| {
            crate::agent::p2p_reconcile::enrollment_reconcile::EnrollmentAuthorizationFence {
                network_id: "network-1".into(),
                request_id: request_id.into(),
                admin_did: "did:key:admin".into(),
                member_did: member.did().to_string(),
                member_peer: "peer-member".into(),
                member_ticket: "ticket-member".into(),
                owner_agent: behavior.agent_did().to_string(),
                request_digest: format!("digest-{sequence}"),
                authorization_sequence: sequence,
                authorization_expires_at: "2099-01-01T00:00:00Z".into(),
            }
        };
        let generation_one = fence(1, "enrollment-1");
        let revoked = create_enrollment_daemon_request(
            node.as_ref(),
            &behavior,
            member.as_ref(),
            &generation_one,
            "revoked",
        )
        .await;
        let (authority, authority_handle) =
            crate::agent::p2p_reconcile::enrollment_reconcile::test_enrollment_authority(Some(
                generation_one,
            ));

        let prompt_builder = LayeredPromptBuilder::for_behavior(
            &behavior.system_prompt,
            &behavior.behavior_id,
            &[],
            false,
            &[],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime_status = crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            behavior.agent_did().to_string(),
        );
        let request_identity = behavior.principal_identity().clone();
        let mut daemon = BehaviorDaemon::new(
            node.clone(),
            behavior.clone(),
            Arc::new(CountingReplyModel(calls.clone())),
            prompt_builder.preamble().to_string(),
            Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            FailurePolicy::default(),
            None,
            BackgroundToolRegistry::default(),
            BackgroundExecutionRegistry::default(),
            Arc::new(StartupBarrier::ready_for_test()),
            runtime_status,
            1,
            crate::request_admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                authority_handle,
            ),
        );
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        authority.replace(None).await;
        daemon
            .process_request(revoked.clone(), shutdown_rx.clone())
            .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "revoked work reached provider"
        );
        let rejected = node.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state claimed_at }} }}"#,
            crate::graphql::escape_graphql_string(&revoked.doc_id))).await;
        let row = rejected
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .unwrap();
        assert_eq!(row["lifecycle_state"], "failed");
        assert!(row["claimed_at"].is_null());

        let generation_two = fence(2, "enrollment-2");
        authority.replace(Some(generation_two.clone())).await;
        let replacement = create_enrollment_daemon_request(
            node.as_ref(),
            &behavior,
            member.as_ref(),
            &generation_two,
            "replacement",
        )
        .await;
        daemon.process_request(replacement, shutdown_rx).await;
        assert!(
            calls.load(Ordering::SeqCst) > 0,
            "exact replacement did not reach provider"
        );

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(data_path);
    }
    include!("execution_lease_regression_tests.rs");
}
