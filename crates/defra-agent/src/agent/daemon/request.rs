use anyhow::Result;
use tracing::Instrument;

use super::{BehaviorDaemon, HandleRequestOutcome};
use crate::admission::{self, AdmissionCallContext, CallKind};
use crate::compaction::{self, CompactionOptions, Compactor};
use crate::prompt::PromptBuilder;
use crate::session;
use crate::streaming::{StreamStatus, StreamWriter};

impl<M: rig::completion::CompletionModel + 'static> BehaviorDaemon<M> {
    pub(super) async fn handle_request(
        &mut self,
        lifecycle: &mut crate::lifecycle::RequestLifecycle,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<HandleRequestOutcome> {
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
                .run_inference(&request, &doc_id, &built.messages, lifecycle, &mut shutdown)
                .instrument(tracing::info_span!(
                    "request.run_inference",
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    behavior_id = %inference_behavior_id,
                    backend_id = %inference_backend_id,
                ))
                .await;

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
