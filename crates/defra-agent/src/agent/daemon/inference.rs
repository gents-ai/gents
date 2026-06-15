use std::future::IntoFuture;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use tracing::Instrument;

use super::{BehaviorDaemon, HandleRequestOutcome};
use crate::admission::{self, CallKind};
use crate::config::AgentBehavior;
use crate::error::classify_completion_error;
use crate::hook::DefraSessionHook;
use crate::llm::message::Message;
use crate::streaming::{StreamStatus, StreamWriter};
use crate::watcher::AgentRequest;

enum InferenceAttemptOutcome {
    Retry(crate::error::InferenceError),
    Finished(HandleRequestOutcome),
}

type RequestDeadline = Option<DateTime<Utc>>;

fn terminal_response_has_visible_output(streamed_text: &str, final_text: Option<&str>) -> bool {
    !streamed_text.trim().is_empty() || final_text.is_some_and(|text| !text.trim().is_empty())
}

fn is_stream_liveness_timeout(error: &rig::agent::StreamingError) -> bool {
    error.to_string().contains("stream liveness timeout")
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

fn request_workspace_cwd(request: &crate::watcher::AgentRequest) -> Option<PathBuf> {
    let metadata = request.metadata.as_deref()?.trim();
    if metadata.is_empty() {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(metadata).ok()?;
    value
        .pointer("/codex_shim/cwd")
        .or_else(|| value.get("workspace_cwd"))
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
}

fn render_request_context_message(
    node: &defra_node::EmbeddedNode,
    behavior: &AgentBehavior,
    request: &AgentRequest,
) -> Result<Option<Message>> {
    let Some(template) = behavior.request_context_template.as_deref() else {
        return Ok(None);
    };
    if template.trim().is_empty() {
        return Ok(None);
    }

    let reads = crate::template::reads::collect_request_reads(template)
        .map_err(|error| anyhow!("request_context_template parse failed: {error}"))?;
    let mut ctx = serde_json::Map::new();
    ctx.insert(
        "now".to_string(),
        serde_json::json!(Utc::now().to_rfc3339()),
    );
    if reads.contains("ctx.collection_summary") {
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
    Ok(Some(Message::user(format!(
        "<context>\n{rendered}\n</context>"
    ))))
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
    // Threads request, admission, shutdown, and interrupt state into a single
    // inference attempt loop. Splitting further would require re-threading the
    // same receivers through private helpers with no readability gain.
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
    ) -> Result<HandleRequestOutcome> {
        let request_deadline = lifecycle.claimed_deadline_at();
        let workspace_cwd = request_workspace_cwd(request);
        let deadline_at = request
            .deadline
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("")
            .to_string();
        let has_deadline = !deadline_at.is_empty();
        let workspace_cwd_set = workspace_cwd.is_some();
        let max_attempts = self.retry_policy.max_retries + 1;
        let mut last_inference_error: Option<crate::error::InferenceError> = None;

        for attempt in 0..max_attempts {
            ensure_request_deadline_open(request_deadline, "starting inference attempt")?;
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
                    behavior_id = %self.behavior.behavior_id,
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
                    result = await_with_request_deadline(
                        request_deadline,
                        tokio::time::sleep(delay),
                        "waiting for inference retry backoff",
                    ) => {
                        result?;
                    }
                }
            }

            let attempt_index = attempt + 1;
            let request_id = request.request_id.clone();
            let session_id = request.session_id.clone();
            let behavior_id = self.behavior.behavior_id.clone();
            let backend_id = lifecycle.backend_id().to_string();
            let model_name = self.behavior.model_name.clone();
            let attempt_result = async {
                let hook = DefraSessionHook::resume_or_create_with_identity_policy(
                    self.node.clone(),
                    &request.session_id,
                    &self.behavior.behavior_id,
                    self.behavior.agent_did(),
                    self.hook_failure_policy,
                )
                .await?
                .with_background_tool_registry(self.background_tool_registry.clone())
                .with_background_execution_registry(self.background_execution_registry.clone());
                hook.set_active_request_id(Some(request.request_id.clone()))
                    .await;
                hook.set_request_deadline_at(request_deadline).await;
                let persistence_hook = hook.clone();

                // Owned completion loop (#400): drive our own multi-turn stream
                // over the model + tool surface. Per-request sampling is resolved
                // into the loop config from the behavior + request.
                let model = (*self.model).clone();
                let mut loop_config = crate::completion_factory::loop_config_for_request(
                    &self.behavior,
                    self.preamble.clone(),
                    request,
                    self.loop_tools.len(),
                );
                loop_config.context_message =
                    render_request_context_message(self.node.as_ref(), &self.behavior, request)?;
                let loop_prompt = crate::llm::message::Message::user(request.content.clone());
                let loop_history = history.to_vec();
                let loop_tools = self.loop_tools.clone();
                // Keep a per-attempt token for the admission permit and cancel it
                // explicitly on interrupt before dropping the guarded stream. The
                // permit's Drop path observes this token to persist the linked
                // InferenceCall as cancelled rather than a generic stream drop.
                let inference_token = request_token.child_token();
                let inference_token_for_start = inference_token.clone();
                let terminal_failure_reason = admission::terminal_failure_reason_observer();
                let hook_for_start_interrupt = persistence_hook.clone();
                let mut stream = admission::scope_call_with_token_and_failure_reason(
                    CallKind::Inference,
                    attempt_index as i64,
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
                    attempt_index as i64,
                    inference_token.clone(),
                    terminal_failure_reason.clone(),
                    async {
                        let liveness_timeout = self.behavior.stream_liveness_timeout;

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
                                    // #442: a tool that completed inline before the
                                    // interrupt recorded its result on the AgentToolCall
                                    // row but may not have persisted its result message;
                                    // backfill so the transcript stays pair-closed.
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
                                    crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_workspace(
                                        request_deadline,
                                        request_token.clone(),
                                        workspace_cwd.clone(),
                                        tokio::time::timeout(liveness_timeout, stream.next()),
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
                                Ok(Some(item)) => item,
                                Ok(None) => break,
                                Err(_) => {
                                    if let Err(error) = persistence_hook
                                        .fail_in_flight_tool_calls(
                                            "stream liveness timeout while tool call was running",
                                            crate::tool_call_lifecycle::FailureClass::External,
                                        )
                                        .await
                                    {
                                        tracing::warn!(
                                            request_id = %request_id,
                                            session_id = %session_id,
                                            error = %error,
                                            "failed to mark in-flight tool calls failed after stream liveness timeout"
                                        );
                                    }
                                    let timeout_reason = format!(
                                        "stream liveness timeout: no data received for {}s",
                                        liveness_timeout.as_secs()
                                    );
                                    admission::set_terminal_failure_reason(
                                        &terminal_failure_reason,
                                        timeout_reason.clone(),
                                    );
                                    stream_error = Some(rig::agent::StreamingError::Completion(
                                        rig::completion::CompletionError::ProviderError(
                                            timeout_reason,
                                        ),
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
                                && !is_stream_liveness_timeout(&error)
                                && !had_observable_activity
                                && attempt_index < max_attempts;

                            if can_retry {
                                return Ok(InferenceAttemptOutcome::Retry(classified));
                            }

                            let _ = processor
                                .persist_partial_turn("persist errored assistant turn")
                                .await?;
                            // #442: a tool that completed inline before the stream
                            // stalled recorded its result on the AgentToolCall row but
                            // may not have persisted its result message (the streamed
                            // ToolResult never arrived); backfill so the transcript
                            // stays pair-closed and the next request is not sent a
                            // dangling assistant tool call.
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

                        ensure_request_deadline_open(
                            request_deadline,
                            "finalizing inference response",
                        )?;
                        self.stream_writer
                            .finalize(doc_id, StreamStatus::Complete)
                            .await?;

                        Ok(InferenceAttemptOutcome::Finished(
                            HandleRequestOutcome::Completed,
                        ))
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
                retry_attempt = attempt > 0,
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
    use super::{
        await_with_request_deadline, ensure_request_deadline_open, is_stream_liveness_timeout,
        request_deadline_remaining, terminal_response_has_visible_output,
    };
    use std::time::Duration;

    #[test]
    fn terminal_response_requires_visible_output() {
        assert!(!terminal_response_has_visible_output("", None));
        assert!(!terminal_response_has_visible_output("   ", Some("")));
        assert!(!terminal_response_has_visible_output("", Some("   ")));
        assert!(terminal_response_has_visible_output("hello", None));
        assert!(terminal_response_has_visible_output("", Some("hello")));
    }

    #[test]
    fn detects_stream_liveness_timeout_errors() {
        let error = rig::agent::StreamingError::Completion(
            rig::completion::CompletionError::ProviderError(
                "stream liveness timeout: no data received for 30s".into(),
            ),
        );

        assert!(is_stream_liveness_timeout(&error));
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
    async fn retry_backoff_wait_is_cut_off_by_request_deadline() {
        let deadline = chrono::Utc::now() + chrono::Duration::milliseconds(20);
        let started = std::time::Instant::now();

        let result = await_with_request_deadline(
            Some(deadline),
            tokio::time::sleep(Duration::from_secs(5)),
            "waiting for inference retry backoff",
        )
        .await;

        let error = result.expect_err("backoff wait should be bounded by request deadline");
        assert!(error
            .to_string()
            .contains("waiting for inference retry backoff"));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "deadline-bounded retry backoff must not wait for the full retry delay"
        );
    }

    #[tokio::test]
    async fn await_with_request_deadline_allows_fast_work() {
        let deadline = chrono::Utc::now() + chrono::Duration::seconds(1);

        let result = await_with_request_deadline(Some(deadline), async { 42 }, "test wait")
            .await
            .expect("fast work should finish before deadline");

        assert_eq!(result, 42);
    }
}
