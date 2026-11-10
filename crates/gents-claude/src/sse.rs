//! Server-sent events of a streaming Messages response: framing, typed events
//! and the message accumulator.
//!
//! The framing follows the vendored Anthropic SDK line and SSE decoders. The
//! decoder is incremental over arbitrary byte chunks and copies each payload
//! byte at most once: a line wholly inside one chunk is parsed in place, and
//! only a line split across chunks is buffered. Memory is bounded by the
//! per-event byte cap (`max_event_bytes`): the bytes of the event name, the
//! data lines (plus one separator each) and any partial line may not exceed it,
//! otherwise [`SseError::EventTooLarge`] is returned. Invalid UTF-8 is not an
//! error: each invalid sequence decodes to U+FFFD, as the SDK's non-fatal
//! `TextDecoder` does, and a leading U+FEFF is stripped from every line. After any error the
//! decoder is in an unspecified state and must be dropped.

mod accumulate;

use serde_json::{Map, Value};

pub use accumulate::{AccumulateError, Message, MessageAccumulator, Usage};

/// Default cap on the bytes of one pending event (32 MiB).
pub const DEFAULT_MAX_EVENT_BYTES: usize = 32 * 1024 * 1024;

/// Failure to frame or type a server-sent event.
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    /// The pending event grew past the byte cap.
    #[error("event exceeds the {max} byte limit")]
    EventTooLarge {
        /// The configured cap in bytes.
        max: usize,
    },
    /// The data of a typed event was not valid JSON.
    #[error("`{event}` event data is not valid JSON")]
    InvalidJson {
        /// The SSE event name.
        event: String,
        /// The JSON parse failure.
        #[source]
        source: serde_json::Error,
    },
    /// The data of a typed event was JSON of the wrong shape.
    #[error("`{event}` event data is malformed: {reason}")]
    InvalidEvent {
        /// The SSE event name.
        event: String,
        /// What was missing or mistyped.
        reason: &'static str,
    },
}

/// One dispatched server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event` field, if the event had one.
    pub event: Option<String>,
    /// The `data` lines joined with `\n`.
    pub data: String,
}

#[derive(Default)]
struct Pending {
    event: Option<String>,
    data: String,
    data_lines: usize,
    bytes: usize,
}

impl Pending {
    fn charge(&mut self, extra: usize, max: usize) -> Result<(), SseError> {
        match self.bytes.checked_add(extra) {
            Some(total) if total <= max => {
                self.bytes = total;
                Ok(())
            }
            _ => Err(SseError::EventTooLarge { max }),
        }
    }

