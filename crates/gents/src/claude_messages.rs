use rapidhash::{HashSetExt, RapidHashSet};
#[cfg(test)]
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use bytes::Bytes;
use futures::StreamExt;
use gents_protocol::message::ReasoningContent;
use rig::completion::{CompletionError, CompletionRequest};
use rig::http_client::{
    self, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient, Response,
    StreamingResponse,
};
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall};
use rig::wasm_compat::WasmCompatSend;
use serde_json::Value;
use thiserror::Error;

use crate::claude_subscription::ClaudeStreamResponse;
use crate::oauth_credential::BearerSource;
use crate::rendered_request::RenderedRequestCapturingHttpClient;
use gents_loop::provider_audit::{ClaudeAuditEvent, ClaudeBlockKind, ProviderAuditSender};

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

/// Incremental SSE parser for one Messages response.
///
/// Feed lines with [`push_line`]; each completed event (terminated by a blank
/// line) may yield zero or more `RawStreamingChoice`s. [`finish`] flushes an
/// unterminated `tool_use` block and guarantees exactly one `FinalResponse`.
/// Lean: `ClaudeMap.runStream` (`step` / `flush`).
///
/// [`push_line`]: MessagesSseState::push_line
/// [`finish`]: MessagesSseState::finish
pub struct MessagesSseState {
    surface: RapidHashSet<String>,
    pending: Option<PendingBlock>,
    last_reasoning_index: Option<u64>,
    seen_ids: RapidHashSet<String>,
    usage: Option<rig::completion::Usage>,
    usage_fields: serde_json::Map<String, Value>,
    data: String,
    finished: bool,
    /// `request-id` response header, carried into stream-error messages only.
    request_id: Option<String>,
    audit_open: Option<u32>,
    audit_events: Vec<ClaudeAuditEvent>,
}

