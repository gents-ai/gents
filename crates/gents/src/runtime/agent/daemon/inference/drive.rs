use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use super::super::{HandleRequestOutcome, ShutdownDrainFailure};
use super::{
    await_with_request_deadline, box_stream_branch, ensure_request_deadline_open,
    stream_failure_reason, terminal_response_has_visible_output, RequestDeadline,
};
use crate::runtime::agent::loop_stream::LoopStreamItem;
use crate::runtime::agent::stream_processor::{processor_failure, StreamProcessor};
use crate::turn::provider_input::ProviderInputProfile;
use gents_inference::admission;
use gents_inference::resolved::MaxTurnsProvenance;
use gents_loop::error::LoopStreamError;
use gents_session::request::lifecycle::{RequestLifecycle, TriggerExecutionContext};
use gents_session::request::seam_error::RuntimeSeamError;
use gents_session::request::streaming::{DefraStreamWriter, StreamWriter};
use gents_toolcalls::call::hook::DefraSessionHook;

type Processor<'a> = StreamProcessor<'a, DefraSessionHook, DefraStreamWriter, RequestLifecycle>;

#[derive(Clone, Copy)]
pub(super) struct StreamDrive<'a> {
    pub(super) persistence_hook: &'a DefraSessionHook,
    pub(super) stream_writer: &'a DefraStreamWriter,
    pub(super) doc_id: &'a str,
    pub(super) provider_profile: ProviderInputProfile,
    pub(super) request_token: &'a CancellationToken,
    pub(super) inference_token: &'a CancellationToken,
    pub(super) request_id: &'a String,
    pub(super) session_id: &'a String,
    pub(super) trigger_correlation: &'a Option<String>,
    pub(super) trigger_context: &'a TriggerExecutionContext,
    pub(super) request_deadline: RequestDeadline,
    pub(super) terminal_failure_reason: &'a std::sync::Arc<std::sync::Mutex<Option<String>>>,
    pub(super) max_turns_provenance: MaxTurnsProvenance,
}

