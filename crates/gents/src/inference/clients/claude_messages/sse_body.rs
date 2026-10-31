use futures::StreamExt;
use rig::completion::CompletionError;
use rig::http_client;
use rig::streaming::RawStreamingChoice;

use super::sse_state::MessagesSseState;
use crate::inference::clients::claude_subscription::ClaudeStreamResponse;
use gents_loop::provider_audit::ProviderAuditSender;

/// Incremental line-splitter over a response body.
#[cfg(test)]
pub(crate) fn stream_sse_body(
    body: http_client::sse::BoxedStream,
    state: MessagesSseState,
) -> impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>
{
    stream_sse_body_observed(body, state, None)
}

pub(super) fn stream_sse_body_observed(
    body: http_client::sse::BoxedStream,
    state: MessagesSseState,
    audit_sender: Option<ProviderAuditSender>,
) -> impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>
{
    async_stream::stream! {
        let mut body = body;
        let mut state = state;
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(CompletionError::ProviderError(format!("Claude Messages body: {error}")));
                    return;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = buffer.drain(..=newline).collect();
                line.pop();
                let line = match String::from_utf8(line) {
                    Ok(line) => line,
                    Err(_) => {
                        yield Err(invalid_sse_utf8());
                        return;
                    }
                };
                match push_line_observed(&mut state, &line, audit_sender.as_ref()).await {
                    Ok(events) => {
                        for event in events {
                            yield Ok(event);
                        }
                    }
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                }
            }
        }
        if !buffer.is_empty() {
            let line = match String::from_utf8(buffer) {
                Ok(line) => line,
                Err(_) => {
                    yield Err(invalid_sse_utf8());
                    return;
                }
            };
            match push_line_observed(&mut state, &line, audit_sender.as_ref()).await {
                Ok(events) => for event in events { yield Ok(event); },
                Err(error) => { yield Err(error); return; }
            }
        }
        let mut reservation = match audit_sender.as_ref() {
            Some(sender) => match sender.reserve().await {
                Ok(reservation) => Some(reservation),
                Err(_) => {
                    yield Err(audit_queue_closed());
                    return;
                }
            },
            None => None,
        };
        let (finished, audit) = state.finish_observed();
        let mut audit_overflow = false;
        for event in audit {
            if let Some(reservation) = reservation.as_mut() {
                if reservation.emit(event).is_err() {
                    audit_overflow = true;
                    break;
                }
            }
        }
        drop(reservation);
        if audit_overflow {
            yield Err(audit_event_overflow());
            return;
        }
        match finished {
            Ok(events) => for event in events { yield Ok(event); },
            Err(error) => yield Err(error),
        }
    }
}

fn audit_queue_closed() -> CompletionError {
    CompletionError::ProviderError(
        "Claude audit receiver closed before provider stream ended".into(),
    )
}

fn invalid_sse_utf8() -> CompletionError {
    CompletionError::ProviderError("Claude Messages SSE contains invalid UTF-8".into())
}

fn audit_event_overflow() -> CompletionError {
    CompletionError::ProviderError("Claude SSE event exceeded reserved audit capacity".into())
}

async fn push_line_observed(
    state: &mut MessagesSseState,
    line: &str,
    audit_sender: Option<&ProviderAuditSender>,
) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, CompletionError> {
    let dispatches = line.strip_suffix('\r').unwrap_or(line).is_empty();
    let mut reservation = match (dispatches, audit_sender) {
        (true, Some(sender)) => Some(sender.reserve().await.map_err(|_| audit_queue_closed())?),
        _ => None,
    };
    let parsed = state.push_line(line);
    for event in state.take_audit_events() {
        if let Some(reservation) = reservation.as_mut() {
            reservation
                .emit(event)
                .map_err(|_| audit_event_overflow())?;
        } else if audit_sender.is_some() {
            return Err(audit_event_overflow());
        }
    }
    parsed
}