impl MessagesSseState {
    pub fn new(surface: RapidHashSet<String>) -> Self {
        Self {
            surface,
            pending: None,
            last_reasoning_index: None,
            seen_ids: RapidHashSet::new(),
            usage: None,
            usage_fields: serde_json::Map::new(),
            data: String::new(),
            finished: false,
            request_id: None,
            audit_open: None,
            audit_events: Vec::new(),
        }
    }

    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        self.request_id = request_id;
        self
    }

    pub fn take_audit_events(&mut self) -> Vec<ClaudeAuditEvent> {
        std::mem::take(&mut self.audit_events)
    }

    /// One SSE line without its trailing newline; a blank line dispatches the
    /// accumulated payload.
    pub fn push_line(
        &mut self,
        line: &str,
    ) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, CompletionError> {
        self.push_line_typed(line).map_err(Into::into)
    }

    /// The same parser before its Rig-facing error conversion. Conformance
    /// checks the exact fail-closed cause at this owner boundary.
    pub fn push_line_typed(
        &mut self,
        line: &str,
    ) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, MessagesParseError> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(rest.trim());
            return Ok(Vec::new());
        }
        if !line.is_empty() {
            // `event:`, `id:`, comments — the payload's own `type` is authoritative.
            return Ok(Vec::new());
        }
        self.dispatch_pending_data()
    }

    /// End of body: flush an open block and emit `FinalResponse` if none seen.
    pub fn finish(self) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, CompletionError> {
        self.finish_typed().map_err(Into::into)
    }

    /// End of body with the parser's typed cause retained.
    pub fn finish_typed(
        mut self,
    ) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, MessagesParseError> {
        self.finish_typed_inner()
    }

    fn finish_typed_inner(
        &mut self,
    ) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, MessagesParseError> {
        let mut events = self.dispatch_pending_data()?;
        self.close_at_eof(&mut events)?;
        if !self.finished {
            self.finished = true;
            events.push(RawStreamingChoice::FinalResponse(ClaudeStreamResponse {
                usage: self.usage.take(),
                token_usage: messages_token_usage(&self.usage_fields, false)?,
            }));
        }
        Ok(events)
    }

    fn finish_observed(
        mut self,
    ) -> (
        Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, CompletionError>,
        Vec<ClaudeAuditEvent>,
    ) {
        let result = self.finish_typed_inner().map_err(Into::into);
        (result, self.take_audit_events())
    }

    fn dispatch_pending_data(
        &mut self,
    ) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, MessagesParseError> {
        if self.data.is_empty() {
            return Ok(Vec::new());
        }
        let raw = std::mem::take(&mut self.data);
        let payload: Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => {
                tracing::debug!(
                    len = raw.len(),
                    "claude messages: ignoring non-JSON SSE payload"
                );
                return Ok(Vec::new());
            }
        };
        self.handle_payload(&payload)
    }

    fn observe_usage(&mut self, value: &Value) -> Result<(), MessagesParseError> {
        for field in [
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ] {
            if let Some(value) = value.get(field).filter(|value| !value.is_null()) {
                if value.as_u64().is_none() {
                    return Err(MessagesParseError::MalformedUsage { field });
                }
                self.usage_fields.insert(field.to_string(), value.clone());
            }
        }
        let usage = messages_token_usage(&self.usage_fields, false)?;
        self.usage = Some(rig::completion::Usage {
            input_tokens: usage.input_tokens.unwrap_or_default(),
            output_tokens: usage.output_tokens.unwrap_or_default(),
            total_tokens: usage.input_tokens.unwrap_or_default()
                + usage.output_tokens.unwrap_or_default(),
            cached_input_tokens: usage.cached_input_tokens.unwrap_or_default(),
            cache_creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or_default(),
        });
        Ok(())
    }

    fn close_at_eof(
        &mut self,
        events: &mut Vec<RawStreamingChoice<ClaudeStreamResponse>>,
    ) -> Result<(), MessagesParseError> {
        match self.pending.take() {
            Some(PendingBlock::Tool(tool)) => {
                events.push(mapped_tool_call(tool, &self.surface, &mut self.seen_ids)?);
                Ok(())
            }
            Some(PendingBlock::Thinking { .. } | PendingBlock::Redacted { .. }) => {
                Err(malformed_thinking(ThinkingParseCause::IncompleteBlock))
            }
            None => Ok(()),
        }
    }

    fn handle_payload(
        &mut self,
        payload: &Value,
    ) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, MessagesParseError> {
        let mut events = Vec::new();
        let Some(kind) = payload.get("type").and_then(Value::as_str) else {
            return Ok(events);
        };
        match kind {
            "message_start" => {
                if let Some(usage) = payload
                    .get("message")
                    .and_then(|message| message.get("usage"))
                {
                    self.observe_usage(usage)?;
                }
            }
            "content_block_start" => {
                let Some(block) = payload.get("content_block") else {
                    return Ok(events);
                };
                if self.audit_open.is_some() {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        return Err(MessagesParseError::OverlappingToolUse {
                            id: block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        });
                    }
                    return Err(malformed_thinking(ThinkingParseCause::WrongBlock));
                }
                let block_index = match block.get("type").and_then(Value::as_str) {
                    Some("text" | "tool_use" | "thinking" | "redacted_thinking") => {
                        Some(audit_index(payload)?)
                    }
                    _ => None,
                };
                let audit_start_at = self.audit_events.len();
                let mut audit_kind = None;
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        audit_kind = Some(ClaudeBlockKind::Text);
                    }
                    Some("tool_use") => {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if self.pending.is_some() {
                            return Err(MessagesParseError::OverlappingToolUse { id });
                        }
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let start_input = match block.get("input") {
                            Some(Value::Object(_)) => Some(block["input"].to_string()),
                            _ => None,
                        };
                        self.pending = Some(PendingBlock::Tool(PendingTool {
                            id: id.clone(),
                            name: name.clone(),
                            start_input,
                            deltas: String::new(),
                        }));
                        audit_kind = Some(ClaudeBlockKind::ToolUse { id, name });
                    }
                    Some("thinking" | "redacted_thinking") => {
                        if self.pending.is_some() {
                            return Err(malformed_thinking(ThinkingParseCause::WrongBlock));
                        }
                        let index = reasoning_index(payload)?;
                        if self.last_reasoning_index.is_some_and(|last| index <= last) {
                            return Err(malformed_thinking(ThinkingParseCause::WrongIndex(index)));
                        }
                        if block["type"] == "thinking" {
                            let text =
                                block
                                    .get("thinking")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| {
                                        malformed_thinking(ThinkingParseCause::Malformed(
                                            "thinking start has no text",
                                        ))
                                    })?;
                            let initial_signature = match block.get("signature") {
                                None => String::new(),
                                Some(Value::String(signature)) => signature.clone(),
                                Some(_) => {
                                    return Err(malformed_thinking(ThinkingParseCause::Malformed(
                                        "thinking start signature is not text",
                                    )));
                                }
                            };
                            self.pending = Some(PendingBlock::Thinking {
                                index,
                                text: text.to_owned(),
                                signature_started: !initial_signature.is_empty(),
                                signature: initial_signature.clone(),
                            });
                            if !text.is_empty() {
                                self.audit_events.push(ClaudeAuditEvent::ThinkingText {
                                    index: block_index.expect("recognized block has an index"),
                                    fragment: text.to_owned(),
                                });
                                events.push(RawStreamingChoice::ReasoningDelta {
                                    id: None,
                                    reasoning: text.to_owned(),
                                });
                            }
                            if !initial_signature.is_empty() {
                                self.audit_events.push(ClaudeAuditEvent::Signature {
                                    index: block_index.expect("recognized block has an index"),
                                    fragment: initial_signature,
                                });
                            }
                            audit_kind = Some(ClaudeBlockKind::Thinking);
                        } else {
                            let data = block
                                .get("data")
                                .and_then(Value::as_str)
                                .filter(|data| !data.is_empty())
                                .ok_or_else(|| {
                                    malformed_thinking(ThinkingParseCause::Malformed(
                                        "redacted block has no data",
                                    ))
                                })?;
                            self.pending = Some(PendingBlock::Redacted {
                                index,
                                data: data.to_owned(),
                            });
                            self.audit_events.push(ClaudeAuditEvent::RedactedData {
                                index: block_index.expect("recognized block has an index"),
                                data: data.to_owned(),
                            });
                            audit_kind = Some(ClaudeBlockKind::RedactedThinking);
                        }
                    }
                    _ => {}
                }
                if let Some(kind) = audit_kind {
                    let index = block_index.expect("recognized block has an index");
                    self.audit_open = Some(index);
                    self.audit_events
                        .insert(audit_start_at, ClaudeAuditEvent::BlockStart { index, kind });
                }
            }
            "content_block_delta" => {
                let Some(delta) = payload.get("delta") else {
                    return Ok(events);
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if self.pending.is_some() {
                            return Err(malformed_thinking(ThinkingParseCause::WrongBlock));
                        }
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                events.push(RawStreamingChoice::Message(text.to_string()));
                            }
                        }
                    }
                    Some("input_json_delta") => {
                        if let (Some(PendingBlock::Tool(tool)), Some(partial)) = (
                            self.pending.as_mut(),
                            delta.get("partial_json").and_then(Value::as_str),
                        ) {
                            tool.deltas.push_str(partial);
                        } else if self.pending.is_some() {
                            return Err(malformed_thinking(ThinkingParseCause::WrongBlock));
                        }
                    }
                    Some("thinking_delta") => {
                        let index = reasoning_index(payload)?;
                        match self.pending.as_mut() {
                            Some(PendingBlock::Thinking {
                                index: current,
                                text,
                                signature_started,
                                ..
                            }) if *current == index && !*signature_started => {
                                let fragment = delta
                                    .get("thinking")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| {
                                        malformed_thinking(ThinkingParseCause::Malformed(
                                            "thinking delta has no text",
                                        ))
                                    })?;
                                text.push_str(fragment);
                                if !fragment.is_empty() {
                                    self.audit_events.push(ClaudeAuditEvent::ThinkingText {
                                        index: audit_index(payload)?,
                                        fragment: fragment.to_owned(),
                                    });
                                    events.push(RawStreamingChoice::ReasoningDelta {
                                        id: None,
                                        reasoning: fragment.to_owned(),
                                    });
                                }
                            }
                            Some(PendingBlock::Thinking {
                                index: current,
                                signature_started: true,
                                ..
                            }) if *current == index => {
                                return Err(malformed_thinking(ThinkingParseCause::SignatureOrder));
                            }
                            Some(PendingBlock::Thinking { .. }) => {
                                return Err(malformed_thinking(ThinkingParseCause::WrongIndex(
                                    index,
                                )));
                            }
                            _ => return Err(malformed_thinking(ThinkingParseCause::WrongBlock)),
                        }
                    }
                    Some("signature_delta") => {
                        let index = reasoning_index(payload)?;
                        match self.pending.as_mut() {
                            Some(PendingBlock::Thinking {
                                index: current,
                                signature,
                                signature_started,
                                ..
                            }) if *current == index => {
                                let fragment = delta
                                    .get("signature")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| {
                                        malformed_thinking(ThinkingParseCause::Malformed(
                                            "signature delta has no signature",
                                        ))
                                    })?;
                                signature.push_str(fragment);
                                *signature_started = true;
                                self.audit_events.push(ClaudeAuditEvent::Signature {
                                    index: audit_index(payload)?,
                                    fragment: fragment.to_owned(),
                                });
                            }
                            Some(PendingBlock::Thinking { .. }) => {
                                return Err(malformed_thinking(ThinkingParseCause::WrongIndex(
                                    index,
                                )));
                            }
                            _ => return Err(malformed_thinking(ThinkingParseCause::WrongBlock)),
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let audit_index = audit_index(payload)?;
                if self.audit_open != Some(audit_index) {
                    return Err(malformed_thinking(ThinkingParseCause::WrongIndex(
                        u64::from(audit_index),
                    )));
                }
                match self.pending.take() {
                    Some(PendingBlock::Tool(tool)) => {
                        events.push(mapped_tool_call(tool, &self.surface, &mut self.seen_ids)?);
                    }
                    Some(PendingBlock::Thinking {
                        index,
                        text,
                        signature,
                        ..
                    }) => {
                        let observed_index = reasoning_index(payload)?;
                        if observed_index != index {
                            return Err(malformed_thinking(ThinkingParseCause::WrongIndex(
                                observed_index,
                            )));
                        }
                        if signature.is_empty() {
                            return Err(malformed_thinking(ThinkingParseCause::MissingSignature));
                        }
                        self.last_reasoning_index = Some(index);
                        events.push(RawStreamingChoice::Reasoning {
                            id: None,
                            content: crate::llm::rig_compat::to_rig_reasoning_part(
                                &ReasoningContent::Text {
                                    text,
                                    signature: Some(signature),
                                },
                            ),
                        });
                    }
                    Some(PendingBlock::Redacted { index, data }) => {
                        let observed_index = reasoning_index(payload)?;
                        if observed_index != index {
                            return Err(malformed_thinking(ThinkingParseCause::WrongIndex(
                                observed_index,
                            )));
                        }
                        self.last_reasoning_index = Some(index);
                        events.push(RawStreamingChoice::Reasoning {
                            id: None,
                            content: crate::llm::rig_compat::to_rig_reasoning_part(
                                &ReasoningContent::Redacted { data },
                            ),
                        });
                    }
                    None => {}
                }
                self.audit_open = None;
                self.audit_events
                    .push(ClaudeAuditEvent::BlockStop { index: audit_index });
            }
            "message_delta" => {
                // A `refusal` stop_reason is the model declining the request.
                // The turn carries no usable content, so without this the owned
                // loop would retract an empty turn and resample until its
                // transport budget ran out, reporting an opaque "no visible
                // output" failure. Surface it as a distinct, permanent error.
                if payload
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                    == Some("refusal")
                {
                    let request_id = self.request_id.as_deref().unwrap_or("-").to_owned();
                    return Err(MessagesParseError::ProviderRefusal { request_id });
                }
                if let Some(value) = payload.get("usage") {
                    self.observe_usage(value)?;
                }
            }
            "message_stop" => {
                // A malformed stream may end without `content_block_stop`;
                // the open block still precedes the final response.
                self.close_at_eof(&mut events)?;
                if !self.finished {
                    self.finished = true;
                    events.push(RawStreamingChoice::FinalResponse(ClaudeStreamResponse {
                        usage: self.usage,
                        token_usage: messages_token_usage(&self.usage_fields, true)?,
                    }));
                }
            }
            "error" => {
                let error = payload.get("error").cloned().unwrap_or(Value::Null);
                let error_type = error.get("type").and_then(Value::as_str).unwrap_or("error");
                let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                let request_id = self.request_id.as_deref().unwrap_or("-");
                return Err(MessagesParseError::ProviderStreamError {
                    error_type: error_type.to_owned(),
                    message: message.to_owned(),
                    request_id: request_id.to_owned(),
                });
            }
            _ => {}
        }
        Ok(events)
    }
}

