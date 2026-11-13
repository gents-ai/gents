use std::future::IntoFuture;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use tracing::Instrument;

#[cfg(test)]
use super::ShutdownDrainFailure;
use super::{AgentDaemon, HandleRequestOutcome};
use crate::runtime::agent::loop_stream::{LoopReplayInput, TaggedMessage};
use gents_inference::admission::{self, CallKind};
use gents_inference::llm::message::Message;
use gents_inference::resolved::{MaxTurnsProvenance, ResolvedAgent};
use gents_loop::error::LoopFailureCause;
use gents_protocol::row::AgentRequest;

mod drive;
mod prepare;
type RequestDeadline = Option<DateTime<Utc>>;

const RETRY_CONTINUATION: &str = "Continue the failed request from the recorded progress. Use recorded tool outcomes; do not repeat completed actions.";

/// Provider-frontier-only framing for background-completion wakes. Applied
/// before the frontier prompt when nonempty wake notifications follow; the
/// durable wake prompt carries no `status` attribute (the XML notifications
/// do), so it is swapped for that exact prompt only; all else passes verbatim.
const BACKGROUND_WAKE_RESULT_FRAMING: &str = "Review the background execution results above. Their status describes a turn/tool result, not assignment completion. Continue the task if needed.";

/// Only the exact plain wake prompt (no captured context) is reframed; any
/// authored message carrying captures or other content passes through
/// unchanged, preserving its provenance.
fn wake_frontier_message(authored: &Message, wake_notifications: &[TaggedMessage]) -> Message {
    if !wake_notifications.is_empty()
        && *authored
            == Message::user(
                gents_protocol::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
            )
    {
        Message::user(BACKGROUND_WAKE_RESULT_FRAMING)
    } else {
        authored.clone()
    }
}

pub(super) fn request_entry(
    mut history: Vec<TaggedMessage>,
    authored: &Message,
    resume: bool,
    context: Option<&Message>,
    wake_notifications: &[TaggedMessage],
) -> Result<(Vec<TaggedMessage>, TaggedMessage)> {
    if !resume {
        history = super::wake_input::project(history, wake_notifications);
        history.push(TaggedMessage::unassociated(wake_frontier_message(
            authored,
            wake_notifications,
        )));
    } else if let Some(context) = context {
        history.push(TaggedMessage::unassociated(context.clone()));
    } else if history
        .last()
        .is_some_and(|row| matches!(row.message, Message::Assistant { .. }))
    {
        history.push(TaggedMessage::unassociated(Message::user(
            RETRY_CONTINUATION.to_owned(),
        )));
    }
    let prompt = history
        .pop()
        .context("retry has no durable provider frontier")?;
    Ok((history, prompt))
}

fn terminal_response_has_visible_output(streamed_text: &str, final_text: Option<&str>) -> bool {
    !streamed_text.trim().is_empty() || final_text.is_some_and(|text| !text.trim().is_empty())
}

/// Harbor (`scripts/harbor/run_gents.sh`) matches the pinned
/// `agent stream failed: PromptError: MaxTurnError: ` prefix; provenance is
/// only ever appended after rig's unmodified `Display`.
fn stream_failure_reason(
    error_display: &str,
    failure: &LoopFailureCause,
    provenance: MaxTurnsProvenance,
) -> String {
    match failure {
        LoopFailureCause::MaxTurns => format!(
            "agent stream failed: {error_display} ({})",
            provenance.describe()
        ),
        _ => format!("agent stream failed: {error_display}"),
    }
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
    _node: &defra_node::Node,
    tool_config: &gents_tools::surface::AgentToolSurfaceConfig,
    request: &AgentRequest,
    frozen_instruction_manifest: Option<&str>,
) -> Result<Option<Message>> {
    Ok(assemble_request_context_message(
        frozen_instruction_manifest,
        gents_session::workspace::request_workspace_cwd(request).as_deref(),
        tool_config.host_tools().read_root(),
    ))
}

