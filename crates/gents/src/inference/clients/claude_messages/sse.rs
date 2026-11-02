//! The Messages SSE facade: the parser state ([`sse_state`]), the body reader
//! ([`sse_body`]) and the Amazon Bedrock event-stream adapter. The parent
//! module owns the transport, the send path, the error text and the types.

use bytes::Bytes;
use futures::StreamExt;
use gents_claude::bedrock::{self as bedrock_rules, Mapped};
use gents_claude::eventstream::{Decoder, EventStreamError};
use gents_loop::provider_stream::{guard_response, ProviderStreamProtocol};
use rig::completion::CompletionError;
use rig::http_client;

pub use super::sse_state::{parse_messages_sse, parse_messages_sse_typed, MessagesSseState};
pub(super) use super::sse_body::stream_sse_body_observed;
#[cfg(test)]
pub(crate) use super::sse_body::stream_sse_body;

/// A failure while reading Amazon Bedrock's event stream.
#[derive(Debug, thiserror::Error)]
enum EventStreamFailure {
    #[error("the Amazon Bedrock event stream is damaged: {0}")]
    Frame(#[from] EventStreamError),
    #[error("{0}")]
    Event(#[from] bedrock_rules::StreamError),
}

impl From<EventStreamFailure> for http_client::Error {
    fn from(error: EventStreamFailure) -> Self {
        Self::Instance(Box::new(error))
    }
}

/// Bedrock's event-stream body as the Messages SSE bytes the parser reads. A
/// modeled exception ends the stream after its `error` event, as the SDK's
/// reader does; a damaged frame or a stream cut inside a frame is an error.
fn event_stream_to_sse(body: http_client::sse::BoxedStream) -> http_client::sse::BoxedStream {
    Box::pin(async_stream::stream! {
        let mut body = body;
        let mut decoder = Decoder::new();
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };
            let frames = match decoder.push(&chunk) {
                Ok(frames) => frames,
                Err(error) => {
                    yield Err(EventStreamFailure::from(error).into());
                    return;
                }
            };
            for frame in frames {
                match bedrock_rules::map_frame(&frame) {
                    Ok(Mapped::Event(text)) => yield Ok(Bytes::from(text)),
                    Ok(Mapped::Error(text)) => {
                        yield Ok(Bytes::from(text));
                        return;
                    }
                    Ok(Mapped::Ignored) => {}
                    Err(error) => {
                        yield Err(EventStreamFailure::from(error).into());
                        return;
                    }
                }
            }
        }
        if let Err(error) = decoder.finish() {
            yield Err(EventStreamFailure::from(error).into());
        }
    })
}

/// The successful response of the Bedrock runtime as a Messages SSE response.
/// A content type that is not an event stream is refused; a missing one is
/// taken as an event stream (Claude Code does the same). Raw EOF without
/// `message_stop` is refused like on every Messages route.
pub(super) fn bedrock_response(
    response: http_client::StreamingResponse,
) -> Result<http_client::StreamingResponse, CompletionError> {
    if let Some(content_type) = response.headers().get("content-type") {
        let text = content_type.to_str().unwrap_or("");
        if !text.contains(bedrock_rules::EVENT_STREAM_CONTENT_TYPE) {
            return Err(CompletionError::ProviderError(format!(
                "Amazon Bedrock answered with content type {text:?}, not an event stream"
            )));
        }
    }
    let (parts, body) = response.into_parts();
    Ok(guard_response(
        http_client::Response::from_parts(parts, event_stream_to_sse(body)),
        Some(ProviderStreamProtocol::Anthropic),
        None,
    ))
}
