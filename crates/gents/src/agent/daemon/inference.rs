use std::future::IntoFuture;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use gents_loop::output_obligation::OutputObligationCheck;
use tracing::Instrument;

use super::{AgentDaemon, HandleRequestOutcome, ShutdownDrainFailure};
use crate::agent::loop_stream::{LoopReplayInput, TaggedMessage};
use crate::agent::stream_processor::{processor_failure, RuntimeSeamError};
use crate::error::LoopFailureCause;
use crate::inference::admission::{self, CallKind};
use crate::inference::compaction::ReductionOptions;
use crate::inference::llm::message::Message;
use crate::inference::resolved::{MaxTurnsProvenance, ResolvedAgent};
use crate::request::streaming::StreamWriter;
use crate::request::watcher::AgentRequest;
use crate::tools::call::hook::DefraSessionHook;

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
                crate::tools::background::completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
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
    _node: &defra_node::EmbeddedNode,
    agent_config: &ResolvedAgent,
    request: &AgentRequest,
    frozen_instruction_manifest: Option<&str>,
) -> Result<Option<Message>> {
    Ok(assemble_request_context_message(
        frozen_instruction_manifest,
        crate::tools::workspace::request_workspace_cwd(request).as_deref(),
        agent_config.tools.host_tools().read_root(),
    ))
}