pub fn parse_messages_sse(
    sse: &str,
    surface: &RapidHashSet<String>,
) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, CompletionError> {
    parse_messages_sse_typed(sse, surface).map_err(Into::into)
}

pub fn parse_messages_sse_typed(
    sse: &str,
    surface: &RapidHashSet<String>,
) -> Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, MessagesParseError> {
    let mut state = MessagesSseState::new(surface.clone());
    let mut events = Vec::new();
    for line in sse.lines() {
        events.extend(state.push_line_typed(line)?);
        state.take_audit_events();
    }
    events.extend(state.finish_typed()?);
    Ok(events)
}

enum PendingBlock {
    Tool(PendingTool),
    Thinking {
        index: u64,
        text: String,
        signature: String,
        signature_started: bool,
    },
    Redacted {
        index: u64,
        data: String,
    },
}

fn malformed_thinking(cause: ThinkingParseCause) -> MessagesParseError {
    MessagesParseError::MalformedThinking { cause }
}

fn reasoning_index(payload: &Value) -> Result<u64, MessagesParseError> {
    payload
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed_thinking(ThinkingParseCause::Malformed("missing content index")))
}

fn audit_index(payload: &Value) -> Result<u32, MessagesParseError> {
    let index = reasoning_index(payload)?;
    u32::try_from(index).map_err(|_| malformed_thinking(ThinkingParseCause::WrongIndex(index)))
}

