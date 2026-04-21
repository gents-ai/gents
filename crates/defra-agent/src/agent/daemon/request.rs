use anyhow::Result;
use tracing::Instrument;

use super::{BehaviorDaemon, HandleRequestOutcome};
use crate::admission::{self, AdmissionCallContext, CallKind};
use crate::compaction::{self, CompactionOptions, Compactor};
use crate::prompt::PromptBuilder;
use crate::session;
use crate::streaming::{StreamStatus, StreamWriter};

/// Grace period after cancellation before force-aborting children, so in-flight
/// cancellable work can observe the cancel and return cleanly. Codex-aligned:
/// long enough for HTTP futures to observe, short enough that Esc feels instant.
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
        let behavior_name = self.behavior.name.clone();
        let admission_context = AdmissionCallContext::for_request(
            &request,
            lifecycle.behavior_id(),
            lifecycle.backend_id(),
        );
        admission::scope_request(admission_context, async {
            let built = async {
                let full_history = session::load_history(&self.node, &request.session_id).await?;
                let (stripped_history, file_activity) =
                    compaction::strip_tool_results(full_history);
                if !file_activity.is_empty() {
                    tracing::debug!(
                        behavior_id = %self.behavior.name,
                        session_id = %request.session_id,
                        files_read = ?file_activity.files_read,
                        files_modified = ?file_activity.files_modified,
                        "files referenced in stripped history"
                    );
                }

                let compaction_entries =
                    session::load_compaction_entries(&self.node, &request.session_id).await?;
                let mut history = drop_compacted_prefix(
                    stripped_history,
                    total_compacted_messages(&compaction_entries),
                );
                let mut summaries = compaction_entries
                    .into_iter()
                    .map(|entry| entry.summary)
                    .collect::<Vec<_>>();

                let mut built = self.prompt_builder.build(&history, &summaries).await?;
                if prompt_exceeds_compaction_threshold(
                    built.estimated_tokens,
                    &request.content,
                    self.behavior.context_window,
                    self.behavior.compaction_threshold,
                ) {
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
                        let entry = session::save_compaction_entry(
                            &self.node,
                            &request.session_id,
                            &summary,
                            &result.files_read,
                            &result.files_modified,
                            result.messages_compacted,
                            result.original_token_estimate,
                            result.compacted_token_estimate,
                        )
                        .await?;
                        summaries.push(entry.summary);
                    }

                    built = self.prompt_builder.build(&history, &summaries).await?;
                }

                Ok::<_, anyhow::Error>(built)
            }
            .instrument(tracing::info_span!(
                "request.prepare_prompt",
                request_id = %request.request_id,
                session_id = %request.session_id,
                behavior_id = %behavior_name,
            ))
            .await?;

            lifecycle.begin_execution().await?;

            let response_behavior_id = lifecycle.behavior_id().to_string();
            let doc_id = self
                .stream_writer
                .begin(
                    &request.session_id,
                    &request.request_id,
                    lifecycle.behavior_id(),
                )
                .instrument(tracing::info_span!(
                    "request.begin_response",
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    behavior_id = %response_behavior_id,
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
                    behavior_id = %inference_behavior_id,
                    backend_id = %inference_backend_id,
                ))
                .await;

            if request_token.is_cancelled() {
                // Interrupt detected inside run_inference — execute the 6-step flow.
                // Graceful fallback: if the token was cancelled by a path that did not
                // also publish an InterruptIntent on the watch channel (e.g. a future
                // tool-child cancellation from Task 8), treat it as a failure rather
                // than panicking. This preserves safety as the cancellation hierarchy
                // expands.
                let Some(intent) = interrupt_rx.borrow().clone() else {
                    tracing::warn!(
                        request_id = %lifecycle.request().request_id,
                        "request_token was cancelled without an interrupt intent on the channel; \
                         treating as failure rather than interrupt"
                    );
                    return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow::anyhow!(
                        "request_token cancelled without interrupt intent"
                    )));
                };

                let interrupt_at = intent.at.to_rfc3339();
                let flow_span = tracing::info_span!(
                    "interrupt.flow",
                    request_id = %lifecycle.request().request_id,
                    interrupt_at = %interrupt_at,
                );
                async {
                    // 1. request_token is already cancelled (the inference arm fired it).
                    // 2. Grace wait so any in-flight work can observe cancellation.
                    tokio::time::sleep(CANCELLATION_GRACE_PERIOD).await;
                    // 3. Force-abort: no child tasks currently (Task 8 adds tool children).
                    // 4. Flip AgentResponse.interrupted_at (sequenced BEFORE step 5).
                    if let Err(error) = self
                        .stream_writer
                        .write_interrupted_at(&doc_id, &interrupt_at)
                        .await
                    {
                        tracing::warn!(
                            behavior_id = %self.behavior.name,
                            doc_id = %doc_id,
                            error = %error,
                            "failed to stamp interrupted_at on response; continuing to terminal transition"
                        );
                    }
                    // 5. Write terminal lifecycle_state = interrupted.
                    lifecycle.transition_to_interrupted().await?;
                    Ok::<_, anyhow::Error>(())
                }
                .instrument(flow_span)
                .await?;
                return Ok(HandleRequestOutcome::Interrupted);
            }

            match result {
                Ok(outcome) => Ok(outcome),
                Err(error) => {
                    if let Err(set_error) = self
                        .stream_writer
                        .set_error_message(&doc_id, &error.to_string())
                        .await
                    {
                        tracing::error!(
                            behavior_id = %self.behavior.name,
                            doc_id = %doc_id,
                            error = %set_error,
                            "failed to persist response error message"
                        );
                    }
                    if let Err(finalize_error) = self
                        .stream_writer
                        .finalize(&doc_id, StreamStatus::Error)
                        .await
                    {
                        tracing::error!(
                            behavior_id = %self.behavior.name,
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

fn total_compacted_messages(entries: &[session::CompactionEntry]) -> usize {
    entries
        .iter()
        .map(|entry| entry.messages_compacted as usize)
        .sum()
}

fn drop_compacted_prefix(
    mut history: Vec<rig::completion::message::Message>,
    compacted: usize,
) -> Vec<rig::completion::message::Message> {
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