    fn line(&mut self, line: &[u8], max: usize, out: &mut Vec<SseEvent>) -> Result<(), SseError> {
        // Non-fatal UTF-8 decode (invalid sequences become U+FFFD), then the
        // SDK strips one leading BOM from every line.
        let line = String::from_utf8_lossy(line);
        let line = line.strip_prefix('\u{feff}').unwrap_or(&line);
        if line.is_empty() {
            if self.event.as_deref().is_none_or(str::is_empty) && self.data_lines == 0 {
                return Ok(());
            }
            out.push(SseEvent {
                event: self.event.take(),
                data: std::mem::take(&mut self.data),
            });
            self.data_lines = 0;
            self.bytes = 0;
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => {
                let old = self.event.as_ref().map_or(0, String::len);
                self.bytes -= old;
                self.charge(value.len(), max)?;
                self.event = Some(value.to_owned());
            }
            "data" => {
                self.charge(value.len() + 1, max)?;
                if self.data_lines > 0 {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.data_lines += 1;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Incremental SSE framing decoder over arbitrary byte chunks.
pub struct SseDecoder {
    line: Vec<u8>,
    skip_lf: bool,
    pending: Pending,
    max_event_bytes: usize,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SseDecoder {
    /// A decoder with the [`DEFAULT_MAX_EVENT_BYTES`] cap.
    pub fn new() -> Self {
        Self::with_max_event_bytes(DEFAULT_MAX_EVENT_BYTES)
    }

    /// A decoder whose pending event may hold at most `max_event_bytes` bytes.
    pub fn with_max_event_bytes(max_event_bytes: usize) -> Self {
        Self {
            line: Vec::new(),
            skip_lf: false,
            pending: Pending::default(),
            max_event_bytes,
        }
    }

    fn emit(&mut self, head: &[u8], out: &mut Vec<SseEvent>) -> Result<(), SseError> {
        if self.line.is_empty() {
            return self.pending.line(head, self.max_event_bytes, out);
        }
        self.buffer(head)?;
        let line = std::mem::take(&mut self.line);
        let result = self.pending.line(&line, self.max_event_bytes, out);
        self.line = line;
        self.line.clear();
        result
    }

    fn buffer(&mut self, part: &[u8]) -> Result<(), SseError> {
        let room = self.max_event_bytes.saturating_sub(self.pending.bytes);
        if self.line.len() + part.len() > room {
            return Err(SseError::EventTooLarge {
                max: self.max_event_bytes,
            });
        }
        self.line.extend_from_slice(part);
        Ok(())
    }

    /// Feeds one chunk, appending every event it completes to `out`.
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<SseEvent>) -> Result<(), SseError> {
        let mut rest = chunk;
        if let Some(&first) = rest.first() {
            if std::mem::take(&mut self.skip_lf) && first == b'\n' {
                rest = &rest[1..];
            }
        }
        while let Some(i) = rest.iter().position(|&b| b == b'\n' || b == b'\r') {
            let (head, tail) = rest.split_at(i);
            let cr = tail[0] == b'\r';
            rest = &tail[1..];
            self.emit(head, out)?;
            if cr {
                match rest.first() {
                    Some(b'\n') => rest = &rest[1..],
                    None => self.skip_lf = true,
                    Some(_) => {}
                }
            }
        }
        self.buffer(rest)
    }

    /// Ends the stream: processes a final unterminated line. As in the SDK, an
    /// event still pending (no terminating blank line) is not dispatched.
    pub fn finish(mut self, out: &mut Vec<SseEvent>) -> Result<(), SseError> {
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            return Ok(());
        }
        self.pending.line(&line, self.max_event_bytes, out)
    }
}

/// A content block delta of a `content_block_delta` event.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    /// `text_delta`.
    Text {
        /// Appended text.
        text: String,
    },
    /// `input_json_delta`.
    InputJson {
        /// Appended tool input JSON fragment.
        partial_json: String,
    },
    /// `thinking_delta`.
    Thinking {
        /// Appended thinking text.
        thinking: String,
    },
    /// `signature_delta`.
    Signature {
        /// The thinking block signature.
        signature: String,
    },
    /// `citations_delta`.
    Citations {
        /// The appended citation object.
        citation: Value,
    },
    /// `compaction_delta`.
    Compaction {
        /// The compaction content.
        content: Value,
        /// Present only when the delta carried the field.
        encrypted_content: Option<Value>,
    },
    /// Any delta type this module does not know, kept whole.
    Other(Map<String, Value>),
}

/// A typed streaming event. `extra` holds the top-level fields not typed here.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// `message_start`.
    MessageStart {
        /// The initial message object.
        message: Map<String, Value>,
        /// Remaining top-level fields.
        extra: Map<String, Value>,
    },
    /// `message_delta`.
    MessageDelta {
        /// `delta.stop_reason`, when a string.
        stop_reason: Option<String>,
        /// The full `delta` object.
        delta: Map<String, Value>,
        /// The full `usage` object.
        usage: Map<String, Value>,
        /// Remaining top-level fields (`context_management`, ...).
        extra: Map<String, Value>,
    },
    /// `message_stop`.
    MessageStop {
        /// Remaining top-level fields.
        extra: Map<String, Value>,
    },
    /// `content_block_start`.
    ContentBlockStart {
        /// Block position.
        index: usize,
        /// The full block object (its `type` is `content_block["type"]`).
        content_block: Map<String, Value>,
        /// Remaining top-level fields.
        extra: Map<String, Value>,
    },
    /// `content_block_delta`.
    ContentBlockDelta {
        /// Block position.
        index: usize,
        /// The typed delta.
        delta: Delta,
        /// Remaining top-level fields.
        extra: Map<String, Value>,
    },
    /// `content_block_stop`.
    ContentBlockStop {
        /// Block position.
        index: usize,
        /// Remaining top-level fields.
        extra: Map<String, Value>,
    },
    /// `error`.
    Error {
        /// `error.type`, when present.
        error_type: Option<String>,
        /// `error.message`, or the raw data text when it was not JSON.
        message: String,
        /// The unparsed event data.
        raw: String,
    },
}

fn object(name: &str, data: &str) -> Result<Map<String, Value>, SseError> {
    match serde_json::from_str::<Value>(data) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(malformed(name, "data is not a JSON object")),
        Err(source) => Err(SseError::InvalidJson {
            event: name.to_owned(),
            source,
        }),
    }
}

fn malformed(name: &str, reason: &'static str) -> SseError {
    SseError::InvalidEvent {
        event: name.to_owned(),
        reason,
    }
}

fn take_object(
    map: &mut Map<String, Value>,
    key: &str,
    name: &str,
    reason: &'static str,
) -> Result<Map<String, Value>, SseError> {
    match map.remove(key) {
        Some(Value::Object(inner)) => Ok(inner),
        _ => Err(malformed(name, reason)),
    }
}

fn take_index(map: &mut Map<String, Value>, name: &str) -> Result<usize, SseError> {
    map.remove("index")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| malformed(name, "missing integer `index`"))
}