struct PendingTool {
    id: String,
    name: String,
    /// `content_block.input` from `content_block_start`, serialized. Anthropic
    /// sends `{}` here and streams the real arguments as deltas.
    start_input: Option<String>,
    /// Concatenated `input_json_delta.partial_json` fragments, in order.
    deltas: String,
}

impl PendingTool {
    /// Lean `ClaudeMap.accumulate`: deltas win when any arrived; otherwise the
    /// start input; otherwise `{}`.
    fn arguments_json(&self) -> String {
        if !self.deltas.is_empty() {
            self.deltas.clone()
        } else {
            self.start_input.clone().unwrap_or_else(|| "{}".to_string())
        }
    }
}

fn mapped_tool_call(
    tool: PendingTool,
    surface: &RapidHashSet<String>,
    seen_ids: &mut RapidHashSet<String>,
) -> Result<RawStreamingChoice<ClaudeStreamResponse>, MessagesParseError> {
    if tool.id.trim().is_empty() || tool.name.trim().is_empty() {
        return Err(MessagesParseError::MalformedToolUse {
            message: "missing id or name".to_string(),
        });
    }
    if !seen_ids.insert(tool.id.clone()) {
        return Err(MessagesParseError::DuplicateToolUseId { id: tool.id });
    }
    if surface.is_empty() || !surface.contains(&tool.name) {
        return Err(MessagesParseError::ToolUse { names: tool.name });
    }
    let raw = tool.arguments_json();
    let input: Value =
        serde_json::from_str(&raw).map_err(|error| MessagesParseError::MalformedToolUse {
            message: format!("tool_use {} input is not JSON: {error}", tool.id),
        })?;
    Ok(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
        tool.id, tool.name, input,
    )))
}

