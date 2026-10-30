use rapidhash::RapidHashSet;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use gents_claude::bedrock::{self as bedrock_rules, Mapped};
use gents_claude::eventstream::{Decoder, EventStreamError};
use gents_loop::provider_stream::{guard_response, ProviderStreamProtocol};
use rig::completion::{CompletionError, CompletionRequest};
use rig::http_client::{self, HeaderValue, HttpClientExt, Request, ReqwestClient};
use rig::streaming::RawStreamingChoice;
use thiserror::Error;

use crate::inference::auth::credential::BearerSource;
use crate::inference::clients::claude_subscription::ClaudeStreamResponse;
use crate::inference::rendered_request::RenderedRequestCapturingHttpClient;

#[cfg(test)]
pub(crate) mod fixtures;
mod sse_body;
mod sse_state;
mod transport;

#[cfg(test)]
use fixtures::take_messages_sse_fixture;
use sse_body::stream_sse_body_observed;
pub use sse_state::{parse_messages_sse, parse_messages_sse_typed, MessagesSseState};
use transport::MessagesTransport;

pub const MESSAGES_URI: &str = "https://api.anthropic.com/v1/messages";
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";
pub(crate) const OAUTH_BETA: &str = "oauth-2025-04-20";

pub use gents_loop::claude_messages_body::{
    apply_reasoning_parameters, build_messages_body, build_messages_body_native,
    ADVERTISED_REASONING_EFFORTS_PARAM, CLAUDE_CODE_IDENTITY,
};

#[derive(Debug, Error, PartialEq, Eq)]
/// Fail-closed causes for malformed native thinking blocks, distinguishing
/// indexed and block-shape errors without parsing display text.
pub enum ThinkingParseCause {
    #[error("wrong content block")]
    WrongBlock,
    #[error("wrong content index {0}")]
    WrongIndex(u64),
    #[error("incomplete content block")]
    IncompleteBlock,
    #[error("thinking after signature")]
    SignatureOrder,
    #[error("thinking block has no signature")]
    MissingSignature,
    #[error("{0}")]
    Malformed(&'static str),
}

#[derive(Debug, Error, PartialEq, Eq)]
/// Fail-closed outcomes of the Messages tool-block parser.
pub enum MessagesParseError {
    #[error("fail-closed: tool_use observed ({names})")]
    ToolUse { names: String },
    #[error("fail-closed: duplicate tool_use id {id}")]
    DuplicateToolUseId { id: String },
    #[error("fail-closed: malformed tool_use: {message}")]
    MalformedToolUse { message: String },
    #[error("fail-closed: overlapping tool_use block {id}")]
    OverlappingToolUse { id: String },
    #[error("fail-closed: malformed thinking: {cause}")]
    MalformedThinking { cause: ThinkingParseCause },
    #[error("Claude Messages stream error {error_type}: {message} (request-id {request_id})")]
    ProviderStreamError {
        error_type: String,
        message: String,
        request_id: String,
    },
    /// The model declined the request: Anthropic closes the turn with
    /// `message_delta.stop_reason == "refusal"` and no usable content block.
    /// Resending the identical body earns the identical refusal, so this is a
    /// permanent, non-retryable failure; Rig carries it to the loop only as this
    /// display, which `gents_loop::error::ProviderReport` reads. Surfacing it here keeps the owned loop from mistaking a
    /// content-free refusal for a transient "no visible output" turn and
    /// resampling it until the transport budget is exhausted.
    #[error("the model declined to respond (stop_reason: refusal) (request-id {request_id})")]
    ProviderRefusal { request_id: String },
    #[error("Claude Messages returned an invalid usage counter: {field}")]
    MalformedUsage { field: &'static str },
}

impl From<MessagesParseError> for CompletionError {
    fn from(error: MessagesParseError) -> Self {
        match error {
            error @ MessagesParseError::MalformedThinking { .. } => {
                // Parsed provider bytes, including EOF truncation, are not a
                // malformed local request. Keep this on the transient lane.
                CompletionError::ResponseError(error.to_string())
            }
            other => CompletionError::ProviderError(other.to_string()),
        }
    }
}
pub async fn stream_messages<S>(
    model: &str,
    request: &CompletionRequest,
    surface: RapidHashSet<String>,
    bearer: &S,
    http: &ReqwestClient,
    usage: Option<Arc<crate::inference::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    CompletionError,
>
where
    S: BearerSource,
{
    stream_messages_at(MESSAGES_URI, model, request, surface, bearer, http, usage).await
}

pub(crate) async fn stream_messages_at<S>(
    uri: &str,
    model: &str,
    request: &CompletionRequest,
    surface: RapidHashSet<String>,
    bearer: &S,
    http: &ReqwestClient,
    usage: Option<Arc<crate::inference::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    CompletionError,
>
where
    S: BearerSource,
{
    #[cfg(test)]
    let fixture = take_messages_sse_fixture();
    #[cfg(not(test))]
    let fixture: Option<String> = None;
    let body = build_messages_body(model, request).map_err(|error| {
        CompletionError::RequestError(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Claude Messages body: {error:#}"),
        )))
    })?;
    let body_bytes = serde_json::to_vec(&body).map_err(|error| {
        CompletionError::RequestError(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("encode Claude Messages body: {error}"),
        )))
    })?;

    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("anthropic-version", ANTHROPIC_VERSION);
    if bearer.is_oauth() {
        builder = builder.header("anthropic-beta", OAUTH_BETA);
    }
    if fixture.is_none() {
        let token = bearer.current_bearer().await.map_err(|error| {
            CompletionError::ProviderError(format!("Claude Messages bearer: {error}"))
        })?;
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|error| {
                CompletionError::ProviderError(format!("Claude Messages auth header: {error}"))
            })?;
        authorization.set_sensitive(true);
        builder = builder.header("authorization", authorization);
        tracing::info!(
            model = %model,
            oauth = bearer.is_oauth(),
            "live Claude Messages HTTP send"
        );
    }
    let http_request = builder.body(Bytes::from(body_bytes)).map_err(|error| {
        CompletionError::ProviderError(format!("Claude Messages request: {error}"))
    })?;

