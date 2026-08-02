use anyhow::Result;
use tracing::Instrument;

use super::{BehaviorDaemon, HandleRequestOutcome};
use crate::admission::{self, AdmissionCallContext, CallKind};
use crate::compaction::{self, CompactionOptions, Compactor};
use crate::prompt::PromptBuilder;
use crate::runtime_trace::RequestTraceAttrs;
use crate::session;

const CANCELLATION_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);

impl<M: rig::completion::CompletionModel + 'static> BehaviorDaemon<M> {
    pub(super) async fn handle_request(
        &mut self,
        lifecycle: &mut crate::lifecycle::RequestLifecycle,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        mut interrupt_rx: tokio::sync::watch::Receiver<Option<crate::interrupt::InterruptIntent>>,
    ) -> Result<HandleRequestOutcome> {
        let request_token = tokio_util::sync::CancellationToken::new();
        let request = lifecycle.request().clone();
        let trace_attrs = RequestTraceAttrs::from_request(&request);
        let behavior_name = self.behavior.behavior_id.clone();
        let admission_context = AdmissionCallContext::for_request(
            &request,
            lifecycle.behavior_id(),
            lifecycle.backend_id(),
        );
        let title_admission_context = admission_context.clone();
        admission::scope_request(admission_context, async {
            self.spawn_conversation_title_generation(&request, title_admission_context);

            let selected_skill_ids = selected_skill_ids(request.metadata.as_deref());
            let skill_reminders = self
                .prompt_builder
                .selected_skill_reminders(&selected_skill_ids);
            let skill_reminder_tokens = crate::prompt::estimate_message_tokens(&skill_reminders);

            let mut built = async {
                let full_history = session::load_history(&self.node, &request.session_id)
                    .instrument(tracing::info_span!(
                        "request.load_history",
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        behavior_id = %behavior_name,
                        history_message_count = tracing::field::Empty,
                    ))
                    .await?;
                // One canonical reduction, shared with the compaction writer:
                // `messages_compacted` is measured against this list, so the
                // prefix drop below must index the same one (#993).
                let (provider_history, file_activity) =
                    compaction::provider_view(full_history);
                if !file_activity.is_empty() {
                    tracing::debug!(
                        behavior_id = %self.behavior.behavior_id,
                        session_id = %request.session_id,
                        files_read = ?file_activity.files_read,
                        files_modified = ?file_activity.files_modified,
                        "files referenced in stripped history"
                    );
                }

                let compaction_entries =
                    session::load_compaction_entries(&self.node, &request.session_id)
                        .instrument(tracing::info_span!(
                        "request.load_compaction_entries",
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        behavior_id = %behavior_name,
                        compaction_entry_count = tracing::field::Empty,
                        compacted_message_count = tracing::field::Empty,
                    ))
                    .await?;
                // Drop in the space the count was measured in. The writer's
                // boundary is always `pair_safe_boundary`, so the drop lands on
                // a turn boundary and the tail is still provider-valid — no
                // re-sanitizing needed (`Compaction.drop_preserves_providerValid`).
                let mut history = drop_compacted_prefix(
                    provider_history,
                    total_compacted_messages(&compaction_entries),
                );
                let mut summaries = compaction_entries
                    .into_iter()
                    .map(|entry| compaction::bounded_summary(entry.summary))
                    .collect::<Vec<_>>();

                let mut built = self
                    .prompt_builder
                    .build(&history, &summaries)
                    .instrument(tracing::info_span!(
                        "request.build_prompt",
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        behavior_id = %behavior_name,
                        history_messages = history.len(),
                        summary_count = summaries.len(),
                    ))
                    .await?;
                built.estimated_tokens =
                    built.estimated_tokens.saturating_add(skill_reminder_tokens);
                let over_threshold = prompt_exceeds_compaction_threshold(
                    built.estimated_tokens,
                    &request.content,
                    self.behavior.context_window,
                    self.behavior.compaction_threshold,
                );
                // Runtime counterpart of Lean `PromptView.safeToReduce`,
                // resolved at session scope: while any response in this session
                // is still streaming, a turn is still being written into the
                // transcript and must not be summarized away. All-terminal at
                // session scope implies terminal for every row, so this can only
                // err toward skipping a compaction the next request retries
                // (`boundary.compaction.safe-to-reduce-session-scope`, #993).
                let may_reduce = if over_threshold {
                    let live_response =
                        session::session_has_live_response(&self.node, &request.session_id).await?;
                    let gate_open = if live_response {
                        compaction::safe_to_reduce(&history, &compaction::NoneKnown)
                    } else {
                        compaction::safe_to_reduce(&history, &compaction::AllTerminal)
                    };
                    if !gate_open {
                        tracing::info!(
                            request_id = %request.request_id,
                            session_id = %request.session_id,
                            behavior_id = %behavior_name,
                            "compaction skipped: a response in this session is still streaming"
                        );
                    }
                    gate_open
                } else {
                    false
                };
                if may_reduce {
                    let result = admission::scope_call(
                        CallKind::Compaction,
                        1,
                        self.compactor.compact(
                            history,
                            self.behavior.context_window,
                            &CompactionOptions {
                                strategy: self.behavior.compaction_strategy.clone(),
                                ..self.compaction_options.clone()
                            },
                        ),
                    )
                    .await?;

                    history = result.messages;
                    if let Some(summary) = result.summary {
                        let entry = session::save_compaction_entry_with_requester_did(
                            &self.node,
                            &request.session_id,
                            &request.agent_did,
                            request.requester_did.as_deref(),
                            &summary,
                            &result.files_read,
                            &result.files_modified,
                            result.messages_compacted,
                            result.original_token_estimate,
                            result.compacted_token_estimate,
                        )
                        .await?;
                        summaries.push(compaction::bounded_summary(entry.summary));
                    }

                    built = self
                        .prompt_builder
                        .build(&history, &summaries)
                        .instrument(tracing::info_span!(
                            "request.build_prompt",
                            request_id = %request.request_id,
                            session_id = %request.session_id,
                            behavior_id = %behavior_name,
                            history_messages = history.len(),
                            summary_count = summaries.len(),
                            compacted = true,
                        ))
                        .await?;
                    built.estimated_tokens =
                        built.estimated_tokens.saturating_add(skill_reminder_tokens);
                }

                Ok::<_, anyhow::Error>(built)
            }
            .instrument(tracing::info_span!(
                "request.prepare_prompt",
                request_id = %request.request_id,
                session_id = %request.session_id,
                agent_did = %request.agent_did,
                behavior_id = %behavior_name,
                deadline_at = %trace_attrs.deadline_at,
                has_deadline = trace_attrs.has_deadline,
                subagent_depth = trace_attrs.subagent_depth,
                is_subagent = trace_attrs.is_subagent,
                selected_skill_count = trace_attrs.selected_skill_count,
                workspace_cwd_set = trace_attrs.workspace_cwd_set,
            ))
            .await?;

            if !skill_reminders.is_empty() {
                let mut reminders = skill_reminders;
                reminders.append(&mut built.messages);
                built.messages = reminders;
            }

            lifecycle.begin_execution().await?;

            let response_behavior_id = lifecycle.behavior_id().to_string();
            let doc_id = self
                .stream_writer
                .begin_with_requester_did(
                    &request.session_id,
                    &request.request_id,
                    lifecycle.behavior_id(),
                    request.requester_did.as_deref(),
                )
                .instrument(tracing::info_span!(
                    "request.begin_response",
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    agent_did = %request.agent_did,
                    behavior_id = %response_behavior_id,
                    subagent_depth = trace_attrs.subagent_depth,
                    is_subagent = trace_attrs.is_subagent,
                ))
                .await?;
            lifecycle.set_response_doc_id(&doc_id);
            lifecycle.advance().await?;

            let inference_behavior_id = lifecycle.behavior_id().to_string();
            let inference_backend_id = lifecycle.backend_id().to_string();
            let result = self
                .run_inference(
                    &request,
                    &doc_id,
                    &built.messages,
                    lifecycle,
                    &mut shutdown,
                    &mut interrupt_rx,
                    &request_token,
                )
                .instrument(tracing::info_span!(
                    "request.run_inference",
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    agent_did = %request.agent_did,
                    behavior_id = %inference_behavior_id,
                    backend_id = %inference_backend_id,
                    deadline_at = %trace_attrs.deadline_at,
                    has_deadline = trace_attrs.has_deadline,
                    subagent_depth = trace_attrs.subagent_depth,
                    is_subagent = trace_attrs.is_subagent,
                    selected_skill_count = trace_attrs.selected_skill_count,
                    workspace_cwd_set = trace_attrs.workspace_cwd_set,
                ))
                .await;

            let token_was_cancelled = request_token.is_cancelled();
            let watched_interrupt = { interrupt_rx.borrow().clone() };
            let interrupt_at = if token_was_cancelled {
                if let Some(intent) = watched_interrupt {
                    Some(intent.at.to_rfc3339())
                } else {
                    crate::interrupt::fetch_interrupt_requested_at(&self.node, &request.request_id)
                        .await?
                }
            } else if let Some(intent) = watched_interrupt {
                request_token.cancel();
                Some(intent.at.to_rfc3339())
            } else {
                let persisted =
                    crate::interrupt::fetch_interrupt_requested_at(&self.node, &request.request_id)
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
                    if let Err(error) = self
                        .stream_writer
                        .write_interrupted_at(&doc_id, &interrupt_at)
                        .await
                    {
                        tracing::warn!(
                            behavior_id = %self.behavior.behavior_id,
                            doc_id = %doc_id,
                            error = %error,
                            "failed to stamp interrupted_at on response; continuing to terminal transition"
                        );
                    }
                    if let Err(error) = self.stream_writer.finalize_interrupted_response(&doc_id).await
                    {
                        tracing::warn!(
                            behavior_id = %self.behavior.behavior_id,
                            doc_id = %doc_id,
                            error = %error,
                            "failed to finalize interrupted response; continuing to terminal request transition"
                        );
                    }
                    lifecycle.transition_to_interrupted().await?;
                    if let Err(error) = crate::lifecycle::queue::drain_automated_wakeups(
                        &self.node,
                        &request.session_id,
                        &request.agent_did,
                        "automated wake-up drained because active request was interrupted",
                    )
                    .await
                    {
                        tracing::warn!(
                            request_id = %request.request_id,
                            session_id = %request.session_id,
                            error = %error,
                            "failed to drain automated wake-ups after request interrupt"
                        );
                    }
                    Ok::<_, anyhow::Error>(())
                }
                .instrument(flow_span)
                .await?;
                return Ok(HandleRequestOutcome::Interrupted);
            }

            match result {
                Ok(outcome) => Ok(outcome),
                Err(error) => {
                    if let Err(finalize_error) = self
                        .stream_writer
                        .finalize_error(&doc_id, &error.to_string())
                        .await
                    {
                        tracing::error!(
                            behavior_id = %self.behavior.behavior_id,
                            doc_id = %doc_id,
                            error = %finalize_error,
                            "failed to finalize stream after error"
                        );
                    }
                    Err(error)
                }
            }
        })
        .await
    }
}

fn selected_skill_ids(metadata: Option<&str>) -> Vec<String> {
    let Some(metadata) = metadata else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(metadata) else {
        return Vec::new();
    };
    value
        .get("selected_skill_ids")
        .and_then(|ids| ids.as_array())
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn total_compacted_messages(entries: &[session::CompactionEntry]) -> usize {
    entries
        .iter()
        .map(|entry| entry.messages_compacted as usize)
        .sum()
}

fn drop_compacted_prefix(
    mut history: Vec<crate::llm::message::Message>,
    compacted: usize,
) -> Vec<crate::llm::message::Message> {
    let drain_count = compacted.min(history.len());
    history.drain(..drain_count);
    history
}

fn prompt_exceeds_compaction_threshold(
    prompt_tokens: usize,
    request_text: &str,
    context_window: usize,
    threshold: f64,
) -> bool {
    let budget = (context_window as f64 * threshold) as usize;
    prompt_tokens + compaction::estimate_tokens(request_text) > budget
}