/// Messages usage deltas are cumulative and may omit the initial input fields.
/// Input excludes cache reads/writes on this wire, so normalize their sum.
/// https://platform.claude.com/docs/en/build-with-claude/streaming
fn messages_token_usage(
    fields: &serde_json::Map<String, Value>,
    final_response: bool,
) -> Result<gents_protocol::usage::TokenUsage, MessagesParseError> {
    let value = |name: &str| fields.get(name).and_then(Value::as_u64);
    let cache_read = value("cache_read_input_tokens");
    let cache_write = value("cache_creation_input_tokens");
    let input_tokens = value("input_tokens")
        .map(|input| {
            input
                .checked_add(cache_read.unwrap_or_default())
                .and_then(|input| input.checked_add(cache_write.unwrap_or_default()))
                .ok_or(MessagesParseError::MalformedUsage {
                    field: "input_tokens",
                })
        })
        .transpose()?;
    let observation = gents_protocol::usage::TokenUsage {
        input_tokens,
        output_tokens: value("output_tokens"),
        cached_input_tokens: cache_read,
        cache_creation_input_tokens: cache_write,
        final_response,
        ..Default::default()
    };
    observation
        .validate()
        .map_err(|_| MessagesParseError::MalformedUsage { field: "usage" })?;
    Ok(observation)
}