fn take_string(
    map: &mut Map<String, Value>,
    key: &str,
    name: &str,
    reason: &'static str,
) -> Result<String, SseError> {
    match map.remove(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(malformed(name, reason)),
    }
}

fn decode_delta(name: &str, mut delta: Map<String, Value>) -> Result<Delta, SseError> {
    let kind = delta.get("type").and_then(Value::as_str).unwrap_or("");
    Ok(match kind {
        "text_delta" => Delta::Text {
            text: take_string(&mut delta, "text", name, "text_delta without `text`")?,
        },
        "input_json_delta" => Delta::InputJson {
            partial_json: take_string(
                &mut delta,
                "partial_json",
                name,
                "input_json_delta without `partial_json`",
            )?,
        },
        "thinking_delta" => Delta::Thinking {
            thinking: take_string(
                &mut delta,
                "thinking",
                name,
                "thinking_delta without `thinking`",
            )?,
        },
        "signature_delta" => Delta::Signature {
            signature: take_string(
                &mut delta,
                "signature",
                name,
                "signature_delta without `signature`",
            )?,
        },
        "citations_delta" => Delta::Citations {
            citation: delta
                .remove("citation")
                .ok_or_else(|| malformed(name, "citations_delta without `citation`"))?,
        },
        "compaction_delta" => Delta::Compaction {
            content: delta
                .remove("content")
                .ok_or_else(|| malformed(name, "compaction_delta without `content`"))?,
            encrypted_content: delta.remove("encrypted_content"),
        },
        _ => Delta::Other(delta),
    })
}

/// Types one SSE event by its event name; `None` for `ping` and unknown names.
pub fn decode_event(event: &SseEvent) -> Result<Option<StreamEvent>, SseError> {
    let Some(name) = event.event.as_deref() else {
        return Ok(None);
    };
    let data = event.data.as_str();
    let mut obj = match name {
        "message_start"
        | "message_delta"
        | "message_stop"
        | "content_block_start"
        | "content_block_delta"
        | "content_block_stop" => object(name, data)?,
        "error" => return Ok(Some(decode_error(data))),
        _ => return Ok(None),
    };
    Ok(Some(match name {
        "message_start" => StreamEvent::MessageStart {
            message: take_object(&mut obj, "message", name, "missing `message` object")?,
            extra: obj,
        },
        "message_delta" => {
            let delta = take_object(&mut obj, "delta", name, "missing `delta` object")?;
            let usage = take_object(&mut obj, "usage", name, "missing `usage` object")?;
            StreamEvent::MessageDelta {
                stop_reason: delta
                    .get("stop_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                delta,
                usage,
                extra: obj,
            }
        }
        "message_stop" => StreamEvent::MessageStop { extra: obj },
        "content_block_start" => StreamEvent::ContentBlockStart {
            index: take_index(&mut obj, name)?,
            content_block: take_object(
                &mut obj,
                "content_block",
                name,
                "missing `content_block` object",
            )?,
            extra: obj,
        },
        "content_block_delta" => {
            let index = take_index(&mut obj, name)?;
            let delta = take_object(&mut obj, "delta", name, "missing `delta` object")?;
            StreamEvent::ContentBlockDelta {
                index,
                delta: decode_delta(name, delta)?,
                extra: obj,
            }
        }
        _ => StreamEvent::ContentBlockStop {
            index: take_index(&mut obj, name)?,
            extra: obj,
        },
    }))
}

fn decode_error(data: &str) -> StreamEvent {
    let parsed = serde_json::from_str::<Value>(data).ok();
    let error = parsed.as_ref().and_then(|v| v.get("error"));
    let field = |key: &str| error.and_then(|e| e.get(key)).and_then(Value::as_str);
    StreamEvent::Error {
        error_type: field("type").map(str::to_owned),
        message: field("message").unwrap_or(data).to_owned(),
        raw: data.to_owned(),
    }
}

#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;