    let client = RenderedRequestCapturingHttpClient::new(MessagesTransport {
        fixture,
        live: match usage {
            Some(usage) => {
                crate::inference::clients::provider_http::ProviderHttpClient::with_usage(
                    http.clone(),
                    usage,
                )
            }
            None => crate::inference::clients::provider_http::ProviderHttpClient::new(http.clone()),
        },
    });
    let response = match client.send_streaming(http_request).await {
        Ok(response) => response,
        Err(http_client::Error::InvalidStatusCodeWithMessage(status, message)) => {
            if status.as_u16() == 401 {
                bearer.invalidate().await;
            }
            let (body, marker) = gents_loop::provider_limit::split_annotated_body(&message);
            return Err(non_success_error(
                status,
                None,
                &format!("{}{marker}", body_prefix(body.as_bytes())),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let status = response.status();
    let request_id = response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if !status.is_success() {
        let prefix = read_body_prefix(response.into_body()).await;
        return Err(non_success_error(status, request_id.as_deref(), &prefix));
    }
    let audit_sender = gents_loop::rendered_request::scope::current_audit_sender();
    if gents_loop::rendered_request::scope::current_scope().is_some() && audit_sender.is_none() {
        return Err(CompletionError::ProviderError(
            "Claude Messages response has no exact provider audit attempt".into(),
        ));
    }
    Ok(stream_sse_body_observed(
        response.into_body(),
        MessagesSseState::new(surface).with_request_id(request_id),
        audit_sender,
    ))
}

const NON_SUCCESS_BODY_PREFIX_BYTES: usize = 512;

/// Non-2xx Messages response as a `ProviderError`. Carries the status, the
/// `request-id` header and a bounded body prefix (Anthropic's
/// `error.type`/`error.message`); never request headers.
fn non_success_error(
    status: reqwest::StatusCode,
    request_id: Option<&str>,
    body_prefix: &str,
) -> CompletionError {
    let mut message = format!(
        "Claude Messages HTTP {status} (request-id {})",
        request_id.unwrap_or("-")
    );
    if !body_prefix.is_empty() {
        message.push_str(" body=");
        message.push_str(body_prefix);
    }
    CompletionError::ProviderError(message)
}

fn body_prefix(bytes: &[u8]) -> String {
    let end = bytes.len().min(NON_SUCCESS_BODY_PREFIX_BYTES);
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

async fn read_body_prefix(mut body: http_client::sse::BoxedStream) -> String {
    let mut bytes: Vec<u8> = Vec::new();
    while bytes.len() < NON_SUCCESS_BODY_PREFIX_BYTES {
        match body.next().await {
            Some(Ok(chunk)) => bytes.extend_from_slice(&chunk),
            Some(Err(_)) | None => break,
        }
    }
    body_prefix(&bytes)
}
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

#[cfg(test)]
mod tests;