/// Incremental line-splitter over a response body.
#[cfg(test)]
pub(crate) fn stream_sse_body(
    body: http_client::sse::BoxedStream,
    state: MessagesSseState,
) -> impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>
{
    stream_sse_body_observed(body, state, None)
}

fn stream_sse_body_observed(
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

pub async fn stream_messages<S>(
    model: &str,
    request: &CompletionRequest,
    surface: RapidHashSet<String>,
    bearer: &S,
    http: &ReqwestClient,
    usage: Option<Arc<crate::usage_observation::UsageReporter>>,
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
    usage: Option<Arc<crate::usage_observation::UsageReporter>>,
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
                crate::provider_http::ProviderHttpClient::with_usage(http.clone(), usage)
            }
            None => crate::provider_http::ProviderHttpClient::new(http.clone()),
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

#[derive(Clone)]
struct MessagesTransport {
    fixture: Option<String>,
    live: crate::provider_http::ProviderHttpClient,
}

impl HttpClientExt for MessagesTransport {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>>
           + WasmCompatSend
           + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.live.clone();
        let fixture = self.fixture.clone();
        let (parts, body) = req.into_parts();
        let body: Bytes = body.into();
        async move {
            if fixture.is_some() {
                let body: LazyBody<U> = Box::pin(async { Ok(U::from(Bytes::from_static(b"{}"))) });
                return Ok(Response::builder().status(200).body(body)?);
            }
            let req = Request::from_parts(parts, body);
            HttpClientExt::send::<Bytes, U>(&inner, req).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>>
           + WasmCompatSend
           + 'static
    where
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.live.clone();
        async move { HttpClientExt::send_multipart::<U>(&inner, req).await }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl std::future::Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let inner = self.live.clone();
        let fixture = self.fixture.clone();
        let (parts, body) = req.into_parts();
        let body: Bytes = body.into();
        async move {
            if let Some(sse) = fixture {
                let stream: http_client::sse::BoxedStream =
                    Box::pin(futures::stream::iter([Ok(Bytes::from(sse))]));
                return Ok(Response::builder().status(200).body(stream)?);
            }
            let req = Request::from_parts(parts, body);
            HttpClientExt::send_streaming(&inner, req).await
        }
    }
}

impl fmt::Debug for MessagesTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessagesTransport")
            .field("fixture", &self.fixture.is_some())
            .finish_non_exhaustive()
    }
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
#[path = "claude_messages/tests.rs"]
mod tests;