fn assemble_request_context_message(
    frozen_instruction_manifest: Option<&str>,
    live_cwd: Option<&std::path::Path>,
    live_tool_root: Option<&std::path::Path>,
) -> Option<Message> {
    crate::tools::workspace::instruction_body_for_request(
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

impl<M> AgentDaemon<M>
where
    M: crate::inference::llm::rig_compat::ProviderModel,
{
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_inference(
        &mut self,
        request: &crate::request::watcher::AgentRequest,
        doc_id: &str,
        history: &[TaggedMessage],
        wake_notifications: &[TaggedMessage],
        mut replay: LoopReplayInput,
        lifecycle: &mut crate::request::lifecycle::RequestLifecycle,
        stream_writer: &crate::request::streaming::DefraStreamWriter,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
        interrupt_rx: &mut tokio::sync::watch::Receiver<
            Option<crate::request::interrupt::InterruptIntent>,
        >,
        request_token: &tokio_util::sync::CancellationToken,
        aggregate_token_budget: Option<crate::agent::loop_stream::AggregateTokenBudget>,
        effective_seed: Option<i64>,
        mut workspace: crate::tools::call::lifecycle::runtime::ToolWorkspaceScope,
        request_context_message: Option<crate::inference::llm::message::Message>,
        resume_from_history: bool,
        consumed_prompts: Vec<crate::agent::loop_stream::FoldedPrompt>,
        folded_prompts: Vec<crate::agent::loop_stream::FoldedPrompt>,
        context_calibration: crate::provider_input::budget::ContextCalibration,
    ) -> Result<HandleRequestOutcome> {
        workspace.runtime_home = self.approval_home.clone();
        let request_deadline = lifecycle.claimed_deadline_at();
        let trigger_context = crate::request::lifecycle::TriggerExecutionContext::parse(
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
        let agent_id = self.agent_config.agent_id.clone();
        let reviewer = self.workspace_reviewer(
            request,
            aggregate_token_budget.clone(),
            request_token.clone(),
            shutdown.clone(),
        );
        let approval_context = crate::plugin::approval::workspace::for_request(
            self.approval_home.as_deref(),
            &request.node_did,
            request.requester_did.as_deref(),
            &agent_id,
            workspace.workspace_cwd.as_deref(),
        )?
        .map(|context| context.with_reviewer(reviewer.callback.clone()));
        let backend_id = lifecycle.backend_id().to_string();
        let model_name = self.agent_config.model_name.clone();
        let inference_observation = self
            .inference_observations
            .begin(crate::inference_observation::InferenceKey {
                node: request.node_did.clone(),
                session: request.session_id.clone(),
                request: request.request_id.clone(),
                generation: lifecycle.execution_generation()?.to_owned(),
            })
            .map(Arc::new);
        let inference = Box::pin(async {
                let prepared = async {
                    let output_obligation_gate = crate::agent::output_obligation::OutputObligationGate::for_request(
                        self.node.clone(), request, self.output_obligations.as_ref(),
                    ).await?;
                    let hook = DefraSessionHook::resume_with_identity_policy(
                        self.node.clone(),
                        &request.session_id,
                        self.agent_config.node_did(),
                        request.requester_did.as_deref(),
                        self.hook_failure_policy,
                    )
                    .await?
                    .with_inference_observation(inference_observation.clone())
                    .with_background_tool_registry(self.background_tool_registry.clone())
                    .with_background_execution_registry(self.background_execution_registry.clone())
                    .with_operator_tool_root(self.operator_tool_root.clone())
                    .with_remote_tools(self.remote_tools.clone())
                    .with_job_tools(self.job_tools.clone())
                    .with_goal_tool_authority(
                        self.agent_config.tools.goal_tools_requested(),
                        self.agent_config.tools.goal_creation_requested(),
                    )
                    .with_output_obligation_gate(output_obligation_gate.clone())
                    .with_steering_inputs(
                        request.clone(),
                        self.request_admission.clone(),
                        lifecycle.natural_finish_allowed(),
                    );
                    let hook = match &self.job_driver {
                        Some(driver) => hook.with_job_driver(driver.clone()),
                        None => hook,
                    };
                    hook.set_active_request_binding(
                        Some(request.request_id.clone()),
                        Some(request.doc_id.clone()),
                        request.requester_did.clone(),
                    )
                    .await;
                    hook.set_request_deadline_at(request_deadline).await;
                    let persistence_hook = hook.clone();

                    let model = (*self.model).clone();
                    let mut loop_config = crate::completion_factory::loop_config_for_request(
                        &self.agent_config,
                        self.preamble.clone(),
                        request,
                        aggregate_token_budget.clone(),
                        self.loop_tools.len(),
                    )?;
                    loop_config.deadline = request_deadline;
                    loop_config.context_calibration = context_calibration.clone();
                    loop_config.output_obligation_gate = output_obligation_gate
                        .map(|gate| Arc::new(gate) as Arc<dyn OutputObligationCheck>);
                    let turn_compactor = self.compactor.clone();
                    let provider_profile = loop_config.provider_input_counter.profile();
                    let turn_context_window = self.agent_config.context_window;
                    let max_turns_provenance = self.agent_config.max_turns_provenance;
                    let turn_compaction_options = self.compaction_options_for_request(
                        request_deadline,
                        aggregate_token_budget,
                        effective_seed,
                        context_calibration.clone(),
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
                                    Output = Result<
                                        crate::agent::loop_stream::TurnCompactionOutcome,
                                        crate::agent::loop_stream::TurnCompactionError,
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
                            let provider_view = crate::agent::loop_stream::provider_view_tagged(
                                compaction_request.messages,
                            )
                            .map_err(compactor_failure)?;
                            options.max_compacted_prefix_messages =
                                crate::agent::loop_stream::replay_compaction_prefix_bound(
                                    &provider_view, &compaction_request.required,
                                )
                                .map_err(crate::inference::compaction::CompactionError::from)?;
                            let native_messages = crate::agent::loop_stream::provider_messages(
                                provider_profile,
                                &provider_view,
                            );
                            options.keep_recent_tokens = compactor.retention_target(
                                options.keep_recent_tokens,
                                &native_messages,
                                compaction_request.admission,
                                &options,
                                turn_context_window,
                            )?;
                            // Request-local sticky projection, not a
                            // session-prefix cursor; the stronger
                            // safe_to_reduce gate belongs to session compaction.
                            let source_boundary =
                                crate::provider_context_reduction::capture_source_boundary(
                                    node.as_ref(),
                                    &request.session_id,
                                    &request.node_did,
                                    request.requester_did.as_deref(),
                                    &request.doc_id,
                                    &request_commit_cid,
                                )
                                .await
                                .map_err(compactor_failure)?;
                            let (result, producer_join) = admission::scope_call_with_join(
                                CallKind::Compaction,
                                1,
                                compactor.reduce(
                                    native_messages,
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
                            let provider_values = crate::agent::loop_stream::provider_messages(
                                provider_profile,
                                &provider_view,
                            );
                            let Some(exact) = result.exact_reduction() else {
                                if result.cannot_fit() {
                                    return Ok(
                                        crate::agent::loop_stream::TurnCompactionOutcome::CannotFit,
                                    );
                                }
                                if result
                                    .provider_messages()
                                    .map_err(crate::inference::compaction::CompactionError::from)?
                                    != provider_values
                                {
                                    return Err(compactor_failure(
                                        "repaired provider view disagrees with source-index projection",
                                    ));
                                }
                                return Ok(
                                    crate::agent::loop_stream::TurnCompactionOutcome::ProviderViewRepaired {
                                        messages: provider_view,
                                    },
                                );
                            };
                            if !exact
                                .compacted_prefix
                                .iter()
                                .chain(exact.retained_suffix)
                                .eq(provider_values.iter())
                            {
                                return Err(compactor_failure(
                                    "exact reduction split disagrees with source-index provider view",
                                ));
                            }
                            let split = exact.compacted_prefix.len();
                            let native = crate::agent::loop_stream::association_reduction(
                                provider_profile,
                                &provider_view,
                                exact,
                            )?;
                            let exact = native.exact();
                            let associations =
                                crate::provider_context_reduction::ReplayAssociations::from_tagged_split(
                                    compaction_request.required,
                                    &provider_view[..split],
                                    &provider_view[split..],
                                );
                            let reduction_index = compaction_request.prior_reduction_keys.len() + 1;
                            let (row, _) =
                                crate::provider_context_reduction::persist_exact(
                                &node,
                                crate::provider_context_reduction::NewExactProviderContextReduction {
                                    node_did: &request.node_did,
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
                                    replay_associations: &associations,
                                    original_tokens: result.original_token_estimate,
                                    compacted_tokens: result.compacted_token_estimate,
                                },
                                exact,
                            )
                            .await
                            .map_err(compactor_failure)?;
                            Ok(crate::agent::loop_stream::TurnCompactionOutcome::Reduced {
                                messages: row.checkpoint_tagged_messages().map_err(compactor_failure)?,
                                reduction_key: row.reduction_key,
                            })
                        })
                    };
                    loop_config.turn_compactor =
                        Some(std::sync::Arc::new(turn_compactor_callback));
                    loop_config.context_message = request_context_message.clone();
                    loop_config.resume_from_history = resume_from_history;
                    loop_config.folded_prompts = folded_prompts.clone();
                    let authored_prompt = request.input.authored_message(&request.content)?;
                    let authored_input = |folded: Vec<crate::agent::loop_stream::FoldedPrompt>| {
                        crate::agent::loop_stream::AuthoredInput {
                            context: request_context_message.clone(),
                            prompt: authored_prompt.clone(),
                            folded,
                        }
                    };
                    loop_config.authored_input = Some(authored_input(folded_prompts.clone()));
                    let restored = crate::provider_context_reduction::load_unconsumed_for_request(
                        self.node.as_ref(),
                        &request.doc_id,
                    )
                    .await?;
                    let (loop_history, loop_prompt) = if let Some((row, lineage_keys)) = restored {
                        let mut messages = row.checkpoint_tagged_messages()?;
                        let prompt = messages.pop().context(
                            "durable provider-context checkpoint has no current prompt",
                        )?;
                        anyhow::ensure!(
                            row.node_did == request.node_did
                                && row.requester_did == request.requester_did
                                && row.session_id == request.session_id
                                && row.request_id == request.request_id
                                && row.request_doc_id == request.doc_id,
                            "durable provider-context checkpoint crossed its current request scope"
                        );
                        let boundary = row.source_boundary()?;
                        let replay_scope = crate::session::CanonicalReplayScope {
                            expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                            node_did: &request.node_did,
                            requester_did: request.requester_did.as_deref(),
                            session_id: &request.session_id,
                            request_id: &request.request_id,
                            request_doc_id: &request.doc_id,
                            request_commit_cid: &row.request_commit_cid,
                        };
                        replay.required = row.replay_associations()?.required;
                        crate::session::validate_canonical_replay_boundary(
                            self.node.as_ref(), replay_scope, &boundary,
                        ).await?;
                        loop_config.context_message = None;
                        // The checkpoint's provider view already holds every
                        // consumed message; a selection made at this claim
                        // waits for the next turn.
                        loop_config.folded_prompts = Vec::new();
                        loop_config.authored_input = Some(authored_input(consumed_prompts.clone()));
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
                        let entry = request_entry(
                            history.to_vec(),
                            &authored_prompt,
                            resume_from_history,
                            loop_config.context_message.as_ref(),
                            wake_notifications,
                        )?;
                        if resume_from_history {
                            loop_config.context_message = None;
                        }
                        entry
                    };
                    loop_config.replay = replay;
                    Ok::<_, anyhow::Error>((
                        hook,
                        persistence_hook,
                        model,
                        loop_config,
                        loop_history,
                        loop_prompt,
                        max_turns_provenance,
                    ))
                };
                let Some(prepared) =
                    super::request::prepare_unless_interrupted(interrupt_rx, prepared).await
                else {
                    request_token.cancel();
                    return Err(anyhow!("request interrupted during inference"));
                };
                let (
                    hook,
                    persistence_hook,
                    model,
                    loop_config,
                    loop_history,
                    loop_prompt,
                    max_turns_provenance,
                ) = prepared?;
                let loop_tools = self.loop_tools.clone();
                let inference_token = request_token.child_token();
                let inference_token_for_start = inference_token.clone();
                let terminal_failure_reason = admission::terminal_failure_reason_observer();
                let hook_for_start_interrupt = persistence_hook.clone();
                let provider_profile = loop_config.provider_input_counter.profile();
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
                            stream_writer,
                            lifecycle,
                            doc_id,
                            provider_profile,
                        );
                        let mut lease_poll = tokio::time::interval(Duration::from_secs(1));
                        lease_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        let mut stream_error = None;

                        loop {
                            let mut flush_failure = None;
                            let flush_deadline = processor.next_flush_deadline().await;
                            let item = match tokio::select! {
                                biased;
                                _ = shutdown.changed() => {
                                    drop(stream);
                                    match tokio::time::timeout(
                                        crate::store::config_client::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                                        async {
                                            match processor.validate_execution().await.map_err(RuntimeSeamError::into_inner) {
                                                Ok(()) => {}
                                                Err(error) if error.is::<crate::request::lifecycle::ExecutionOwnershipLost>() => {
                                                    tracing::warn!(
                                                        request_id = %request_id,
                                                        session_id = %session_id,
                                                        error = %error,
                                                        "execution ownership lost before shutdown drain; received in-memory bytes, if any, were not committed by this drain; only durable output remains recoverable"
                                                    );
                                                    return Ok(false);
                                                }
                                                Err(error) => return Err(error),
                                            }
                                            processor.persist_received_partial_turn("persist received assistant turn during graceful shutdown").await.map_err(processor_failure)?;
                                            Ok(true)
                                        },
                                    ).await {
                                        Ok(Ok(false)) => return Err(anyhow::Error::new(crate::request::lifecycle::ExecutionOwnershipLost)
                                            .context("shutdown drain was not attempted")),
                                        Ok(Ok(true)) => return Err(anyhow!("shutdown requested during inference stream")),
                                        Ok(Err(error)) => return Err(ShutdownDrainFailure {
                                            reason: format!("partial-turn persistence failed: {error:#}"),
                                        }.into()),
                                        Err(_) => return Err(ShutdownDrainFailure {
                                            reason: "partial-turn persistence exceeded the embedded storage-step deadline; commit outcome must be recovered from durable state".into(),
                                        }.into()),
                                    }
                                }
                                _ = async {
                                    tokio::select! {
                                        _ = request_token.cancelled() => {},
                                        _ = interrupt_rx.changed() => {},
                                    }
                                } => {
                                    request_token.cancel();
                                    inference_token.cancel();
                                    drop(stream);
                                    let output_result = processor
                                        .persist_received_partial_turn("persist interrupted assistant turn")
                                        .await.map_err(processor_failure);
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
                                    if let Err(error) = output_result {
                                        tracing::warn!(
                                            request_id = %request_id,
                                            session_id = %session_id,
                                            error = %error,
                                            "failed to persist received provider audit before interrupt terminal transition"
                                        );
                                        return Err(error.context("persisting received provider audit during interrupt"));
                                    }
                                    return Err(anyhow!("request interrupted during inference"));
                                }
                                _ = lease_poll.tick() => {
                                    if let Err(error) = processor.validate_execution().await.map_err(RuntimeSeamError::into_inner) {
                                        let reason = format!("request execution lease: {error:#}");
                                        admission::set_terminal_failure_reason(
                                            &terminal_failure_reason,
                                            reason.clone(),
                                        );
                                        drop(stream);
                                        return Err(error);
                                    }
                                    lease_poll.reset();
                                    continue;
                                }
                                _ = async {
                                    match flush_deadline {
                                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                                        None => std::future::pending::<()>().await,
                                    }
                                } => {
                                    let progress = async {
                                        crate::tools::call::lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
                                            request_deadline,
                                            request_token.clone(),
                                            None,
                                            None,
                                            Some(session_id.clone()),
                                            trigger_correlation.clone(),
                                            trigger_context.source_fields.clone(),
                                            false,
                                            stream.next(),
                                        ).await
                                    };
                                    let (flushed, item) = crate::agent::stream_processor::flush_while_polling(
                                        processor.flush_pending(), progress,
                                    ).await;
                                    match item {
                                        Some(Some(item)) => {
                                            flush_failure = flushed.err().map(processor_failure);
                                            Some(item)
                                        }
                                        Some(None) => { flushed.map_err(processor_failure)?; None }
                                        None => { flushed.map_err(processor_failure)?; continue }
                                    }
                                }
                                result = await_with_request_deadline(
                                    request_deadline,
                                    crate::tools::call::lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
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
                                            admission::set_terminal_failure_reason(
                                                &terminal_failure_reason,
                                                error.to_string(),
                                            );
                                            drop(stream);
                                            let output_result = processor
                                                .persist_received_partial_turn(
                                                    "persist deadline-expired assistant turn",
                                                )
                                                .await.map_err(processor_failure);
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
                                            output_result.context(
                                                "persisting received provider audit after request deadline",
                                            )?;
                                            return Err(error);
                                        }
                                    }
                                }
                            } {
                                Some(item) => item,
                                None => break,
                            };
                            let processed = match processor.process_item(item).await.map_err(processor_failure) {
                                Ok(processed) => processed,
                                Err(error) => {
                                    drop(stream);
                                    return Err(match flush_failure {
                                        Some(flush_error) => error.context(format!(
                                            "retaining the received stream item after a flush failed: {flush_error:#}"
                                        )),
                                        None => error,
                                    });
                                }
                            };
                            if let Some(error) = flush_failure {
                                drop(stream);
                                if processor.natural_finished {
                                    return Err(error);
                                }
                                let retained = processor
                                    .persist_received_partial_turn("persist received assistant turn after flush failure")
                                    .await.map_err(processor_failure);
                                return Err(match retained {
                                    Ok(_) => error,
                                    Err(retention_error) => retention_error.context(format!(
                                        "retaining received output after a flush failed: {error:#}"
                                    )),
                                });
                            }
                            if let Err(error) = if processor.natural_finished { Ok(()) } else { ensure_request_deadline_open(
                                request_deadline,
                                "processing inference stream item",
                            ) } {
                                admission::set_terminal_failure_reason(
                                    &terminal_failure_reason,
                                    error.to_string(),
                                );
                                let output_result = processor
                                    .persist_received_partial_turn(
                                        "persist deadline-expired assistant turn",
                                    )
                                    .await.map_err(processor_failure);
                                drop(stream);
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
                                output_result.context(
                                    "persisting received provider audit after request deadline",
                                )?;
                                return Err(error);
                            }
                            match processed {
                                crate::agent::stream_processor::StreamAction::Continue => {}
                                crate::agent::stream_processor::StreamAction::Done => break,
                                crate::agent::stream_processor::StreamAction::Error(error) => {
                                    stream_error = Some(error);
                                    break;
                                }
                            }
                        }

                        drop(stream);
                        if let Some(error) = stream_error {
                            let _ = processor
                                .persist_received_partial_turn("persist errored assistant turn")
                                .await.map_err(processor_failure)?;
                            let error_reason = stream_failure_reason(
                                &error.to_string(),
                                error.cause(),
                                max_turns_provenance,
                            );
                            return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
                                error_reason
                            )));
                        }

                        if processor.final_text.is_none() {
                            processor
                                .persist_received_partial_turn("persist unterminated assistant turn")
                                .await.map_err(processor_failure)
                                .context("persisting received provider audit after stream EOF")?;
                            return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
                                "provider stream ended without an explicit terminal response"
                            )));
                        }

                        if processor.natural_finished {
                            return Ok(HandleRequestOutcome::Completed);
                        }

                        let mut streamed_text = std::mem::take(&mut processor.streamed_text);
                        let final_text = processor.final_text.take();

                        if let Some(text) = final_text.as_deref() {
                            if streamed_text.is_empty() {
                                let _ = stream_writer.write_tokens(doc_id, text).await?;
                                streamed_text.push_str(text);
                            } else if let Some(remainder) = text.strip_prefix(&streamed_text) {
                                if !remainder.is_empty() {
                                    let _ =
                                        stream_writer.write_tokens(doc_id, remainder).await?;
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

                        if let Err(error) = ensure_request_deadline_open(
                            request_deadline,
                            "finalizing inference response",
                        ) {
                            processor
                                .persist_received_partial_turn(
                                    "persist deadline-expired assistant turn",
                                )
                                .await.map_err(processor_failure)
                                .context("persisting received provider audit after request deadline")?;
                            return Err(error);
                        }
                        Ok(HandleRequestOutcome::Completed)
                    },
                )
                .await
            }
            .instrument(tracing::info_span!(
                "inference.attempt",
                request_id = %request_id,
                session_id = %session_id,
                node_did = %request.node_did,
                agent_id = %agent_id,
                backend_id = %backend_id,
                model_name = %model_name,
                deadline_at = %deadline_at,
                has_deadline,
                request_hop = request.request_hop,
                workspace_cwd_set,
                attempt = attempt_index,
                retry_attempt = false,
            )));

        let outcome = crate::tools::call::lifecycle::runtime::scope_tool_request_identity(
            request.requester_did.clone(),
            Some(request.node_did.clone()),
            Some(agent_id.to_string()),
            Some(request.request_id.clone()),
            crate::tools::call::lifecycle::runtime::scope_request_tool_execution_with_workspace_overlay(
                request_deadline,
                request_token.clone(),
                workspace,
                None,
                Some(session_id.clone()),
                trigger_correlation.clone(),
                trigger_context.source_fields.clone(),
                false,
                crate::plugin::approval::scope_interactive(
                    request.execution_origin.as_deref() == Some("interactive"),
                    crate::plugin::approval::workspace::scope(approval_context, inference),
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
) -> crate::agent::loop_stream::TurnCompactionError {
    crate::agent::loop_stream::TurnCompactionError::Compactor(error.into())
}

#[cfg(test)]
pub(super) mod tests {
    use super::{stream_failure_reason, AgentDaemon, MaxTurnsProvenance};
    use crate::agent::completion_retry::CompletionRetryProfileFields;
    use crate::agent::daemon::AgentDaemonInit;
    use crate::agent::runtime::StartupBarrier;
    use crate::error::LoopFailureCause;
    use crate::inference::backends::provider::BackendProviderKind;
    use crate::inference::llm::tool::ToolDyn;
    use crate::inference::resolved::{ResolvedAgent, SamplingConfig};
    use crate::prompt::LayeredPromptBuilder;
    use crate::request::watcher::AgentRequest;
    use crate::store::identity::{KeyIdentity, NodeIdentity, RuntimeNode};
    use crate::tools::call::hook::{
        BackgroundExecutionRegistry, BackgroundToolRegistry, FailurePolicy,
    };
    use crate::tools::surface::AgentToolSurfaceConfig;
    use anyhow::Context;
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
            crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;
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
        successor: Option<(Arc<defra_node::EmbeddedNode>, String, String)>,
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
            crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;
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
                        if let Some((node, request_id, did)) = &self.successor {
                            crate::tools::call::lifecycle::admission_fixture::complete_child(
                                node,
                                request_id,
                                did,
                                "successor native output",
                            )
                            .await;
                            assert_eq!(
                                crate::tools::background::completion::settle_running_session_message_rows(
                                    node, did
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
        node: &defra_node::EmbeddedNode,
        doc_id: &str,
    ) -> serde_json::Value {
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state failure_reason terminal_output }} }}"#,
                crate::store::graphql::escape_graphql_string(doc_id)
            ))
            .await;
        assert!(
            !response.has_errors(),
            "request lifecycle query: {:?}",
            response.errors
        );
        let rows = response.data.as_ref().unwrap()["AgentRequest"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1);
        rows[0].clone()
    }

    async fn persisted_request_state(
        node: &defra_node::EmbeddedNode,
        doc_id: &str,
    ) -> gents_protocol::request_lifecycle::RequestLifecycleState {
        let row = persisted_request_by_doc_id(node, doc_id).await;
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
            crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;
            self.0.fetch_add(1, Ordering::SeqCst);
            let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::iter(vec![
                Ok(RawStreamingChoice::Message("admitted reply".to_string())),
                Ok(RawStreamingChoice::FinalResponse(())),
            ]));
            Ok(StreamingCompletionResponse::stream(inner))
        }
    }

    pub(in crate::agent::daemon) fn test_agent_with_deadline(
        deadline: Duration,
    ) -> Arc<ResolvedAgent> {
        let mut agent_config = ResolvedAgent::clone(&test_agent());
        agent_config.deadline_duration = deadline;
        Arc::new(agent_config)
    }

    fn test_agent() -> Arc<ResolvedAgent> {
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(
                std::env::temp_dir().join(format!("daemon-lineage-{}.key", uuid::Uuid::new_v4())),
                None,
            )
            .expect("test identity"),
        );
        test_agent_with_identity(identity)
    }

    fn test_agent_with_identity(identity: Arc<dyn NodeIdentity>) -> Arc<ResolvedAgent> {
        let principal = Arc::new(RuntimeNode {
            node_did: identity.did().to_string(),
            identity,
            default_agent_id: "general".to_string(),
            display_name: None,
            enabled: true,
        });

        Arc::new(ResolvedAgent {
            profile_id: None,
            agent_id: "general".to_string(),
            node: principal,
            backend_id: Some("backend-general".to_string()),
            backend_provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
            backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
            backend_auth: crate::documents::BackendAuth::Unauthenticated,
            model_name: "scripted".to_string(),
            service_tier: None,
            resolved_reasoning_efforts: None,
            context_window: 8_192,
            max_output_tokens: 1_024,
            max_turns: 2,
            max_turns_provenance: crate::inference::resolved::MaxTurnsProvenance::Default,
            system_prompt: "system".to_string(),
            tools: AgentToolSurfaceConfig::meta_only(),
            compaction: None,
            compaction_inference: None,
            max_total_tokens: None,
            stream_batch_ms: 0,
            stream_liveness_timeout: Duration::from_secs(5),
            deadline_duration: Duration::from_secs(30),
            provider_idle_timeout: Duration::from_secs(
                crate::inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
            ),
            completion_retry: CompletionRetryProfileFields::default(),
            sampling: SamplingConfig::default(),
            skills: Vec::new(),
        })
    }

    async fn create_routed_request(
        node: &std::sync::Arc<defra_node::EmbeddedNode>,
        agent_config: &ResolvedAgent,
        requester_did: &str,
    ) -> AgentRequest {
        crate::test_support::install_test_agent(
            node,
            agent_config.node_did(),
            &agent_config.agent_id,
        )
        .await;
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            agent_config.node_did(),
            requester_did,
            &agent_config.agent_id,
            session_id,
            "route this reply",
            "interactive",
            created_at,
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                agent_config.node_did(),
            ),
        );
        create.request_hop = 1;
        create.caused_by_parent_request_id = Some("parent-request".into());
        create.caused_by_parent_request_doc_id = Some("parent-request-doc".into());
        create.caused_by_parent_tool_call_id = Some("parent-tool-call".into());
        create.caused_by_parent_tool_call_doc_id = Some("parent-tool-call-doc".into());
        crate::sign_agent_request_create(agent_config.node_identity().as_ref(), &mut create)
            .await
            .unwrap();
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(
            !response.has_errors(),
            "create routed AgentRequest failed: {:?}",
            response.errors
        );
        let doc_id =
            crate::store::graphql::single_mutation_document(&response, "create_AgentRequest")
                .unwrap()
                .expect("created request receipt")["_docID"]
                .as_str()
                .expect("created request physical ID")
                .to_owned();

        crate::request::admission::load_request_for_admission_test(node, &doc_id)
            .await
            .unwrap()
    }

    async fn create_enrollment_daemon_request(
        node: &std::sync::Arc<defra_node::EmbeddedNode>,
        agent_config: &ResolvedAgent,
        member: &dyn NodeIdentity,
        fence: &crate::agent::p2p_reconcile::enrollment_reconcile::EnrollmentAuthorizationFence,
        suffix: &str,
    ) -> AgentRequest {
        crate::test_support::install_test_agent(
            node,
            agent_config.node_did(),
            &agent_config.agent_id,
        )
        .await;
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            format!("request-{suffix}"),
            agent_config.node_did(),
            member.did(),
            &agent_config.agent_id,
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
            crate::store::graphql::escape_graphql_string(&create.request_id)
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
        crate::request::admission::load_request_for_admission_test(node, doc_id)
            .await
            .unwrap()
    }

    #[test]
    fn wake_frontier_prompt_framing_scopes() {
        use super::{
            request_entry, wake_frontier_message, TaggedMessage, BACKGROUND_WAKE_RESULT_FRAMING,
        };
        use crate::inference::llm::message::Message;
        use crate::tools::background::completion::BACKGROUND_COMPLETION_WAKE_PROMPT;

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
                crate::inference::llm::message::UserContent::text(
                    BACKGROUND_COMPLETION_WAKE_PROMPT,
                ),
                crate::inference::llm::message::UserContent::Document(
                    crate::inference::llm::message::Document {
                        data: crate::inference::llm::message::DocumentSourceKind::String(
                            "fn main() {}".into(),
                        ),
                        media_type: Some(crate::inference::llm::message::DocumentMediaType::TXT),
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
        for case in &crate::lean_vocab_test::lean_contract_snapshot().retry_entry_cases {
            let message = |value: &serde_json::Value| {
                use crate::inference::llm::message::{
                    AssistantContent, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
                    UserContent,
                };
                let native = match value.as_u64().unwrap() {
                    2 => crate::inference::llm::message::Message::Assistant {
                        id: None,
                        content: vec![AssistantContent::ToolCall(ToolCall {
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
                    3 => crate::inference::llm::message::Message::User {
                        content: vec![UserContent::ToolResult(ToolResult {
                            id: "call-1".into(),
                            call_id: Some("call-1".into()),
                            content: vec![ToolResultContent::Text(Text {
                                text: "recorded result".into(),
                            })],
                        })],
                    },
                    4 => crate::inference::llm::message::Message::assistant("recorded progress"),
                    10 => crate::inference::llm::message::Message::user(super::RETRY_CONTINUATION),
                    _ => crate::inference::llm::message::Message::user(value.to_string()),
                };
                super::TaggedMessage::unassociated(native)
            };
            let history = case["history"]
                .as_array()
                .unwrap()
                .iter()
                .map(message)
                .collect();
            let context = (!case["context"].is_null()).then(|| message(&case["context"]).message);
            let (mut actual, prompt) = super::request_entry(
                history,
                &crate::inference::llm::message::Message::user(case["authored"].to_string()),
                case["resume"].as_bool().unwrap(),
                context.as_ref(),
                &[],
            )
            .unwrap();
            actual.push(prompt);
            let expected: Vec<_> = case["expected"]
                .as_array()
                .unwrap()
                .iter()
                .map(message)
                .collect();
            assert_eq!(actual, expected, "{case}");
        }
    }

    #[tokio::test]
    async fn daemon_request_path_stamps_requester_lineage_on_hook_messages() {
        use gents_protocol::request_lifecycle::RequestLifecycleState;

        let data_path =
            std::env::temp_dir().join(format!("daemon-requester-lineage-{}", uuid::Uuid::new_v4()));
        let node = Arc::new(
            crate::test_support::node_builder()
                .data_path(&data_path)
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("runtime schemas");

        let agent_config = test_agent();
        let requester_did = agent_config.node_did().to_string();
        let request = create_routed_request(&node, &agent_config, &requester_did).await;
        let prompt_builder = LayeredPromptBuilder::for_agent(
            &agent_config.system_prompt,
            &agent_config.agent_id,
            &[],
            false,
            &[],
        );
        let preamble = prompt_builder.preamble().to_string();
        let loop_tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(Vec::new());
        let runtime_status = crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            agent_config.node_did().to_string(),
        );
        let request_identity = agent_config.node_identity().clone();
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: node.clone(),
            agent_config,
            provider_family: None,
            model: Arc::new(RoutedReplyModel),
            preamble,
            loop_tools,
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                crate::inference::rendered_request::defra_rendered_request_capture_factory(
                    node.clone(),
                ),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status,
            slot_generation: 1,
            request_admission: crate::request::admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                crate::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
        })
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        daemon
            .process_request(request.clone(), shutdown_rx)
            .await
            .unwrap();

        assert_eq!(
            persisted_request_state(&node, &request.doc_id).await,
            RequestLifecycleState::Completed,
            "successful processing returns only after request completion"
        );
        let request_doc_id = crate::store::graphql::escape_graphql_string(&request.doc_id);
        let requester = crate::store::graphql::escape_graphql_string(&requester_did);
        let authored_prompt = node
            .execute(&format!(
                r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }}, requester_did: {{ _eq: "{requester}" }}, role: {{ _eq: "user" }} }}) {{ _docID message_key request_doc_id requester_did role }} }}"#
            ))
            .await;
        assert!(
            !authored_prompt.has_errors(),
            "query completed request's authored prompt failed: {:?}",
            authored_prompt.errors
        );
        let authored_rows = authored_prompt.data.unwrap()["AgentMessage"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            authored_rows.iter().any(|row| {
                row["request_doc_id"] == request.doc_id
                    && row["requester_did"] == requester_did
                    && row["message_key"]
                        == crate::session::canonical_rows::authored_message_key(
                            &request.doc_id,
                            "prompt",
                        )
            }),
            "completed fresh non-retry request must have its exact durable authored prompt: {authored_rows:?}"
        );

        let escaped_session_id = crate::store::graphql::escape_graphql_string(&request.session_id);
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
        let access = crate::store::config_client::ConfigAccess::local(node.clone());
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
            let (_, message) = crate::session::load_canonical_message(
                &access,
                doc_id,
                &requester_did,
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
            node.execute(&format!(
                "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ lifecycle_state failure_reason terminal_output }} }}",
                crate::store::graphql::escape_graphql_string(&request.doc_id)
            )).await
        );

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(data_path);
    }

    #[tokio::test]
    async fn successor_background_wake_moves_notification_after_previous_answer() {
        use gents_protocol::request_lifecycle::RequestLifecycleState;

        let (selected, successor) =
            crate::tools::call::lifecycle::completion_owner_conformance::selected_background_wake_with_successor(true)
                .await;
        let admission = selected.admission;
        let wake = selected.wake;
        let notification_text = selected.notification_text;
        let node = admission.node.clone();
        assert_eq!(
            persisted_request_state(&node, &wake.doc_id).await,
            RequestLifecycleState::Pending,
            "watcher selection must not preclaim the wake"
        );
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(admission.path.join("test-agent.key"), None)
                .expect("load the signed fixture's original identity"),
        );
        let agent_config = test_agent_with_identity(identity);
        assert_eq!(agent_config.node_did(), admission.node_did);
        assert_eq!(wake.node_did, admission.node_did);
        assert_eq!(
            wake.max_total_tokens, None,
            "fixture allows the daemon to create an auxiliary title request"
        );
        assert!(
            crate::session::session_needs_generated_title(
                &node,
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
            &agent_config.agent_id,
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
            agent_config,
            provider_family: None,
            model: Arc::new(WakeInputModel {
                provider_inputs: provider_inputs.clone(),
                title_calls: title_calls.clone(),
                title_shape_mismatches: title_shape_mismatches.clone(),
                successor: successor
                    .map(|request| (node.clone(), request, admission.node_did.clone())),
            }),
            preamble,
            loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                crate::inference::rendered_request::defra_rendered_request_capture_factory(
                    node.clone(),
                ),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status: crate::runtime_status::RuntimeStatusHandle::new(
                node.clone(),
                admission.node_did.clone(),
            ),
            slot_generation: 1,
            request_admission: crate::request::admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                crate::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
        })
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        daemon
            .process_request(wake.clone(), shutdown_rx)
            .await
            .unwrap();

        let session_id = crate::store::graphql::escape_graphql_string(&wake.session_id);
        let node_did = crate::store::graphql::escape_graphql_string(&admission.node_did);
        let title_purpose = crate::store::graphql::escape_graphql_string(
            gents_protocol::request_admission::RequestPurpose::TitleAudit.as_str(),
        );
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let response = crate::store::config_client::ConfigAccess::local(node.clone())
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
            crate::session::session_needs_generated_title(
                &node,
                &admission.node_did,
                wake.requester_did.as_deref(),
                &wake.session_id,
            )
            .await
            .expect("read title pending session"),
            "creator cannot update the title without watcher dispatch"
        );

        let diagnostic = persisted_request_by_doc_id(&node, &wake.doc_id).await;
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
            persisted_request_state(&node, &wake.doc_id).await,
            RequestLifecycleState::Completed,
            "wake must complete through the daemon's owned request path"
        );

        let mut watcher =
            crate::request::watcher::DefraWatcher::new(node.clone(), &admission.node_did)
                .expect("request watcher");
        let successor = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let request = crate::request::watcher::Watcher::next_request(&mut watcher)
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
        let contract = crate::lean_vocab_test::lean_contract_snapshot()
            .wake_input_cases
            .iter()
            .find(|case| case.name == "successor_wake_moves_only_new_notification")
            .unwrap();
        let labels = ["notification-a", "notification-b", "answer-0"];
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
            let expected = contract
                .expected_order
                .iter()
                .map(String::as_str)
                .filter(|header| labels.contains(header))
                .collect::<Vec<_>>();
            assert_eq!(
                observed.map(|(header, _)| header).as_slice(),
                expected,
                "current wake order must match the model: {input}"
            );
            assert_eq!(input.matches("successor native output").count(), 1);
            assert_eq!(input.matches("durable native output").count(), 1);
        }
        let history = crate::session::load_history(
            &node,
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
        node.shutdown().await;
        std::fs::remove_dir_all(admission.path).unwrap();
    }

    #[tokio::test]
    async fn started_daemon_rechecks_workspace_root_policy_for_each_fresh_request() {
        let data_path = std::env::temp_dir().join(format!(
            "daemon-workspace-root-recheck-{}",
            uuid::Uuid::new_v4()
        ));
        let node = Arc::new(
            crate::test_support::node_builder()
                .data_path(&data_path)
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let selected = workspace.path().join("selected");
        std::fs::create_dir_all(&selected).unwrap();
        let selected = std::fs::canonicalize(selected).unwrap();
        let escaped_selected =
            crate::store::graphql::escape_graphql_string(&selected.to_string_lossy());
        let created = node
            .execute(&format!(
                r#"mutation {{ create_WorkspaceRoot(input: {{root_path:"{escaped_selected}", enabled:true}}) {{_docID}} }}"#
            ))
            .await;
        assert!(!created.has_errors(), "{:?}", created.errors);
        let workspace_root_doc_id =
            crate::store::graphql::single_mutation_document(&created, "create_WorkspaceRoot")
                .unwrap()
                .expect("created WorkspaceRoot")["_docID"]
                .as_str()
                .expect("WorkspaceRoot document id")
                .to_owned();

        let agent_config = test_agent();
        let requester_did = agent_config.node_did().to_string();
        let first = create_routed_request(&node, &agent_config, &requester_did).await;
        let prompt_builder = LayeredPromptBuilder::for_agent(
            &agent_config.system_prompt,
            &agent_config.agent_id,
            &[],
            false,
            &[],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime_status = crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            agent_config.node_did().to_string(),
        );
        let request_identity = agent_config.node_identity().clone();
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: node.clone(),
            agent_config: agent_config.clone(),
            provider_family: None,
            model: Arc::new(CountingReplyModel(calls.clone())),
            preamble: prompt_builder.preamble().to_string(),
            loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                crate::inference::rendered_request::defra_rendered_request_capture_factory(
                    node.clone(),
                ),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status,
            slot_generation: 1,
            request_admission: crate::request::admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                crate::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
        })
        .unwrap()
        .with_root_execution_guard(Some(crate::tools::surface::RootExecutionGuard {
            agent_id: agent_config.agent_id.clone(),
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
        let escaped_first_doc_id = crate::store::graphql::escape_graphql_string(&first_doc_id);
        let first_terminal = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_first_doc_id}" }} }}, limit: 1) {{ lifecycle_state failure_reason terminal_output }} }}"#
            ))
            .await;
        assert!(!first_terminal.has_errors(), "{:?}", first_terminal.errors);
        let first_row: serde_json::Value =
            crate::store::graphql::first_row(&first_terminal, "AgentRequest")
                .unwrap()
                .unwrap();
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

        let escaped_doc_id = crate::store::graphql::escape_graphql_string(&workspace_root_doc_id);
        let revoked = node
            .execute(&format!(
                r#"mutation {{ update_WorkspaceRoot(filter: {{_docID: {{_eq:"{escaped_doc_id}"}}}}, input: {{enabled:false}}) {{_docID}} }}"#
            ))
            .await;
        assert!(!revoked.has_errors(), "{:?}", revoked.errors);
        let second = create_routed_request(&node, &agent_config, &requester_did).await;
        let second_doc_id = second.doc_id.clone();
        daemon.process_request(second, shutdown_rx).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            calls_before_revocation,
            "the revoked fresh request must fail before provider exposure"
        );
        let escaped_request_doc_id = crate::store::graphql::escape_graphql_string(&second_doc_id);
        let rejected = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{_docID: {{_eq:"{escaped_request_doc_id}"}}}}, limit:1) {{lifecycle_state terminal_output}} }}"#
            ))
            .await;
        assert!(!rejected.has_errors(), "{:?}", rejected.errors);
        let lifecycle_state = rejected
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(serde_json::Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("lifecycle_state"))
            .and_then(serde_json::Value::as_str);
        assert_eq!(lifecycle_state, Some("failed"));
        let second_selection: gents_protocol::output::TerminalOutput = serde_json::from_value(
            rejected.data.as_ref().unwrap()["AgentRequest"][0]["terminal_output"].clone(),
        )
        .unwrap();
        assert_eq!(
            second_selection,
            gents_protocol::output::TerminalOutput::NoMessage,
            "a later failed request must not inherit the earlier reply"
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
            crate::test_support::node_builder()
                .data_path(&data_path)
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let agent_config = test_agent();
        let member_dir = tempfile::tempdir().unwrap();
        let member: Arc<dyn NodeIdentity> = Arc::new(
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
                owner_node: agent_config.node_did().to_string(),
                profile: crate::agent::p2p_reconcile::templates::CLIENT_TEMPLATE.into(),
                request_digest: format!("digest-{sequence}"),
                authorization_sequence: sequence,
                authorization_expires_at: "2099-01-01T00:00:00Z".into(),
            }
        };
        let generation_one = fence(1, "enrollment-1");
        let revoked = create_enrollment_daemon_request(
            &node,
            &agent_config,
            member.as_ref(),
            &generation_one,
            "revoked",
        )
        .await;
        let (authority, authority_handle) =
            crate::agent::p2p_reconcile::enrollment_reconcile::test_enrollment_authority(Some(
                generation_one,
            ));

        let prompt_builder = LayeredPromptBuilder::for_agent(
            &agent_config.system_prompt,
            &agent_config.agent_id,
            &[],
            false,
            &[],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime_status = crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            agent_config.node_did().to_string(),
        );
        let request_identity = agent_config.node_identity().clone();
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: node.clone(),
            agent_config: agent_config.clone(),
            provider_family: None,
            model: Arc::new(CountingReplyModel(calls.clone())),
            preamble: prompt_builder.preamble().to_string(),
            loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                crate::inference::rendered_request::defra_rendered_request_capture_factory(
                    node.clone(),
                ),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status,
            slot_generation: 1,
            request_admission: crate::request::admission::AgentRequestAdmissionVerifier::new(
                node.clone(),
                request_identity,
                authority_handle,
            ),
        })
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

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
        let rejected = node.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state claimed_at }} }}"#,
            crate::store::graphql::escape_graphql_string(&revoked.doc_id))).await;
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
            &node,
            &agent_config,
            member.as_ref(),
            &generation_two,
            "replacement",
        )
        .await;
        daemon
            .process_request(replacement, shutdown_rx)
            .await
            .unwrap();
        assert!(
            calls.load(Ordering::SeqCst) > 0,
            "exact replacement did not reach provider"
        );

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(data_path);
    }
    include!("pre_inference_interrupt_tests.rs");
    include!("execution_lease_regression_tests.rs");
    include!("fold_checkpoint_reclaim_tests.rs");
}
