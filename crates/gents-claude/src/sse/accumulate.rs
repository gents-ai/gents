//! The message accumulator: folds typed stream events into the final message,
//! reproducing the Anthropic SDK stream accumulation (event order checks, key
//! order preserved by in-place updates, tool input parsed at block stop).
//!
//! Unlike the SDK, a tool block's `input` is not best-effort parsed while its
//! JSON is still streaming; it keeps the `content_block_start` value until
//! `content_block_stop` parses the complete buffer.

use serde_json::{Map, Value};

use super::{Delta, StreamEvent};

/// Failure while accumulating stream events.
#[derive(Debug, thiserror::Error)]
pub enum AccumulateError {
    /// `message_start` arrived while a message was already in progress.
    #[error("unexpected event order: message_start before message_stop")]
    DuplicateMessageStart,
    /// An event arrived before `message_start`.
    #[error("unexpected event order: {event} before message_start")]
    BeforeMessageStart {
        /// The offending event name.
        event: &'static str,
    },
    /// The accumulated tool input JSON of a block did not parse.
    #[error("tool input of content block {index} is not valid JSON")]
    ToolInput {
        /// The content block index.
        index: usize,
        /// The JSON parse failure.
        #[source]
        source: serde_json::Error,
    },
    /// The stream ended without `message_stop`.
    #[error("stream ended before message_stop")]
    IncompleteStream,
}

/// Folds [`StreamEvent`]s into a message snapshot.
#[derive(Debug, Default)]
pub struct MessageAccumulator {
    message: Option<Map<String, Value>>,
    // Partial tool input JSON per content block; empty means no buffer.
    partial: Vec<String>,
    stopped: bool,
}

fn is_tool_block(block: &Map<String, Value>) -> bool {
    matches!(
        block.get("type").and_then(Value::as_str),
        Some("tool_use" | "server_tool_use" | "mcp_tool_use")
    )
}

fn set_or_remove(target: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    match value {
        Some(v) => {
            target.insert(key.to_owned(), v.clone());
        }
        None => {
            target.shift_remove(key);
        }
    }
}

fn set_unless_null(target: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(v) = value.filter(|v| !v.is_null()) {
        target.insert(key.to_owned(), v.clone());
    }
}

const USAGE_OPTIONAL: [&str; 6] = [
    "input_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "server_tool_use",
    "iterations",
    "fallback_credit",
];

impl MessageAccumulator {
    /// An empty accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current snapshot, once `message_start` has been applied.
    pub fn message(&self) -> Option<&Map<String, Value>> {
        self.message.as_ref()
    }

    /// Applies one event to the snapshot.
    pub fn apply(&mut self, event: &StreamEvent) -> Result<(), AccumulateError> {
        if let StreamEvent::MessageStart { message, .. } = event {
            if self.message.is_some() {
                return Err(AccumulateError::DuplicateMessageStart);
            }
            self.partial = vec![
                String::new();
                message
                    .get("content")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            ];
            self.message = Some(message.clone());
            return Ok(());
        }
        let name = match event {
            StreamEvent::MessageDelta { .. } => "message_delta",
            StreamEvent::MessageStop { .. } => "message_stop",
            StreamEvent::ContentBlockStart { .. } => "content_block_start",
            StreamEvent::ContentBlockDelta { .. } => "content_block_delta",
            StreamEvent::ContentBlockStop { .. } => "content_block_stop",
            StreamEvent::Error { .. } | StreamEvent::MessageStart { .. } => return Ok(()),
        };
        let Some(message) = self.message.as_mut() else {
            return Err(AccumulateError::BeforeMessageStart { event: name });
        };
        match event {
            StreamEvent::MessageStop { .. } => self.stopped = true,
            StreamEvent::MessageDelta {
                delta,
                usage,
                extra,
                ..
            } => apply_message_delta(message, delta, usage, extra),
            StreamEvent::ContentBlockStart { content_block, .. } => {
                let fallback =
                    content_block.get("type").and_then(Value::as_str) == Some("fallback");
                if let (true, Some(model)) = (
                    fallback,
                    content_block.get("to").and_then(|t| t.get("model")),
                ) {
                    message.insert("model".to_owned(), model.clone());
                }
                if let Some(content) = content_mut(message) {
                    content.push(Value::Object(content_block.clone()));
                    self.partial.push(String::new());
                }
            }
            StreamEvent::ContentBlockDelta { index, delta, .. } => {
                let index = *index;
                let Some(block) = content_mut(message)
                    .and_then(|c| c.get_mut(index))
                    .and_then(Value::as_object_mut)
                else {
                    return Ok(());
                };
                if let Delta::InputJson { partial_json } = delta {
                    if let (true, Some(buf)) = (is_tool_block(block), self.partial.get_mut(index)) {
                        buf.push_str(partial_json);
                    }
                } else {
                    apply_block_delta(block, delta);
                }
            }
            StreamEvent::ContentBlockStop { index, .. } => {
                let index = *index;
                let Some(block) = content_mut(message)
                    .and_then(|c| c.get_mut(index))
                    .and_then(Value::as_object_mut)
                else {
                    return Ok(());
                };
                let buf = self.partial.get(index).filter(|b| !b.is_empty());
                if let (true, Some(buf)) = (is_tool_block(block), buf) {
                    let input = serde_json::from_str::<Value>(buf)
                        .map_err(|source| AccumulateError::ToolInput { index, source })?;
                    block.insert("input".to_owned(), input);
                }
            }
            StreamEvent::MessageStart { .. } | StreamEvent::Error { .. } => {}
        }
        Ok(())
    }