fn assemble_request_context_message(
    frozen_instruction_manifest: Option<&str>,
    live_cwd: Option<&std::path::Path>,
    live_tool_root: Option<&std::path::Path>,
) -> Option<Message> {
    gents_session::workspace::instruction_body_for_request(
        frozen_instruction_manifest,
        live_cwd,
        live_tool_root,
    )
    .map(|body| Message::user(format!("<context>\n{body}\n</context>")))
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

// Construct a branch's potentially large future outside the live stream poll
// frame. The call must remain separate even in optimized builds.
#[inline(never)]
fn box_stream_branch<F, Fut>(branch: F) -> std::pin::Pin<Box<Fut>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future,
{
    Box::pin(branch())
}

impl<M> AgentDaemon<M>
where
    M: gents_inference::llm::rig_compat::ProviderModel,
{
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_inference(
        &mut self,
        request: &gents_protocol::row::AgentRequest,
        doc_id: &str,
        history: &[TaggedMessage],
        wake_notifications: &[TaggedMessage],
        replay: LoopReplayInput,
        lifecycle: &mut gents_session::request::lifecycle::RequestLifecycle,
        stream_writer: &gents_session::request::streaming::DefraStreamWriter,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
        interrupt_rx: &mut tokio::sync::watch::Receiver<
            Option<gents_session::request::interrupt::InterruptIntent>,
        >,
        request_token: &tokio_util::sync::CancellationToken,
        aggregate_token_budget: Option<crate::runtime::agent::loop_stream::AggregateTokenBudget>,
        effective_seed: Option<i64>,
        mut workspace: gents_tools::runtime_context::ToolWorkspaceScope,
        request_context_message: Option<gents_inference::llm::message::Message>,
        resume_from_history: bool,
        consumed_prompts: Vec<crate::runtime::agent::loop_stream::FoldedPrompt>,
        folded_prompts: Vec<crate::runtime::agent::loop_stream::FoldedPrompt>,
        context_calibration: crate::turn::provider_input::budget::ContextCalibration,
    ) -> Result<HandleRequestOutcome> {
        workspace.runtime_home = self.approval_home.clone();
        let request_deadline = lifecycle.claimed_deadline_at();
        let trigger_context = gents_session::request::lifecycle::TriggerExecutionContext::parse(
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
        let agent_did = self.agent_config.agent_did.clone();
        let reviewer = self.workspace_reviewer(
            request,
            aggregate_token_budget.clone(),
            request_token.clone(),
            shutdown.clone(),
        );
        let approval_context = gents_tools::plugin::approval::workspace::for_request(
            self.approval_home.as_deref(),
            &request.node_did,
            request.requester_did.as_deref(),
            &agent_did,
            workspace.workspace_cwd.as_deref(),
        )?
        .map(|context| context.with_reviewer(reviewer.callback.clone()));
        let backend_id = lifecycle.backend_id().to_string();
        let model_name = self.agent_config.model_name.clone();
        let inference_observation = self
            .inference_observations
            .begin(gents_inference::inference_observation::InferenceKey {
                node: request.node_did.clone(),
                session: request.session_id.clone(),
                request: request.request_id.clone(),
                generation: lifecycle.execution_generation()?.to_owned(),
            })
            .map(Arc::new);
        let inference = Box::pin(async {
                let prepared = Box::pin(self.prepare_inference(
                    prepare::InferencePreparation {
                        request,
                        lifecycle: &*lifecycle,
                        request_deadline,
                        inference_observation: &inference_observation,
                        effective_seed,
                        context_calibration: &context_calibration,
                        request_context_message: &request_context_message,
                        resume_from_history,
                        consumed_prompts: &consumed_prompts,
                        folded_prompts: &folded_prompts,
                        history,
                        wake_notifications,
                    },
                    replay,
                    aggregate_token_budget,
                ));
                let Some(prepared) =
                    super::request::prepare_unless_interrupted(interrupt_rx, prepared).await
                else {
                    request_token.cancel();
                    return Err(anyhow!("request interrupted during inference"));
                };
                let prepare::PreparedInference {
                    hook,
                    persistence_hook,
                    model,
                    loop_config,
                    loop_history,
                    loop_prompt,
                    max_turns_provenance,
                } = prepared?;
                let loop_tools = gents_loop::tool::bind_request_tools(
                    self.loop_tools.clone(),
                    lifecycle.claimed_tool_context()?,
                );
                let inference_token = request_token.child_token();
                let inference_token_for_start = inference_token.clone();
                let terminal_failure_reason = admission::terminal_failure_reason_observer();
                let hook_for_start_interrupt = persistence_hook.clone();
                let provider_profile = loop_config.provider_input_counter.profile();
                let stream = admission::scope_call_with_token_and_failure_reason(
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
                            stream = std::future::ready(Box::pin(crate::runtime::agent::loop_stream::run_loop_stream(
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
                    Box::pin(drive::drive_inference_stream(
                        drive::StreamDrive {
                            persistence_hook: &persistence_hook,
                            stream_writer,
                            doc_id,
                            provider_profile,
                            request_token,
                            inference_token: &inference_token,
                            request_id: &request_id,
                            session_id: &session_id,
                            trigger_correlation: &trigger_correlation,
                            trigger_context: &trigger_context,
                            request_deadline,
                            terminal_failure_reason: &terminal_failure_reason,
                            max_turns_provenance,
                        },
                        stream,
                        lifecycle,
                        shutdown,
                        interrupt_rx,
                    )),
                )
                .await
            }
            .instrument(tracing::info_span!(
                "inference.attempt",
                request_id = %request_id,
                session_id = %session_id,
                node_did = %request.node_did,
                agent_did = %agent_did,
                backend_id = %backend_id,
                model_name = %model_name,
                deadline_at = %deadline_at,
                has_deadline,
                request_hop = request.request_hop,
                workspace_cwd_set,
                attempt = attempt_index,
                retry_attempt = false,
            )));

        let outcome = gents_loop::tool_call_lifecycle::runtime::scope_tool_request_identity(
            request.requester_did.clone(),
            Some(request.node_did.clone()),
            Some(agent_did.to_string()),
            Some(request.request_id.clone()),
            gents_tools::runtime_context::scope_request_tool_execution_with_workspace_overlay(
                request_deadline,
                request_token.clone(),
                workspace,
                None,
                Some(session_id.clone()),
                trigger_correlation.clone(),
                trigger_context.source_fields.clone(),
                false,
                gents_tools::plugin::approval::scope_interactive(
                    request.execution_origin.as_deref() == Some("interactive"),
                    gents_tools::plugin::approval::workspace::scope(approval_context, inference),
                ),
            ),
        )
        .await;

        if let Some(error) = reviewer.take_failure() {
            return Err(error);
        }
        let outcome = outcome?;

        Ok(outcome)
    }
}

/// Runtime storage and consistency failures inside the turn compactor; typed
/// compaction failures pass through `?` so the loop can refuse them.
fn compactor_failure(
    error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> crate::runtime::agent::loop_stream::TurnCompactionError {
    crate::runtime::agent::loop_stream::TurnCompactionError::Compactor(error.into())
}

#[cfg(test)]
pub(super) mod tests {
    use super::{stream_failure_reason, AgentDaemon};
    use crate::runtime::agent::daemon::AgentDaemonInit;
    use crate::runtime::agent::runtime::StartupBarrier;
    use crate::turn::prompt::LayeredPromptBuilder;
    use anyhow::Context;
    use futures::stream;
    use gents_inference::backends::provider::BackendProviderKind;
    use gents_inference::resolved::MaxTurnsProvenance;
    use gents_inference::resolved::{ResolvedAgent, SamplingConfig};
    use gents_loop::completion_retry::CompletionRetryProfileFields;
    use gents_loop::error::LoopFailureCause;
    use gents_loop::tool::ToolDyn;
    use gents_protocol::row::AgentRequest;
    use gents_store::identity::{NodeIdentity, RuntimeNode};
    use gents_toolcalls::call::hook::{
        BackgroundExecutionRegistry, BackgroundToolRegistry, FailurePolicy,
    };
    use gents_tools::surface::AgentToolSurfaceConfig;
    use rig::completion::{
        CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
    };
    use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;
    /// rig's own turn-exhaustion `Display`. Its `PromptError: MaxTurnError: `
    /// prefix is pinned by `agent/loop_stream/tests/streaming.rs`; the
    /// parenthesised limit is pinned by
    /// `tests/e2e_background/session_message_turn_limit.rs`.
    const MAX_TURNS_DISPLAY: &str = "PromptError: MaxTurnError: (reached max turn limit: 1000)";
    const PINNED_PREFIX: &str = "agent stream failed: PromptError: MaxTurnError: ";

    #[test]
    fn max_turns_failure_message_reports_default_provenance_and_keeps_pinned_prefix() {
        let reason = stream_failure_reason(
            MAX_TURNS_DISPLAY,
            &LoopFailureCause::MaxTurns,
            MaxTurnsProvenance::Default,
        );
        assert!(
            reason.starts_with(PINNED_PREFIX),
            "must preserve the pinned Harbor/streaming-test prefix: {reason}"
        );
        assert!(
            reason.contains("built-in default"),
            "must name the default provenance: {reason}"
        );
    }

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
            request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
            gents_inference::test_support::capture_scripted_provider_request(&request, "scripted")
                .await?;
            let items = vec![
                Ok(RawStreamingChoice::Message("routed reply".to_string())),
                Ok(RawStreamingChoice::FinalResponse(())),
            ];
            let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::iter(items));
            Ok(StreamingCompletionResponse::stream(inner))
        }
    }

    #[derive(Clone)]
    struct WakeInputModel {
        provider_inputs: Arc<std::sync::Mutex<Vec<String>>>,
        title_calls: Arc<AtomicUsize>,
        title_shape_mismatches: Arc<AtomicUsize>,
        successor: Option<WakeSuccessor>,
    }

    #[derive(Clone)]
    struct WakeSuccessor {
        node: Arc<defra_node::Node>,
        request_id: String,
        did: String,
        signer: gents_store::identity::ManagedAgentSigner,
        runtime: Arc<dyn NodeIdentity>,
    }

    #[allow(refining_impl_trait)]
    impl CompletionModel for WakeInputModel {
        type Response = ();
        type StreamingResponse = ();
        type Client = ();

        fn make(_: &Self::Client, _: impl Into<String>) -> Self {
            unreachable!("the wake test constructs its deterministic model directly")
        }

        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            Err(CompletionError::ProviderError(
                "non-streaming completion is unused".into(),
            ))
        }

        async fn stream(
            &self,
            request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
            gents_inference::test_support::capture_scripted_provider_request(&request, "scripted")
                .await?;
            let history =
                serde_json::to_value(&request.chat_history).expect("serialize provider input");
            let title_preamble = super::super::title::title_generation_preamble();
            let title_shape = request.temperature == Some(0.0) && request.tools.is_empty();
            let title_preamble_present = json_contains_text(&history, &title_preamble);
            let text = match (title_shape, title_preamble_present) {
                (true, true) => {
                    self.title_calls.fetch_add(1, Ordering::SeqCst);
                    "background-completion-review"
                }
                (false, false) => {
                    let first = {
                        let mut inputs = self
                            .provider_inputs
                            .lock()
                            .expect("wake provider input capture");
                        inputs.push(
                            serde_json::to_string(&request.chat_history)
                                .expect("serialize provider input"),
                        );
                        inputs.len() == 1
                    };
                    if first {
                        if let Some(WakeSuccessor {
                            node,
                            request_id,
                            did,
                            signer,
                            runtime,
                        }) = &self.successor
                        {
                            gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
                                node,
                                request_id,
                                "successor native output",
                                signer,
                                runtime.clone(),
                            )
                            .await;
                            assert_eq!(
                                gents_toolcalls::background::completion::settle_running_session_message_rows_with_access(
                                    node, &gents_store::config_client::ConfigAccess::local_with_agent_signer(node.clone(), identity::Did::new(signer.did()).unwrap(), signer.clone()).unwrap(), &gents_automation::goal::session_message::SessionGoals, did
                                )
                                .await
                                .unwrap(),
                                1
                            );
                        }
                    }
                    "wake handled"
                }
                _ => {
                    self.title_shape_mismatches.fetch_add(1, Ordering::SeqCst);
                    return Err(CompletionError::ProviderError(
                        "wake fixture title shape and owner preamble disagree".into(),
                    ));
                }
            };
            let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::iter(vec![
                Ok(RawStreamingChoice::Message(text.to_string())),
                Ok(RawStreamingChoice::FinalResponse(())),
            ]));
            Ok(StreamingCompletionResponse::stream(inner))
        }
    }

    fn json_contains_text(value: &serde_json::Value, expected: &str) -> bool {
        match value {
            serde_json::Value::String(text) => text.contains(expected),
            serde_json::Value::Array(values) => values
                .iter()
                .any(|value| json_contains_text(value, expected)),
            serde_json::Value::Object(fields) => fields
                .values()
                .any(|value| json_contains_text(value, expected)),
            _ => false,
        }
    }

    async fn persisted_request_by_doc_id(
        access: &gents_store::config_client::ConfigAccess,
        doc_id: &str,
    ) -> serde_json::Value {
        let response = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state failure_reason terminal_output }} }}"#,
                gents_store::graphql::escape_graphql_string(doc_id)
            ))
            .await
            .unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        rows[0].clone()
    }

    async fn persisted_request_state(
        access: &gents_store::config_client::ConfigAccess,
        doc_id: &str,
    ) -> gents_protocol::request_lifecycle::RequestLifecycleState {
        let row = persisted_request_by_doc_id(access, doc_id).await;
        gents_protocol::request_lifecycle::RequestLifecycleState::parse(
            row["lifecycle_state"].as_str().unwrap(),
        )
        .unwrap()
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
            request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
            gents_inference::test_support::capture_scripted_provider_request(&request, "scripted")
                .await?;
            self.0.fetch_add(1, Ordering::SeqCst);
            let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::iter(vec![
                Ok(RawStreamingChoice::Message("admitted reply".to_string())),
                Ok(RawStreamingChoice::FinalResponse(())),
            ]));
            Ok(StreamingCompletionResponse::stream(inner))
        }
    }

    fn test_agent_with_binding(
        identity: Arc<dyn NodeIdentity>,
        binding: gents_protocol::agent_identity::AgentIdentityBinding,
    ) -> Arc<ResolvedAgent> {
        let principal = Arc::new(RuntimeNode {
            node_did: identity.did().to_string(),
            identity,
            default_agent_did: binding.agent_did.clone(),
            display_name: None,
            enabled: true,
        });
        let agent_identity =
            gents_store::identity::resolve_agent_identity(principal.identity.as_ref(), &binding)
                .unwrap();
        let backend_id = format!("{}:backend", binding.agent_did);
        let agent_did = binding.agent_did;
        Arc::new(ResolvedAgent {
            configuration: gents_inference::resolved::ResolvedConfiguration::Static,
            profile_id: None,
            agent_did,
            agent_identity,
            node: principal,
            backend_id: Some(backend_id),
            backend_provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
            backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
            claude_platform: None,
            backend_auth: gents_documents::BackendAuth::Unauthenticated,
            model_name: "scripted".to_string(),
            service_tier: None,
            resolved_reasoning_efforts: None,
            context_window: 8_192,
            max_output_tokens: 1_024,
            max_turns: 2,
            max_turns_provenance: gents_inference::resolved::MaxTurnsProvenance::Default,
            system_prompt: "system".to_string(),
            compaction: None,
            compaction_inference: None,
            max_total_tokens: None,
            stream_batch_ms: 0,
            stream_liveness_timeout: Duration::from_secs(5),
            deadline_duration: Duration::from_secs(30),
            provider_idle_timeout: Duration::from_secs(
                gents_inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
            ),
            completion_retry: CompletionRetryProfileFields::default(),
            sampling: SamplingConfig::default(),
            skills: Vec::new(),
            oauth_pool: Vec::new(),
        })
    }

    async fn held_daemon_fixture(
        name: &str,
    ) -> (
        crate::support::TestDb,
        gents_store::identity::KeyIdentity,
        gents_store::config_client::ConfigAccess,
        Arc<ResolvedAgent>,
    ) {
        use gents_store::config_client::NodePermission;
        let (db, user, access, binding) = crate::test_support::root::signed_test_node(name).await;
        let config = test_agent_with_binding(db.node_identity.clone(), binding);
        let admin = gents_store::config_client::ConfigAccess::local_as(
            db.node.clone(),
            db.node_identity.did().parse().unwrap(),
        );
        let agent_did = identity::Did::new(&config.agent_did).unwrap();
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DocumentDelete,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&agent_did, permission)
                .await
                .unwrap();
        }
        (db, user, access, config)
    }

    fn held_daemon_agent_access(
        db: &crate::support::TestDb,
        config: &ResolvedAgent,
    ) -> gents_store::config_client::ConfigAccess {
        gents_store::config_client::ConfigAccess::local_managed_agent(
            db.node.clone(),
            identity::Did::new(&config.agent_did).unwrap(),
            config.agent_identity.as_ref(),
        )
        .unwrap()
    }

    async fn held_daemon_query(
        access: &gents_store::config_client::ConfigAccess,
        document: &str,
    ) -> query::QueryResponse {
        let result = if document.trim_start().starts_with("mutation") {
            access.write("test.daemon_fixture", document).await
        } else {
            access.execute(document).await
        }
        .unwrap();
        serde_json::from_value(result).unwrap()
    }

    async fn create_held_routed_request(
        db: &crate::support::TestDb,
        user: &gents_store::identity::KeyIdentity,
        access: &gents_store::config_client::ConfigAccess,
        agent_config: &ResolvedAgent,
        title: Option<&str>,
    ) -> AgentRequest {
        use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
        use gents_session::request::lifecycle::{RequestIdentity, RequestSigner, RequestSpec};
        let request_id = uuid::Uuid::new_v4().to_string();
        let mut spec = RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.clone(),
                node_did: db.node_identity.did().to_owned(),
                requester_did: Some(user.did().to_owned()),
                agent_did: agent_config.agent_did.clone(),
                session_id: uuid::Uuid::new_v4().to_string(),
                content: "route this reply".into(),
                execution_origin: gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            AgentRequestAdmissionRecord::client(user.did()),
        );
        spec.input.initial_title = title.map(|text| gents_protocol::session::SessionTitle {
            text: text.to_owned(),
            source: gents_protocol::session::SessionTitleSource::Task,
        });
        let create = gents_session::request::lifecycle::build_signed_client_request(
            access,
            spec,
            RequestSigner::Identity(user),
        )
        .await
        .unwrap();
        gents_session::request::lifecycle::submit_client_request(access, &create)
            .await
            .unwrap();
        let escaped = gents_store::graphql::escape_graphql_string(&request_id);
        let response = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{_docID}} }}"#
            ))
            .await
            .unwrap();
        let doc_id = response["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap();
        gents_session::request::admission::load_request_for_admission_test_as(
            &db.node,
            doc_id,
            &agent_config.agent_did,
        )
        .await
        .unwrap()
    }

    async fn create_enrollment_daemon_request(
        db: &crate::support::TestDb,
        member_access: &gents_store::config_client::ConfigAccess,
        agent_config: &ResolvedAgent,
        member: &dyn NodeIdentity,
        fence: &gents_p2p::p2p_reconcile::enrollment_reconcile::EnrollmentAuthorizationFence,
        suffix: &str,
    ) -> AgentRequest {
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            format!("request-{suffix}"),
            agent_config.node_did(),
            member.did(),
            &agent_config.agent_did,
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
        gents_session::request::admission::sign_agent_request_create(member, &mut create)
            .await
            .unwrap();
        let input = gents_store::config_client::client_request_input(&create).unwrap();
        let agent_did = identity::Did::new(&agent_config.agent_did).unwrap();
        let doc_id = member_access
            .transact_write("test.enrollment_request", |txn| {
                let input = &input;
                let agent_did = &agent_did;
                Box::pin(async move {
                    let response = txn.execute_client_signed_create(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &serde_json::json!({"input":input}),
                        gents_protocol::schemas::AGENT_REQUEST,
                    ).await?;
                    let doc_id = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
                    txn.add_actor_relationship("AgentRequest", &doc_id, "working_agent", agent_did).await?;
                    Ok::<_, anyhow::Error>(doc_id)
                })
            })
            .await
            .unwrap();
        gents_session::request::admission::load_request_for_admission_test_as(
            &db.node,
            &doc_id,
            &agent_config.agent_did,
        )
        .await
        .unwrap()
    }

    #[test]
    fn wake_frontier_prompt_framing_scopes() {
        use super::{
            request_entry, wake_frontier_message, TaggedMessage, BACKGROUND_WAKE_RESULT_FRAMING,
        };
        use gents_inference::llm::message::Message;
        use gents_protocol::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT;

        let notification = TaggedMessage::unassociated(Message::user(
            "<tool-completion tool_call_id=\"c1\" status=\"completed\">".to_owned(),
        ));
        let notifications = [notification.clone()];

        for (content, wake_notifications, expected) in [
            (
                BACKGROUND_COMPLETION_WAKE_PROMPT,
                notifications.as_slice(),
                BACKGROUND_WAKE_RESULT_FRAMING,
            ),
            (
                BACKGROUND_COMPLETION_WAKE_PROMPT,
                &[],
                BACKGROUND_COMPLETION_WAKE_PROMPT,
            ),
            (
                "Summarize the repo layout.",
                notifications.as_slice(),
                "Summarize the repo layout.",
            ),
        ] {
            assert_eq!(
                wake_frontier_message(&Message::user(content), wake_notifications),
                Message::user(expected)
            );
        }

        // A wake prompt carrying captured editor context is authored input with
        // provenance; it is never reframed and its document parts survive.
        let captured = Message::User {
            content: vec![
                gents_inference::llm::message::UserContent::text(BACKGROUND_COMPLETION_WAKE_PROMPT),
                gents_inference::llm::message::UserContent::Document(
                    gents_inference::llm::message::Document {
                        data: gents_inference::llm::message::DocumentSourceKind::String(
                            "fn main() {}".into(),
                        ),
                        media_type: Some(gents_inference::llm::message::DocumentMediaType::TXT),
                        additional_params: Some(
                            serde_json::json!({ "captured_context": { "kind": "selection" } }),
                        ),
                    },
                ),
            ],
        };
        assert_eq!(wake_frontier_message(&captured, &notifications), captured);
        let (_, frontier) =
            request_entry(Vec::new(), &captured, false, None, &notifications).unwrap();
        assert_eq!(
            frontier.message, captured,
            "captured provenance reaches provider"
        );

        let (history, frontier) = request_entry(
            Vec::new(),
            &Message::user(BACKGROUND_COMPLETION_WAKE_PROMPT),
            false,
            None,
            &notifications,
        )
        .unwrap();
        assert_eq!(
            frontier.message,
            Message::user(BACKGROUND_WAKE_RESULT_FRAMING)
        );
        assert_eq!(
            history.first().map(|row| row.message.clone()),
            Some(notification.message.clone()),
            "projected notification reaches provider input verbatim"
        );

        // Resume pops the existing final row instead of appending content: the
        // recorded frontier is returned unchanged and the notification row is
        // not re-projected.
        let recorded = TaggedMessage::unassociated(Message::user("recorded frontier".to_owned()));
        let (history, frontier) = request_entry(
            vec![notification.clone(), recorded.clone()],
            &Message::user(BACKGROUND_COMPLETION_WAKE_PROMPT),
            true,
            None,
            &notifications,
        )
        .unwrap();
        assert_eq!(frontier, recorded);
        assert_eq!(history, vec![notification], "resume is byte-identical");
    }

    #[test]
    fn retry_entry_matches_lean_frontier() {
        gents_lean_vocab::families::contracts_retry_entry_frontier::replay(|case| {
            let message = |value: &u64| {
                use gents_inference::llm::message::{
                    AssistantContent, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
                    UserContent,
                };
                let native = match *value {
                    2 => gents_inference::llm::message::Message::Assistant {
                        request_id: None,
                        id: None,
                        content: vec![AssistantContent::ToolCall(ToolCall {
                            raw_arguments: None,
                            id: "call-1".into(),
                            call_id: Some("call-1".into()),
                            signature: None,
                            additional_params: None,
                            function: ToolFunction {
                                name: "read_file".into(),
                                arguments: serde_json::json!({"path":"source.rs"}),
                            },
                        })],
                    },
                    3 => gents_inference::llm::message::Message::User {
                        content: vec![UserContent::ToolResult(ToolResult {
                            id: "call-1".into(),
                            call_id: Some("call-1".into()),
                            content: vec![ToolResultContent::Text(Text {
                                text: "recorded result".into(),
                            })],
                        })],
                    },
                    4 => gents_inference::llm::message::Message::assistant("recorded progress"),
                    10 => gents_inference::llm::message::Message::user(super::RETRY_CONTINUATION),
                    _ => gents_inference::llm::message::Message::user(value.to_string()),
                };
                super::TaggedMessage::unassociated(native)
            };
            let history = case.history.iter().map(message).collect();
            let context = case.context.as_ref().map(|value| message(value).message);
            let (mut actual, prompt) = super::request_entry(
                history,
                &gents_inference::llm::message::Message::user(case.authored.to_string()),
                case.resume,
                context.as_ref(),
                &[],
            )
            .unwrap();
            actual.push(prompt);
            let candidates = case
                .history
                .iter()
                .copied()
                .chain([case.authored, 10])
                .chain(case.context)
                .collect::<Vec<_>>();
            actual
                .iter()
                .map(|row| {
                    candidates
                        .iter()
                        .copied()
                        .find(|value| message(value) == *row)
                        .unwrap_or_else(|| panic!("{case:?}: unmodeled entry row {row:?}"))
                })
                .collect::<Vec<u64>>()
        });
    }

    #[tokio::test]
    async fn daemon_request_path_stamps_requester_lineage_on_hook_messages() {
        use gents_protocol::request_lifecycle::RequestLifecycleState;

        let (db, user, access, agent_config) =
            held_daemon_fixture("daemon-requester-lineage").await;
        let node = db.node.clone();
        let node_did = db.node_identity.did().to_owned();
        let requester_did = user.did().to_owned();
        let request = create_held_routed_request(&db, &user, &access, &agent_config, None).await;
        let prompt_builder = LayeredPromptBuilder::for_agent(
            &agent_config.system_prompt,
            &agent_config.agent_did,
            &[],
            false,
            &[],
        );
        let preamble = prompt_builder.preamble().to_string();
        let loop_tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(Vec::new());
        let runtime_status = crate::runtime::status::RuntimeStatusHandle::new(
            node.clone(),
            agent_config.node_did().to_string(),
        );
        let request_identity = agent_config.node_identity().clone();
        let rendered_request_capture_factory =
            gents_inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                node.clone(),
                agent_config.agent_identity.as_ref(),
            )
            .unwrap();
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: node.clone(),
            tool_config: Arc::new(AgentToolSurfaceConfig::meta_only()),
            agent_config,
            provider_family: None,
            model: Arc::new(RoutedReplyModel),
            preamble,
            loop_tools,
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(rendered_request_capture_factory),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status,
            slot_generation: 1,
            request_admission:
                gents_session::request::admission::AgentRequestAdmissionVerifier::new(
                    node.clone(),
                    request_identity,
                    gents_p2p::p2p_reconcile::enrollment_authority_channel().1,
                ),
        })
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        daemon
            .process_request(request.clone(), shutdown_rx)
            .await
            .unwrap();

        let escaped = gents_store::graphql::escape_graphql_string(&request.doc_id);
        let result = access.execute(&format!(r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{escaped}"}}}}) {{lifecycle_state}} }}"#)).await.unwrap();
        assert_eq!(
            result["data"]["AgentRequest"][0]["lifecycle_state"],
            RequestLifecycleState::Completed.as_str()
        );
        let request_doc_id = gents_store::graphql::escape_graphql_string(&request.doc_id);
        let requester = gents_store::graphql::escape_graphql_string(&requester_did);
        let authored_prompt = access
            .execute(&format!(
                r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }}, requester_did: {{ _eq: "{requester}" }}, role: {{ _eq: "user" }} }}) {{ _docID message_key request_doc_id requester_did role }} }}"#
            ))
            .await.unwrap();
        let authored_rows = authored_prompt["data"]["AgentMessage"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            authored_rows.iter().any(|row| {
                row["request_doc_id"] == request.doc_id
                    && row["requester_did"] == requester_did
                    && row["message_key"]
                        == gents_protocol::output::rows::authored_message_key(
                            &request.doc_id,
                            "prompt",
                        )
            }),
            "completed fresh non-retry request must have its exact durable authored prompt: {authored_rows:?}"
        );

        let escaped_session_id = gents_store::graphql::escape_graphql_string(&request.session_id);
        let query = format!(
            r#"{{
                AgentMessage(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}
                ) {{
                    _docID
                    role
                    requester_did
                }}
            }}"#
        );
        let response = access.execute(&query).await.unwrap();
        let rows = response["data"]["AgentMessage"]
            .as_array()
            .expect("AgentMessage rows");
        let mut found_reply = false;
        for row in rows {
            assert_eq!(
                row.get("requester_did").and_then(serde_json::Value::as_str),
                Some(requester_did.as_str()),
                "every published header must preserve requester lineage"
            );
            let doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .unwrap();
            let (_, message) = gents_session::session::load_canonical_message(
                &access,
                doc_id,
                &node_did,
                Some(&requester_did),
            )
            .await
            .expect("exact published header must reconstruct under its requester");
            found_reply |= row.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
                && serde_json::to_string(&message)
                    .unwrap()
                    .contains("routed reply");
        }
        assert!(
            found_reply,
            "daemon-persisted assistant message must carry requester lineage; rows={rows:?}; request={:?}",
            access.execute(&format!(
                "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ lifecycle_state failure_reason terminal_output }} }}",
                gents_store::graphql::escape_graphql_string(&request.doc_id)
            )).await
        );

        node.shutdown().await.expect("node shutdown");
    }

    #[tokio::test]
    async fn successor_background_wake_moves_notification_after_previous_answer() {
        use gents_protocol::request_lifecycle::RequestLifecycleState;

        let (selected, successor) =
            crate::toolcalls_tests::lifecycle::completion_owner_conformance::selected_background_wake_with_successor(true)
                .await;
        let admission = selected.admission;
        let wake = selected.wake;
        let notification_text = selected.notification_text;
        let node = admission.node.clone();
        assert_eq!(
            persisted_request_state(&admission.client, &wake.doc_id).await,
            RequestLifecycleState::Pending,
            "watcher selection must not preclaim the wake"
        );
        let identity = admission.runtime.clone();
        let signer = admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap();
        let escaped_agent = gents_store::graphql::escape_graphql_string(signer.did());
        let agent_row = admission.client.execute(&format!(
            r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{escaped_agent}" }} }}, limit: 2) {{ agent_did creation_nonce }} }}"#,
        )).await.unwrap();
        let [row] = agent_row["data"]["Agent"].as_array().unwrap().as_slice() else {
            panic!("the selected Agent binding must be unique");
        };
        let binding = serde_json::from_value(row.clone()).unwrap();
        let agent_config = test_agent_with_binding(identity, binding);
        assert_eq!(agent_config.agent_identity.did(), signer.did());
        assert_eq!(agent_config.node_did(), admission.node_did);
        assert_eq!(wake.node_did, admission.node_did);
        assert_eq!(
            wake.max_total_tokens, None,
            "fixture allows the daemon to create an auxiliary title request"
        );
        assert!(
            gents_session::session::session_needs_generated_title(
                &admission.client,
                &admission.node_did,
                wake.requester_did.as_deref(),
                &wake.session_id,
            )
            .await
            .expect("read fixture session title"),
            "fixture starts with a placeholder title so title work is created"
        );

        let prompt_builder = LayeredPromptBuilder::for_agent(
            &agent_config.system_prompt,
            &agent_config.agent_did,
            &[],
            false,
            &[],
        );
        let preamble = prompt_builder.preamble().to_string();
        let provider_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let title_calls = Arc::new(AtomicUsize::new(0));
        let title_shape_mismatches = Arc::new(AtomicUsize::new(0));
        let request_identity = agent_config.node_identity().clone();
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: node.clone(),
            tool_config: Arc::new(AgentToolSurfaceConfig::meta_only()),
            agent_config: agent_config.clone(),
            provider_family: None,
            model: Arc::new(WakeInputModel {
                provider_inputs: provider_inputs.clone(),
                title_calls: title_calls.clone(),
                title_shape_mismatches: title_shape_mismatches.clone(),
                successor: successor.map(|request_id| WakeSuccessor {
                    node: node.clone(),
                    request_id,
                    did: admission.node_did.clone(),
                    signer: signer.clone(),
                    runtime: admission.runtime.clone(),
                }),
            }),
            preamble,
            loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                gents_inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                    node.clone(), agent_config.agent_identity.as_ref(),
                ).unwrap(),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status: crate::runtime::status::RuntimeStatusHandle::new(
                node.clone(),
                admission.node_did.clone(),
            ),
            slot_generation: 1,
            request_admission: gents_session::request::admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                gents_p2p::p2p_reconcile::enrollment_authority_channel().1,
            ),
        })
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        daemon
            .process_request(wake.clone(), shutdown_rx)
            .await
            .unwrap();

        let session_id = gents_store::graphql::escape_graphql_string(&wake.session_id);
        let node_did = gents_store::graphql::escape_graphql_string(&admission.node_did);
        let title_purpose = gents_store::graphql::escape_graphql_string(
            gents_protocol::request_admission::RequestPurpose::TitleAudit.as_str(),
        );
        // The auxiliary title request is the Agent's own work, read as it.
        let agent_reader = admission.tool.access().unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let response = agent_reader
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{session_id}" }}, purpose: {{ _eq: "{title_purpose}" }} }}) {{ lifecycle_state caused_by_parent_request_id caused_by_parent_request_doc_id }} }}"#
                    ))
                    .await
                    .expect("observe pending title request");
                let rows = response["data"]["AgentRequest"]
                    .as_array()
                    .expect("title request rows");
                if !rows.is_empty() {
                    assert_eq!(rows.len(), 1, "one title request for the wake session");
                    assert_eq!(rows[0]["lifecycle_state"], "pending");
                    assert_eq!(rows[0]["caused_by_parent_request_id"], wake.request_id);
                    assert_eq!(rows[0]["caused_by_parent_request_doc_id"], wake.doc_id);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("daemon must persist pending title work before test shutdown");
        assert!(
            gents_session::session::session_needs_generated_title(
                &admission.client,
                &admission.node_did,
                wake.requester_did.as_deref(),
                &wake.session_id,
            )
            .await
            .expect("read title pending session"),
            "creator cannot update the title without watcher dispatch"
        );

        let diagnostic = persisted_request_by_doc_id(&admission.client, &wake.doc_id).await;
        {
            let captured = provider_inputs.lock().expect("wake provider input capture");
            assert_eq!(
                captured.len(),
                1,
                "one deterministic main provider turn must run; title_calls={}, wake={:?}",
                title_calls.load(Ordering::SeqCst),
                diagnostic
            );
            assert_eq!(title_calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                title_shape_mismatches.load(Ordering::SeqCst),
                0,
                "title errors must not be hidden by the fallback"
            );
            let first_input: serde_json::Value =
                serde_json::from_str(&captured[0]).expect("decode first provider chat history");
            assert!(
                json_contains_text(&first_input, &notification_text),
                "exact durable notification must reach first provider input: {}",
                captured[0]
            );
        }

        assert_eq!(
            persisted_request_state(&admission.client, &wake.doc_id).await,
            RequestLifecycleState::Completed,
            "wake must complete through the daemon's owned request path"
        );

        let mut watcher = gents_session::request::watcher::DefraWatcher::for_agents(
            node.clone(),
            &admission.node_did,
            vec![agent_config.agent_identity.clone()],
        )
        .expect("request watcher");
        let successor = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let request = gents_session::request::watcher::Watcher::next_request(&mut watcher)
                    .await
                    .context("successor wake disappeared")??;
                if request.session_id == wake.session_id
                    && request.input.queue.as_ref().is_some_and(|queue| {
                        queue.source
                            == gents_protocol::request_input::QueueSource::BackgroundCompletion
                    })
                {
                    return Ok::<_, anyhow::Error>(request);
                }
            }
        })
        .await
        .unwrap()
        .unwrap();
        assert_ne!(successor.doc_id, wake.doc_id);
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        daemon
            .process_request(successor.clone(), shutdown_rx)
            .await
            .unwrap();
        let [contract] = gents_lean_vocab::families::contracts_wake_input_provider_order::cases()
            .try_into()
            .unwrap_or_else(|_| panic!("the successor wake scenario replays one modeled wake"));
        let labels = ["notification-a", "notification-b", "answer-0"];
        assert_eq!(
            contract.input.observable, labels,
            "the scenario locates these headers in the provider input"
        );
        {
            let captured = provider_inputs.lock().unwrap();
            assert_eq!(captured.len(), 2);
            let input = &captured[1];
            let answer = input
                .find("wake handled")
                .expect("previous wake answer remains in history");
            let new_notification = input
                .find("successor native output")
                .expect("successor notification reaches provider input");
            let mut observed = [
                (
                    "notification-a",
                    input.find("durable native output").unwrap(),
                ),
                ("notification-b", new_notification),
                ("answer-0", answer),
            ];
            observed.sort_by_key(|(_, position)| *position);
            contract.check(observed.map(|(header, _)| header.to_owned()).to_vec());
            assert_eq!(input.matches("successor native output").count(), 1);
            assert_eq!(input.matches("durable native output").count(), 1);
        }
        let history = gents_session::session::load_history_with_access(
            &admission.client,
            &wake.session_id,
            &admission.node_did,
            wake.requester_did.as_deref(),
        )
        .await
        .unwrap();
        let transcript = history
            .iter()
            .map(gents_protocol::transcript::render_message_body_markdown)
            .collect::<Vec<_>>();
        let notification = transcript
            .iter()
            .position(|text| text.contains("successor native output"))
            .unwrap();
        let answer = transcript
            .iter()
            .position(|text| text == "wake handled")
            .unwrap();
        let mut durable = [
            (
                "notification-a",
                transcript
                    .iter()
                    .position(|text| text.contains("durable native output"))
                    .unwrap(),
            ),
            ("notification-b", notification),
            ("answer-0", answer),
        ];
        durable.sort_by_key(|(_, position)| *position);
        let expected = contract
            .input
            .wake
            .rows
            .iter()
            .map(|row| row.header.as_str())
            .filter(|header| labels.contains(header))
            .collect::<Vec<_>>();
        assert_eq!(
            durable.map(|(header, _)| header).as_slice(),
            expected,
            "provider projection must preserve durable append order"
        );
        drop(watcher);

        drop(daemon);
        node.shutdown().await.expect("node shutdown");
        std::fs::remove_dir_all(admission.path).unwrap();
    }

    #[tokio::test]
    async fn started_daemon_rechecks_workspace_root_policy_for_each_fresh_request() {
        let (db, user, user_access, agent_config) =
            held_daemon_fixture("daemon-workspace-root").await;
        let node = db.node.clone();
        let workspace = tempfile::tempdir().unwrap();
        let selected = workspace.path().join("selected");
        std::fs::create_dir_all(&selected).unwrap();
        let selected = std::fs::canonicalize(selected).unwrap();
        let root_path = selected.to_str().unwrap();
        gents_tools::surface::set_workspace_root_with_readers(
            &user_access,
            agent_config.node_did(),
            root_path,
            None,
            true,
            std::slice::from_ref(&agent_config.agent_did),
        )
        .await
        .unwrap();
        let first = create_held_routed_request(&db, &user, &user_access, &agent_config, None).await;
        let prompt_builder = LayeredPromptBuilder::for_agent(
            &agent_config.system_prompt,
            &agent_config.agent_did,
            &[],
            false,
            &[],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime_status = crate::runtime::status::RuntimeStatusHandle::new(
            node.clone(),
            agent_config.node_did().to_string(),
        );
        let request_identity = agent_config.node_identity().clone();
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: node.clone(),
            tool_config: Arc::new(AgentToolSurfaceConfig::meta_only()),
            agent_config: agent_config.clone(),
            provider_family: None,
            model: Arc::new(CountingReplyModel(calls.clone())),
            preamble: prompt_builder.preamble().to_string(),
            loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                gents_inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                    node.clone(),
                    agent_config.agent_identity.as_ref(),
                )
                .unwrap(),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status,
            slot_generation: 1,
            request_admission: gents_session::request::admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                gents_p2p::p2p_reconcile::enrollment_authority_channel().1,
            ),
        })
        .unwrap()
        .with_root_execution_guard(Some(gents_tools::surface::RootExecutionGuard {
            agent_did: agent_config.agent_did.clone(),
            selected_root: Some(selected.clone()),
            ceiling_root: Some(selected.clone()),
        }));
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let first_doc_id = first.doc_id.clone();
        daemon
            .process_request(first, shutdown_rx.clone())
            .await
            .unwrap();
        assert!(
            calls.load(Ordering::SeqCst) > 0,
            "the admitted request must reach the provider before revocation"
        );
        let escaped_first_doc_id = gents_store::graphql::escape_graphql_string(&first_doc_id);
        let first_terminal = user_access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_first_doc_id}" }} }}, limit: 1) {{ lifecycle_state failure_reason terminal_output }} }}"#
        )).await.unwrap();
        let first_row = first_terminal["data"]["AgentRequest"][0].clone();
        assert_eq!(
            first_row["lifecycle_state"], "completed",
            "first request failed after provider call: {first_row}"
        );
        let first_selection: gents_protocol::output::TerminalOutput =
            serde_json::from_value(first_row["terminal_output"].clone()).unwrap();
        assert!(
            matches!(
                first_selection,
                gents_protocol::output::TerminalOutput::Message { .. }
            ),
            "the first request must select its published reply"
        );
        let calls_before_revocation = calls.load(Ordering::SeqCst);

        gents_tools::surface::set_workspace_root(
            &user_access,
            agent_config.node_did(),
            root_path,
            None,
            false,
        )
        .await
        .unwrap();
        let second =
            create_held_routed_request(&db, &user, &user_access, &agent_config, None).await;
        let second_doc_id = second.doc_id.clone();
        daemon.process_request(second, shutdown_rx).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            calls_before_revocation,
            "the revoked fresh request must fail before provider exposure"
        );
        let escaped_request_doc_id = gents_store::graphql::escape_graphql_string(&second_doc_id);
        let rejected = user_access.execute(&format!(
            r#"{{ AgentRequest(filter: {{_docID: {{_eq:"{escaped_request_doc_id}"}}}}, limit:1) {{lifecycle_state terminal_output}} }}"#
        )).await.unwrap();
        let row = &rejected["data"]["AgentRequest"][0];
        assert_eq!(row["lifecycle_state"], "failed");
        let second_selection: gents_protocol::output::TerminalOutput =
            serde_json::from_value(row["terminal_output"].clone()).unwrap();
        assert_eq!(
            second_selection,
            gents_protocol::output::TerminalOutput::NoMessage,
            "a later failed request must not inherit the earlier reply"
        );

        node.shutdown().await.expect("node shutdown");
    }

    #[tokio::test]
    async fn daemon_final_claim_leaves_cross_actor_enrollment_requests_blocked() {
        let (db, member, member_access, agent_config) =
            held_daemon_fixture("daemon-enrollment-final-claim").await;
        let node = db.node.clone();
        let fence = |sequence: u64, request_id: &str| {
            gents_p2p::p2p_reconcile::enrollment_reconcile::EnrollmentAuthorizationFence {
                network_id: "network-1".into(),
                request_id: request_id.into(),
                admin_did: agent_config.node_did().to_string(),
                member_did: member.did().to_string(),
                member_peer: "peer-member".into(),
                member_ticket: "ticket-member".into(),
                owner_node: agent_config.node_did().to_string(),
                profile: gents_p2p::p2p_reconcile::templates::CLIENT_TEMPLATE.into(),
                request_digest: format!("digest-{sequence}"),
                authorization_sequence: sequence,
                authorization_expires_at: "2099-01-01T00:00:00Z".into(),
            }
        };
        let generation_one = fence(1, "enrollment-1");
        let revoked = create_enrollment_daemon_request(
            &db,
            &member_access,
            &agent_config,
            &member,
            &generation_one,
            "revoked",
        )
        .await;
        let (authority, authority_handle) =
            gents_p2p::p2p_reconcile::enrollment_reconcile::test_enrollment_authority(Some(
                generation_one,
            ));

        let prompt_builder = LayeredPromptBuilder::for_agent(
            &agent_config.system_prompt,
            &agent_config.agent_did,
            &[],
            false,
            &[],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime_status = crate::runtime::status::RuntimeStatusHandle::new(
            node.clone(),
            agent_config.node_did().to_string(),
        );
        let request_identity = agent_config.node_identity().clone();
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: node.clone(),
            tool_config: Arc::new(AgentToolSurfaceConfig::meta_only()),
            agent_config: agent_config.clone(),
            provider_family: None,
            model: Arc::new(CountingReplyModel(calls.clone())),
            preamble: prompt_builder.preamble().to_string(),
            loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                gents_inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                    node.clone(),
                    agent_config.agent_identity.as_ref(),
                )
                .unwrap(),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status,
            slot_generation: 1,
            request_admission: gents_session::request::admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                authority_handle,
            ),
        })
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // A member-signed enrollment request is cross-actor work for the
        // target Agent. Without atomic requester authorization in the claim
        // transaction, Request.CrossActorAuthorization.decideClaim is blocked:
        // neither denied nor claimed. Revoking the enrollment fence or
        // replacing it with an exact new generation leaves that decision
        // unchanged, and no generation reaches the provider.
        let assert_blocked = |doc_id: String| {
            let member_access = &member_access;
            async move {
                let observed = member_access.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state claimed_at }} }}"#,
                    gents_store::graphql::escape_graphql_string(&doc_id))).await.unwrap();
                let row = &observed["data"]["AgentRequest"][0];
                assert_eq!(row["lifecycle_state"], "pending", "{doc_id}");
                assert!(row["claimed_at"].is_null(), "{doc_id}");
            }
        };
        authority.replace(None).await;
        daemon
            .process_request(revoked.clone(), shutdown_rx.clone())
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "revoked work reached provider"
        );
        assert_blocked(revoked.doc_id.clone()).await;

        let generation_two = fence(2, "enrollment-2");
        authority.replace(Some(generation_two.clone())).await;
        let replacement = create_enrollment_daemon_request(
            &db,
            &member_access,
            &agent_config,
            &member,
            &generation_two,
            "replacement",
        )
        .await;
        daemon
            .process_request(replacement.clone(), shutdown_rx)
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "cross-actor replacement reached provider without atomic requester authorization"
        );
        assert_blocked(replacement.doc_id.clone()).await;

        node.shutdown().await.expect("node shutdown");
    }
    include!("pre_inference_interrupt_tests.rs");
    include!("execution_lease_regression_tests.rs");
    include!("fold_checkpoint_reclaim_tests.rs");
}
