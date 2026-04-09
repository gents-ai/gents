use std::time::Duration;

use anyhow::{bail, Result};

use super::{BehaviorDaemon, HandleRequestOutcome};
use crate::backend_registry::{self, BackendPermit};
use crate::compaction::{self, CompactionOptions, Compactor};
use crate::prompt::PromptBuilder;
use crate::session;
use crate::streaming::{StreamStatus, StreamWriter};

const BACKEND_WAIT_POLL_MS: u64 = 1_000;

impl<M: rig::completion::CompletionModel + 'static> BehaviorDaemon<M> {
    pub(super) async fn acquire_backend_permit(
        &self,
        lifecycle: &mut crate::lifecycle::RequestLifecycle,
    ) -> Result<BackendPermit> {
        let backend_id = lifecycle.backend_id();
        if backend_id.is_empty() {
            bail!(
                "request {} cannot start because behavior {} has no backend binding",
                lifecycle.request().request_id,
                self.behavior.name
            );
        }

        let deadline = tokio::time::Instant::now() + self.behavior.deadline_duration;
        loop {
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "timed out waiting for backend {} capacity before inference start",
                    backend_id
                );
            }

            let backend = match backend_registry::lookup_backend(&self.node, backend_id).await? {
                Some(backend) => backend,
                None => bail!(
                    "backend {} not found for behavior {}",
                    backend_id,
                    self.behavior.name
                ),
            };

            if backend.is_available() {
                if let Some(permit) = self
                    .backend_tracker
                    .try_acquire_permit(backend_id, backend.max_concurrent)
                {
                    lifecycle.mark_slot_acquired().await?;
                    return Ok(permit);
                }
            }

            tokio::time::sleep(Duration::from_millis(BACKEND_WAIT_POLL_MS)).await;
        }
    }

    pub(super) async fn handle_request(
        &mut self,
        lifecycle: &mut crate::lifecycle::RequestLifecycle,
    ) -> Result<HandleRequestOutcome> {
        let request = lifecycle.request().clone();
        let full_history = session::load_history(&self.node, &request.session_id).await?;
        let (stripped_history, file_activity) = compaction::strip_tool_results(full_history);
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
            let result = self
                .compactor
                .compact(
                    history,
                    self.behavior.context_window,
                    &CompactionOptions {
                        strategy: self.behavior.compaction_strategy.clone(),
                        ..self.compaction_options.clone()
                    },
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

        let _backend_permit = self.acquire_backend_permit(lifecycle).await?;
        lifecycle.begin_execution().await?;

        let doc_id = self
            .stream_writer
            .begin(
                &request.session_id,
                &request.request_id,
                lifecycle.behavior_id(),
            )
            .await?;
        lifecycle.set_response_doc_id(&doc_id);
        lifecycle.advance().await?;

        let result = self
            .run_inference(&request, &doc_id, &built.messages, lifecycle)
            .await;

        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
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