    /// Ends the stream, returning the message if `message_stop` was applied.
    pub fn finish(self) -> Result<Message, AccumulateError> {
        match self.message {
            Some(map) if self.stopped => Ok(Message { map }),
            _ => Err(AccumulateError::IncompleteStream),
        }
    }
}

fn content_mut(message: &mut Map<String, Value>) -> Option<&mut Vec<Value>> {
    let content = message
        .entry("content")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !content.is_array() {
        *content = Value::Array(Vec::new());
    }
    content.as_array_mut()
}

fn apply_message_delta(
    message: &mut Map<String, Value>,
    delta: &Map<String, Value>,
    usage: &Map<String, Value>,
    extra: &Map<String, Value>,
) {
    for key in ["stop_reason", "stop_sequence", "stop_details"] {
        set_or_remove(message, key, delta.get(key));
    }
    set_unless_null(message, "container", delta.get("container"));
    for key in ["context_management", "input_transformations"] {
        set_unless_null(message, key, extra.get(key));
    }
    if !message.get("usage").is_some_and(Value::is_object) {
        message.insert("usage".to_owned(), Value::Object(Map::new()));
    }
    let Some(Value::Object(target)) = message.get_mut("usage") else {
        return;
    };
    set_or_remove(target, "output_tokens", usage.get("output_tokens"));
    for key in USAGE_OPTIONAL {
        set_unless_null(target, key, usage.get(key));
    }
    set_unless_null(
        target,
        "output_tokens_details",
        usage.get("output_tokens_details"),
    );
}

fn apply_block_delta(block: &mut Map<String, Value>, delta: &Delta) {
    let kind = block.get("type").and_then(Value::as_str);
    match (delta, kind) {
        (Delta::Text { text }, Some("text")) => {
            let mut full = block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            full.push_str(text);
            block.insert("text".to_owned(), Value::String(full));
        }
        (Delta::Citations { citation }, Some("text")) => {
            match block.get_mut("citations") {
                Some(Value::Array(list)) => list.push(citation.clone()),
                _ => {
                    block.insert("citations".to_owned(), Value::Array(vec![citation.clone()]));
                }
            };
        }
        (Delta::Thinking { thinking }, Some("thinking")) => {
            let mut full = block
                .get("thinking")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            full.push_str(thinking);
            block.insert("thinking".to_owned(), Value::String(full));
        }
        (Delta::Signature { signature }, Some("thinking")) => {
            block.insert("signature".to_owned(), Value::String(signature.clone()));
        }
        (
            Delta::Compaction {
                content,
                encrypted_content,
            },
            Some("compaction"),
        ) => {
            block.insert("content".to_owned(), content.clone());
            if let Some(enc) = encrypted_content {
                block.insert("encrypted_content".to_owned(), enc.clone());
            }
        }
        _ => {}
    }
}

/// Token usage of a finished message; `raw` is the full usage object.
#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    /// `input_tokens`.
    pub input_tokens: Option<u64>,
    /// `output_tokens`.
    pub output_tokens: Option<u64>,
    /// `cache_read_input_tokens`.
    pub cache_read_input_tokens: Option<u64>,
    /// `cache_creation_input_tokens`.
    pub cache_creation_input_tokens: Option<u64>,
    /// The whole usage object, unknown fields included.
    pub raw: Map<String, Value>,
}

/// A fully accumulated message (a `message_stop` was seen).
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    map: Map<String, Value>,
}

impl Message {
    /// The message as a JSON object, key order as the SDK produces it.
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.map
    }

    /// Consumes the message into its JSON object.
    pub fn into_map(self) -> Map<String, Value> {
        self.map
    }

    fn str_field(&self, key: &str) -> Option<&str> {
        self.map.get(key).and_then(Value::as_str)
    }

    /// The message `id`.
    pub fn id(&self) -> Option<&str> {
        self.str_field("id")
    }

    /// The `model`, as updated by a `fallback` block.
    pub fn model(&self) -> Option<&str> {
        self.str_field("model")
    }

    /// The `stop_reason`, absent when null.
    pub fn stop_reason(&self) -> Option<&str> {
        self.str_field("stop_reason")
    }

    /// The content blocks.
    pub fn content(&self) -> &[Value] {
        self.map
            .get("content")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    /// The token usage, if the message carried a usage object.
    pub fn usage(&self) -> Option<Usage> {
        let raw = self.map.get("usage")?.as_object()?;
        let count = |key: &str| raw.get(key).and_then(Value::as_u64);
        Some(Usage {
            input_tokens: count("input_tokens"),
            output_tokens: count("output_tokens"),
            cache_read_input_tokens: count("cache_read_input_tokens"),
            cache_creation_input_tokens: count("cache_creation_input_tokens"),
            raw: raw.clone(),
        })
    }
}
