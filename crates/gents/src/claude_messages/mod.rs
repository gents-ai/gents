//! The Claude Messages client: the SSE parser ([`sse`]), the open and 401 replay
//! path ([`send`]), the Bun-exact transport ([`transport`]) and the error text
//! of a rejected response ([`error`]). Public paths are those of the former
//! single module.

#[cfg(test)]
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use rig::completion::CompletionError;
use thiserror::Error;

use gents_claude::profile;

mod error;
mod route;
mod send;
mod sse;
mod transport;

#[cfg(test)]
use crate::claude_subscription::ClaudeStreamResponse;
pub(crate) use error::non_success_message;
#[cfg(test)]
pub(crate) use error::{body_prefix, non_success_error, NON_SUCCESS_BODY_PREFIX_BYTES};
#[cfg(test)]
use gents_loop::claude_messages_body::ClaudeAccountContext;
pub(crate) use route::ClaudeRoute;
pub(crate) use send::open_messages_at;
pub use send::stream_messages;
#[cfg(test)]
pub(crate) use send::stream_messages_at;
#[cfg(test)]
pub(crate) use sse::stream_sse_body;
#[cfg(test)]
use sse::stream_sse_body_observed;
pub use sse::{parse_messages_sse, parse_messages_sse_typed, MessagesSseState};

/// The Messages endpoint of Claude Code's first-party API, with the beta route every call uses.
/// It is also the replay issuer's destination, so one const names both.
pub const MESSAGES_URI: &str = "https://api.anthropic.com/v1/messages?beta=true";
pub(crate) const ANTHROPIC_VERSION: &str = profile::ANTHROPIC_VERSION;
pub(crate) const OAUTH_BETA: &str = gents_claude::betas::OAUTH;

pub use gents_loop::claude_messages_body::{
    build_messages_body, ADVERTISED_REASONING_EFFORTS_PARAM, CLAUDE_CODE_IDENTITY,
    SESSION_KEY_PARAM,
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

#[cfg(test)]
fn messages_sse_fixture_queue() -> &'static Mutex<VecDeque<String>> {
    static QUEUE: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Test-only: SSE bodies served instead of the network; cleared by
/// `lock_fixtures_for_test`. Compiled out of non-test builds so no linking
/// crate can bypass the network or the write gate.
#[cfg(test)]
pub(crate) fn install_messages_sse_fixtures(bodies: Vec<String>) {
    *messages_sse_fixture_queue()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = bodies.into_iter().collect();
}

#[cfg(test)]
fn take_messages_sse_fixture() -> Option<String> {
    messages_sse_fixture_queue()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .pop_front()
}

#[cfg(test)]
static FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Async tests hold the guard across awaits, so the lock is a tokio mutex.
#[cfg(test)]
pub(crate) async fn lock_fixtures_for_test() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = FIXTURE_LOCK.lock().await;
    install_messages_sse_fixtures(Vec::new());
    guard
}

/// For synchronous tests; panics if called inside a Tokio runtime.
#[cfg(test)]
pub(crate) fn blocking_lock_fixtures_for_test() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = FIXTURE_LOCK.blocking_lock();
    install_messages_sse_fixtures(Vec::new());
    guard
}

#[cfg(test)]
pub(crate) fn sse_fixture_text(text: &str) -> String {
    let text = serde_json::to_string(text).expect("escape");
    format!(
        "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
         event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{text}}}}}\n\n\
         event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n"
    )
}

#[cfg(test)]
pub(crate) fn sse_fixture_tool_use(id: &str, name: &str, partial_json: &str) -> String {
    let partial = serde_json::to_string(partial_json).expect("escape");
    format!(
        "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"{name}\",\"input\":{{}}}}}}\n\n\
         event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{partial}}}}}\n\n\
         event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}},\"usage\":{{\"input_tokens\":10,\"output_tokens\":5}}}}\n\n\
         event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    )
}

#[cfg(test)]
pub(crate) fn sse_fixture_final_text(text: &str) -> String {
    format!(
        "{}event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"input_tokens\":12,\"output_tokens\":3}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
        sse_fixture_text(text)
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod platform_wire_tests;

#[cfg(test)]
mod wire_tests;

#[cfg(test)]
mod sse_tests;
