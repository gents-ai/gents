use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::StreamExt;
use rig::streaming::StreamingPrompt;
use tracing::Instrument;

use super::{BehaviorDaemon, HandleRequestOutcome};
use crate::admission::{self, CallKind};
use crate::completion_factory::agent_with_request_sampling;
use crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS;
use crate::error::classify_completion_error;
use crate::hook::DefraSessionHook;
use crate::streaming::{StreamStatus, StreamWriter};

enum InferenceAttemptOutcome {
    Retry(crate::error::InferenceError),
    Finished(HandleRequestOutcome),
}

fn terminal_response_has_visible_output(streamed_text: &str, final_text: Option<&str>) -> bool {
    !streamed_text.trim().is_empty() || final_text.is_some_and(|text| !text.trim().is_empty())
}

impl<M: rig::completion::CompletionModel + 'static> BehaviorDaemon<M> {
    pub(super) async fn run_inference(
        &mut self,
        request: &crate::watcher::AgentRequest,
        doc_id: &str,
        history: &[rig::completion::message::Message],
        lifecycle: &mut crate::lifecycle::RequestLifecycle,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
        interrupt_rx: &mut tokio::sync::watch::Receiver<Option<crate::interrupt::InterruptIntent>>,
        request_token: &tokio_util::sync::CancellationToken,
    ) -> Result<HandleRequestOutcome> {
        let max_attempts = self.retry_policy.max_retries + 1;
        let mut last_inference_error: Option<crate::error::InferenceError> = None;

        for attempt in 0..max_attempts {
            if *shutdown.borrow() {
                return Err(anyhow!("shutdown requested during inference"));
            }
            if interrupt_rx.borrow().is_some() {
                request_token.cancel();
                return Err(anyhow!("request interrupted during inference"));
            }
            if attempt > 0 {
                let delay = self.retry_policy.delay_for_attempt(attempt - 1);
                tracing::info!(
                    behavior_id = %self.behavior.name,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    request_id = %request.request_id,
                    "retrying inference after transient failure"
                );
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => {
                        return Err(anyhow!("shutdown requested during inference retry backoff"));
                    }
                    _ = interrupt_rx.changed() => {
                        request_token.cancel();
                        return Err(anyhow!("request interrupted during inference"));
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
            }

            let attempt_index = attempt + 1;
            let request_id = request.request_id.clone();
            let session_id = request.session_id.clone();
            let behavior_id = self.behavior.name.clone();
            let backend_id = lifecycle.backend_id().to_string();
            let model_name = self.behavior.model_name.clone();
            let attempt_result = async {
                let hook = DefraSessionHook::resume_or_create_with_identity_policy(
                    self.node.clone(),
                    &request.session_id,
                    &self.behavior.name,
                    self.behavior.did(),
                    self.hook_failure_policy,
                )
                .await?;
                hook.set_active_request_id(Some(request.request_id.clone()))
                    .await;
                let persistence_hook = hook.clone();

                let agent = agent_with_request_sampling(&self.agent, &self.behavior, request);
                // Child token so any admission-layer cancel stays scoped to inference;
                // cancels from request_token still propagate down via the parent.
                let mut stream = admission::scope_call_with_token(
                    CallKind::Inference,
                    attempt_index as i64,
                    request_token.child_token(),
                    async {
                        tokio::select! {
                            biased;
                            _ = shutdown.changed() => {
                                Err(anyhow!("shutdown requested before inference stream started"))
                            }
                            _ = interrupt_rx.changed() => {
                                request_token.cancel();
                                Err(anyhow!("request interrupted during inference"))
                            }
                            stream = agent
                                .stream_prompt(&request.content)
                                .with_history(history.to_vec())
                                .with_hook(hook) => Ok::<_, anyhow::Error>(stream)
                        }
                    },
                )
                .await?;

                let liveness_timeout = Duration::from_secs(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS);

                let mut processor = crate::agent::stream_processor::StreamProcessor::new(
                    &persistence_hook,
                    &self.stream_writer,
                    lifecycle,
                    doc_id,
                );
                let mut stream_error = None;

                loop {
                    let item = match tokio::select! {
                        biased;
                        _ = shutdown.changed() => {
                            return Err(anyhow!("shutdown requested during inference stream"));
                        }
                        _ = interrupt_rx.changed() => {
                            request_token.cancel();
                            return Err(anyhow!("request interrupted during inference"));
                        }
                        result = tokio::time::timeout(liveness_timeout, stream.next()) => result
                    } {
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

                let had_observable_activity = processor.has_observable_activity();

                if let Some(error) = stream_error {
                    let classified = classify_completion_error(&error);
                    let can_retry = classified.is_retryable()
                        && !had_observable_activity
                        && attempt_index < max_attempts;

                    if can_retry {
                        return Ok(InferenceAttemptOutcome::Retry(classified));
                    }

                    let _ = processor
                        .persist_partial_turn("persist errored assistant turn")
                        .await?;

                    let error_reason = format!("agent stream failed: {}", error);
                    self.stream_writer
                        .set_error_message(doc_id, &error_reason)
                        .await?;
                    self.stream_writer
                        .finalize(doc_id, StreamStatus::Error)
                        .await?;

                    return Ok(InferenceAttemptOutcome::Finished(
                        HandleRequestOutcome::FailedAfterResponse(anyhow!(error_reason)),
                    ));
                }

                let mut streamed_text = std::mem::take(&mut processor.streamed_text);
                let final_text = processor.final_text.take();

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

                if !terminal_response_has_visible_output(&streamed_text, final_text.as_deref()) {
                    let error_reason =
                        "agent stream completed without producing any visible response content";
                    self.stream_writer
                        .set_error_message(doc_id, error_reason)
                        .await?;
                    self.stream_writer
                        .finalize(doc_id, StreamStatus::Error)
                        .await?;

                    return Ok(InferenceAttemptOutcome::Finished(
                        HandleRequestOutcome::FailedAfterResponse(anyhow!(error_reason)),
                    ));
                }

                self.stream_writer
                    .finalize(doc_id, StreamStatus::Complete)
                    .await?;

                Ok(InferenceAttemptOutcome::Finished(
                    HandleRequestOutcome::Completed,
                ))
            }
            .instrument(tracing::info_span!(
                "inference.attempt",
                request_id = %request_id,
                session_id = %session_id,
                behavior_id = %behavior_id,
                backend_id = %backend_id,
                model_name = %model_name,
                attempt = attempt_index,
                max_attempts,
            ))
            .await?;

            match attempt_result {
                InferenceAttemptOutcome::Retry(classified) => {
                    last_inference_error = Some(classified);
                    continue;
                }
                InferenceAttemptOutcome::Finished(outcome) => return Ok(outcome),
            }
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

#[cfg(test)]
mod tests {
    use super::terminal_response_has_visible_output;

    #[test]
    fn terminal_response_requires_visible_output() {
        assert!(!terminal_response_has_visible_output("", None));
        assert!(!terminal_response_has_visible_output("   ", Some("")));
        assert!(!terminal_response_has_visible_output("", Some("   ")));
        assert!(terminal_response_has_visible_output("hello", None));
        assert!(terminal_response_has_visible_output("", Some("hello")));
    }
}