pub(super) async fn drive_inference_stream<S>(
    drive: StreamDrive<'_>,
    mut stream: S,
    lifecycle: &mut RequestLifecycle,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
    interrupt_rx: &mut tokio::sync::watch::Receiver<
        Option<gents_session::request::interrupt::InterruptIntent>,
    >,
) -> Result<HandleRequestOutcome>
where
    S: Stream<Item = Result<LoopStreamItem, LoopStreamError>> + Unpin,
{
    let StreamDrive {
        persistence_hook,
        stream_writer,
        doc_id,
        provider_profile,
        request_token,
        inference_token,
        request_id,
        session_id,
        trigger_correlation,
        trigger_context,
        request_deadline,
        terminal_failure_reason,
        ..
    } = drive;
    let mut processor = crate::runtime::agent::stream_processor::StreamProcessor::new(
        persistence_hook,
        stream_writer,
        lifecycle,
        doc_id,
        provider_profile,
    );
    let mut lease_poll = tokio::time::interval(Duration::from_secs(1));
    lease_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut stream_error = None;

    // The drain and flush futures are large. Construct them only
    // after their branch wins so polling the live stream retains
    // enough stack for nested provider and storage owners.
    loop {
        let mut flush_failure = None;
        let flush_deadline = processor.next_flush_deadline().await;
        let item = match tokio::select! {
            biased;
            _ = shutdown.changed() => {
                drop(stream);
                let processor = &mut processor;
                let request_id = &request_id;
                let session_id = &session_id;
                return box_stream_branch(move || async move {
                    match tokio::time::timeout(
                        gents_store::config_client::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                        async {
                            match processor.validate_execution().await.map_err(RuntimeSeamError::into_inner) {
                                Ok(()) => {}
                                Err(error) if gents_session::request::lifecycle::ExecutionOwnershipLost::occurs_in(&error) => {
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
                        Ok(Ok(false)) => Err(anyhow::Error::new(gents_session::request::lifecycle::ExecutionOwnershipLost)
                            .context("shutdown drain was not attempted")),
                        Ok(Ok(true)) => Err(anyhow!("shutdown requested during inference stream")),
                        Ok(Err(error)) => Err(ShutdownDrainFailure {
                            reason: format!("partial-turn persistence failed: {error:#}"),
                        }.into()),
                        Err(_) => Err(ShutdownDrainFailure {
                            reason: "partial-turn persistence exceeded the embedded storage-step deadline; commit outcome must be recovered from durable state".into(),
                        }.into()),
                    }
                }).await;
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
                let processor = &mut processor;
                return box_stream_branch(move || {
                    drain_for_interrupt(processor, persistence_hook, request_id, session_id)
                })
                .await;
            }
            _ = lease_poll.tick() => {
                if let Err(error) = processor.validate_execution().await.map_err(RuntimeSeamError::into_inner) {
                    let reason = format!("request execution lease: {error:#}");
                    admission::set_terminal_failure_reason(
                        terminal_failure_reason,
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
                let processor = &mut processor;
                let stream = &mut stream;
                let (flushed, item) = box_stream_branch(move || async move {
                    let progress = async {
                        gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
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
                    crate::runtime::agent::stream_processor::flush_while_polling(
                        processor.flush_pending(), progress,
                    ).await
                }).await;
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
                gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
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
                            terminal_failure_reason,
                            error.to_string(),
                        );
                        drop(stream);
                        let processor = &mut processor;
                        return box_stream_branch(move || {
                            drain_for_deadline(
                                processor,
                                persistence_hook,
                                request_id,
                                session_id,
                                error,
                            )
                        })
                        .await;
                    }
                }
            }
        } {
            Some(item) => item,
            None => break,
        };
        let processed = match Box::pin(processor.process_item(item))
            .await
            .map_err(processor_failure)
        {
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
                .persist_received_partial_turn(
                    "persist received assistant turn after flush failure",
                )
                .await
                .map_err(processor_failure);
            return Err(match retained {
                Ok(_) => error,
                Err(retention_error) => retention_error.context(format!(
                    "retaining received output after a flush failed: {error:#}"
                )),
            });
        }
        if let Err(error) = if processor.natural_finished {
            Ok(())
        } else {
            ensure_request_deadline_open(request_deadline, "processing inference stream item")
        } {
            admission::set_terminal_failure_reason(terminal_failure_reason, error.to_string());
            let output_result = processor
                .persist_received_partial_turn("persist deadline-expired assistant turn")
                .await
                .map_err(processor_failure);
            drop(stream);
            if let Err(sweep_error) = persistence_hook.timeout_expired_tool_calls().await {
                tracing::warn!(
                    request_id = %request_id,
                    session_id = %session_id,
                    error = %sweep_error,
                    "failed to sweep expired in-flight tool calls after request deadline"
                );
            }
            output_result.context("persisting received provider audit after request deadline")?;
            return Err(error);
        }
        match processed {
            crate::runtime::agent::stream_processor::StreamAction::Continue => {}
            crate::runtime::agent::stream_processor::StreamAction::Done => break,
            crate::runtime::agent::stream_processor::StreamAction::Error(error) => {
                stream_error = Some(error);
                break;
            }
        }
    }

    drop(stream);
    finish_inference_stream(drive, processor, stream_error).await
}

async fn drain_for_interrupt(
    processor: &mut Processor<'_>,
    persistence_hook: &DefraSessionHook,
    request_id: &str,
    session_id: &str,
) -> Result<HandleRequestOutcome> {
    let output_result = processor
        .persist_received_partial_turn("persist interrupted assistant turn")
        .await
        .map_err(processor_failure);
    if let Err(error) = persistence_hook.cancel_in_flight_tool_calls().await {
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
    Err(anyhow!("request interrupted during inference"))
}

async fn drain_for_deadline(
    processor: &mut Processor<'_>,
    persistence_hook: &DefraSessionHook,
    request_id: &str,
    session_id: &str,
    error: anyhow::Error,
) -> Result<HandleRequestOutcome> {
    let output_result = processor
        .persist_received_partial_turn("persist deadline-expired assistant turn")
        .await
        .map_err(processor_failure);
    if let Err(sweep_error) = persistence_hook.timeout_expired_tool_calls().await {
        tracing::warn!(
            request_id = %request_id,
            session_id = %session_id,
            error = %sweep_error,
            "failed to sweep expired in-flight tool calls after request deadline"
        );
    }
    output_result.context("persisting received provider audit after request deadline")?;
    Err(error)
}

async fn finish_inference_stream(
    drive: StreamDrive<'_>,
    mut processor: Processor<'_>,
    stream_error: Option<LoopStreamError>,
) -> Result<HandleRequestOutcome> {
    let StreamDrive {
        stream_writer,
        doc_id,
        request_deadline,
        max_turns_provenance,
        ..
    } = drive;
    if let Some(error) = stream_error {
        let _ = processor
            .persist_received_partial_turn("persist errored assistant turn")
            .await
            .map_err(processor_failure)?;
        let error_reason =
            stream_failure_reason(&error.to_string(), error.cause(), max_turns_provenance);
        if matches!(
            error.loop_verdict(),
            Some((gents_loop::error::LoopVerdict::BackendUnavailable, _))
        ) {
            return Ok(HandleRequestOutcome::BackendUnavailable(
                anyhow::Error::new(error).context("agent stream failed"),
            ));
        }
        return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
            error_reason
        )));
    }

    if processor.final_text.is_none() {
        processor
            .persist_received_partial_turn("persist unterminated assistant turn")
            .await
            .map_err(processor_failure)
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
                let _ = stream_writer.write_tokens(doc_id, remainder).await?;
                streamed_text.push_str(remainder);
            }
        }
    }

    if !terminal_response_has_visible_output(&streamed_text, final_text.as_deref()) {
        let error_reason = "agent stream completed without producing any visible response content";
        return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
            error_reason
        )));
    }

    if let Err(error) =
        ensure_request_deadline_open(request_deadline, "finalizing inference response")
    {
        processor
            .persist_received_partial_turn("persist deadline-expired assistant turn")
            .await
            .map_err(processor_failure)
            .context("persisting received provider audit after request deadline")?;
        return Err(error);
    }
    Ok(HandleRequestOutcome::Completed)
}
