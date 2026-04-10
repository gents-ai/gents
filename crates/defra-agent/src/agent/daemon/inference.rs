use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::StreamExt;
use rig::streaming::StreamingPrompt;

use super::{BehaviorDaemon, HandleRequestOutcome};
use crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS;
use crate::error::classify_completion_error;
use crate::hook::DefraSessionHook;
use crate::streaming::{StreamStatus, StreamWriter};

impl<M: rig::completion::CompletionModel + 'static> BehaviorDaemon<M> {
    pub(super) async fn run_inference(
        &mut self,
        request: &crate::watcher::AgentRequest,
        doc_id: &str,
        history: &[rig::completion::message::Message],
        lifecycle: &mut crate::lifecycle::RequestLifecycle,
    ) -> Result<HandleRequestOutcome> {
        let max_attempts = self.retry_policy.max_retries + 1;
        let mut last_inference_error: Option<crate::error::InferenceError> = None;

        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = self.retry_policy.delay_for_attempt(attempt - 1);
                tracing::info!(
                    behavior_id = %self.behavior.name,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    request_id = %request.request_id,
                    "retrying inference after transient failure"
                );
                tokio::time::sleep(delay).await;
            }

            let hook = DefraSessionHook::resume_or_create_with_identity_policy(
                self.node.clone(),
                &request.session_id,
                &self.behavior.name,
                self.behavior.did(),
                self.hook_failure_policy,
            )
            .await?;
            let persistence_hook = hook.clone();

            let mut stream = self
                .agent
                .stream_prompt(&request.content)
                .with_history(history.to_vec())
                .with_hook(hook)
                .await;

            let liveness_timeout = Duration::from_secs(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS);

            let (mut streamed_text, final_text, stream_error) = {
                let mut processor = crate::agent::stream_processor::StreamProcessor::new(
                    &persistence_hook,
                    &self.stream_writer,
                    lifecycle,
                    doc_id,
                );
                let mut stream_error = None;

                loop {
                    let item = match tokio::time::timeout(liveness_timeout, stream.next()).await {
                        Ok(Some(item)) => item,
                        Ok(None) => break,
                        Err(_) => {
                            stream_error = Some(rig::agent::StreamingError::Completion(
                                rig::completion::CompletionError::ProviderError(format!(
                                    "stream liveness timeout: no data received for {}s",
                                    DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS
                                )),
                            ));
                            break;
                        }
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

                (processor.streamed_text, processor.final_text, stream_error)
            };

            if let Some(error) = stream_error {
                let classified = classify_completion_error(&error);
                let can_retry = classified.is_retryable()
                    && streamed_text_has_no_visible_content(&streamed_text)
                    && attempt + 1 < max_attempts;

                if can_retry {
                    last_inference_error = Some(classified);
                    continue;
                }

                let error_reason = format!("agent stream failed: {}", error);
                self.stream_writer
                    .set_error_message(doc_id, &error_reason)
                    .await?;
                self.stream_writer
                    .finalize(doc_id, StreamStatus::Error)
                    .await?;

                return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
                    error_reason
                )));
            }

            if let Some(text) = final_text.as_deref() {
                if streamed_text.is_empty() {
                    let _ = self.stream_writer.write_tokens(doc_id, text).await?;
                    streamed_text.push_str(text);
                } else if let Some(remainder) = text.strip_prefix(&streamed_text) {
                    if !remainder.is_empty() {
                        let _ = self.stream_writer.write_tokens(doc_id, remainder).await?;
                        streamed_text.push_str(remainder);
                    }
                }
            }

            self.stream_writer
                .finalize(doc_id, StreamStatus::Complete)
                .await?;

            return Ok(HandleRequestOutcome::Completed);
        }

        let last_error = last_inference_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let error_text = format!(
            "Inference failed after {} attempts: {}",
            max_attempts, last_error
        );
        self.stream_writer
            .set_error_message(doc_id, &error_text)
            .await?;
        let _ = self.stream_writer.write_tokens(doc_id, &error_text).await?;
        self.stream_writer
            .finalize(doc_id, StreamStatus::Error)
            .await?;

        Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
            "inference retries exhausted"
        )))
    }

    pub(super) async fn write_error_response(
        &self,
        request: &crate::watcher::AgentRequest,
        behavior_id: &str,
        error: &anyhow::Error,
    ) -> Result<()> {
        let doc_id = self
            .stream_writer
            .begin(&request.session_id, &request.request_id, behavior_id)
            .await?;
        let error_reason = error.to_string();
        let error_text = format!("Error: {}", error_reason);
        self.stream_writer
            .set_error_message(&doc_id, &error_reason)
            .await?;
        let _ = self
            .stream_writer
            .write_tokens(&doc_id, &error_text)
            .await?;
        self.stream_writer
            .finalize(&doc_id, StreamStatus::Error)
            .await?;
        Ok(())
    }
}

fn streamed_text_has_no_visible_content(text: &str) -> bool {
    text.trim().is_empty()
}
