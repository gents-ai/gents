use rapidhash::{HashSetExt, RapidHashSet};

use gents_protocol::message::ReasoningContent;
use rig::completion::CompletionError;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall};
use serde_json::Value;

use super::{MessagesParseError, ThinkingParseCause};
use crate::inference::clients::claude_subscription::ClaudeStreamResponse;
use gents_loop::provider_audit::{ClaudeAuditEvent, ClaudeBlockKind};

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

    pub(super) fn finish_observed(
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
                            content: crate::inference::llm::rig_compat::to_rig_reasoning_part(
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
                            content: crate::inference::llm::rig_compat::to_rig_reasoning_part(
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
