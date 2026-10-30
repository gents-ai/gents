//! Request and response types serialized in the key order Claude Code sends.
//!
//! Claude Code serializes with plain `JSON.stringify`: insertion order, no
//! whitespace. `serde_json`'s compact writer escapes strings identically, so a
//! struct whose fields follow Claude Code's key order, with `skip_serializing_if`
//! for absent keys, produces the same bytes. Passthrough JSON (tool schemas and
//! inputs) is held as raw JSON text, and other blocks as ordered raw entries, so
//! key order never depends on serde_json's map feature. The request is
//! serialized straight to bytes with no intermediate `Value`.

use std::fmt;

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

/// A failure producing the request bytes.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// The request could not be serialized to JSON.
    #[error("the Messages request could not be serialized: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// The main-thread Messages request body, in Claude Code's key order.
///
/// Claude Code always sends `tools` (an empty list as `[]`), never sends
/// `tool_choice` on the main thread (an undefined value is dropped), moves
/// `betas` into the `anthropic-beta` header, and leaves `safeguards`, the
/// context hint, cache eviction, `fallbacks`, extra body, `speed`, `thread` and
/// `fallback_credit_token` off by default; none of those are modeled here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MessagesRequest {
    /// The model id.
    pub model: String,
    /// The conversation.
    pub messages: Vec<Message>,
    /// The system blocks.
    pub system: Vec<SystemBlock>,
    /// The tool list; `None` omits the key, Claude Code sends `Some(vec![])`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// The request metadata.
    pub metadata: Metadata,
    /// The output token cap.
    pub max_tokens: u64,
    /// The thinking mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    /// The sampling temperature (the only non-integer number on the wire).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<serde_json::Number>,
    /// The context-management edits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_management: Option<ContextManagement>,
    /// The output configuration; omitted when `None` or empty.
    #[serde(skip_serializing_if = "output_config_absent")]
    pub output_config: Option<OutputConfig>,
    /// The diagnostics block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Diagnostics>,
    /// Whether the response streams.
    pub stream: bool,
}

fn output_config_absent(config: &Option<OutputConfig>) -> bool {
    config
        .as_ref()
        .is_none_or(|c| c.effort.is_none() && c.format.is_none())
}

/// Serializes the request to the exact bytes Claude Code would send.
pub fn to_bytes(request: &MessagesRequest) -> Result<Vec<u8>, WireError> {
    let mut hint = 512 + request.model.len();
    hint += request
        .system
        .iter()
        .map(|b| b.text.len() + 48)
        .sum::<usize>();
    hint += request
        .messages
        .iter()
        .map(Message::size_hint)
        .sum::<usize>();
    let mut out = Vec::with_capacity(hint);
    serde_json::to_writer(&mut out, request)?;
    Ok(out)
}

/// A conversation turn.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    /// The speaker.
    pub role: Role,
    /// The turn body.
    pub content: MessageContent,
    /// The per-turn control of a system row, serialized after `content`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
}

impl Message {
    fn size_hint(&self) -> usize {
        match &self.content {
            MessageContent::Text(t) => t.len() + 32,
            MessageContent::Blocks(b) => b.iter().map(ContentBlock::size_hint).sum::<usize>() + 32,
        }
    }
}

/// The speaker of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The user.
    User,
    /// The assistant.
    Assistant,
    /// A system row (`api_system`): the per-turn effort control.
    System,
}

/// A message body: a bare string or an array of blocks.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// A string body.
    Text(String),
    /// A block array body.
    Blocks(Vec<ContentBlock>),
}

/// A tool result body: a bare string or an array of blocks.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// A string body.
    Text(String),
    /// A block array body.
    Blocks(Vec<ContentBlock>),
}

/// A content block, serialized in the key order Claude Code builds it.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    /// `{type:"text", text, citations?, cache_control?}`.
    Text {
        /// The text.
        text: String,
        /// The citations JSON array, when present, kept as the caller's bytes.
        citations: Option<Box<RawValue>>,
        /// The cache marker.
        cache_control: Option<CacheControl>,
    },
    /// `{type:"image", source:{type:"base64",..}, cache_control?}`.
    Image {
        /// The image media type.
        media_type: String,
        /// The base64 data.
        data: String,
        /// The cache marker.
        cache_control: Option<CacheControl>,
    },
    /// `{type:"document", source:{type:"base64",..}, cache_control?}`.
    Document {
        /// The document media type.
        media_type: String,
        /// The base64 data.
        data: String,
        /// The cache marker.
        cache_control: Option<CacheControl>,
    },
    /// `{type:"tool_use", id, name, input, cache_control?}`.
    ToolUse {
        /// The call id.
        id: String,
        /// The tool name, sent verbatim.
        name: String,
        /// The call input, kept as the caller's bytes.
        input: Box<RawValue>,
        /// The cache marker.
        cache_control: Option<CacheControl>,
    },
    /// `{tool_use_id, type:"tool_result", content, is_error?, cache_control?}`.
    ToolResult {
        /// The id of the call answered.
        tool_use_id: String,
        /// The result body.
        content: ToolResultContent,
        /// The error flag, when set.
        is_error: Option<bool>,
        /// The cache marker.
        cache_control: Option<CacheControl>,
    },
    /// `{type:"thinking", thinking, signature}`.
    Thinking {
        /// The thinking text.
        thinking: String,
        /// The signature.
        signature: String,
    },
    /// `{type:"redacted_thinking", data}`.
    RedactedThinking {
        /// The opaque data.
        data: String,
    },
    /// Any other block, kept verbatim; a cache marker is appended as the last key.
    Raw(RawObject),
}

/// A JSON object's entries in document order, each value kept as raw text.
#[derive(Debug, Clone)]
pub struct RawObject(Vec<(String, Box<RawValue>)>);

struct RawObjectVisitor;

impl<'de> Visitor<'de> for RawObjectVisitor {
    type Value = RawObject;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<RawObject, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut entries: Vec<(String, Box<RawValue>)> =
            Vec::with_capacity(map.size_hint().unwrap_or(0));
        while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
            // JSON.parse keeps the last value at the first key's position.
            // vertexia: linear scan, a block has a handful of keys; index the keys if wide objects appear
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => entries.push((key, value)),
            }
        }
        Ok(RawObject(entries))
    }
}

impl<'de> Deserialize<'de> for RawObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawObjectVisitor)
    }
}

impl Serialize for RawObject {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut m = serializer.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

impl RawObject {
    /// Parses a JSON object, keeping entry order and each value's bytes.
    pub fn from_json(text: &str) -> Result<Self, WireError> {
        Ok(serde_json::from_str(text)?)
    }

    /// The raw value of a key.
    pub fn get(&self, key: &str) -> Option<&RawValue> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| &**v)
    }

    /// The `type` entry when it is a JSON string without escapes.
    pub fn kind(&self) -> Option<&str> {
        serde_json::from_str(self.get("type")?.get()).ok()
    }

    fn set_cache_control(&mut self, cc: &CacheControl) {
        // JS `{...block, cache_control}` places the key last.
        self.0.retain(|(k, _)| k != "cache_control");
        self.0.push(("cache_control".to_owned(), cc.to_raw()));
    }
}

fn raw_eq(a: &RawValue, b: &RawValue) -> bool {
    a.get() == b.get()
}

fn raw_opt_eq(a: &Option<Box<RawValue>>, b: &Option<Box<RawValue>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => raw_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

impl PartialEq for RawObject {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .zip(&other.0)
                .all(|((ka, va), (kb, vb))| ka == kb && raw_eq(va, vb))
    }
}

impl PartialEq for ContentBlock {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Text {
                    text: t1,
                    citations: c1,
                    cache_control: k1,
                },
                Self::Text {
                    text: t2,
                    citations: c2,
                    cache_control: k2,
                },
            ) => t1 == t2 && raw_opt_eq(c1, c2) && k1 == k2,
            (
                Self::ToolUse {
                    id: i1,
                    name: n1,
                    input: p1,
                    cache_control: k1,
                },
                Self::ToolUse {
                    id: i2,
                    name: n2,
                    input: p2,
                    cache_control: k2,
                },
            ) => i1 == i2 && n1 == n2 && raw_eq(p1, p2) && k1 == k2,
            (
                Self::Image {
                    media_type: m1,
                    data: d1,
                    cache_control: k1,
                },
                Self::Image {
                    media_type: m2,
                    data: d2,
                    cache_control: k2,
                },
            )
            | (
                Self::Document {
                    media_type: m1,
                    data: d1,
                    cache_control: k1,
                },
                Self::Document {
                    media_type: m2,
                    data: d2,
                    cache_control: k2,
                },
            ) => m1 == m2 && d1 == d2 && k1 == k2,
            (
                Self::ToolResult {
                    tool_use_id: i1,
                    content: c1,
                    is_error: e1,
                    cache_control: k1,
                },
                Self::ToolResult {
                    tool_use_id: i2,
                    content: c2,
                    is_error: e2,
                    cache_control: k2,
                },
            ) => i1 == i2 && c1 == c2 && e1 == e2 && k1 == k2,
            (
                Self::Thinking {
                    thinking: t1,
                    signature: s1,
                },
                Self::Thinking {
                    thinking: t2,
                    signature: s2,
                },
            ) => t1 == t2 && s1 == s2,
            (Self::RedactedThinking { data: d1 }, Self::RedactedThinking { data: d2 }) => d1 == d2,
            (Self::Raw(a), Self::Raw(b)) => a == b,
            _ => false,
        }
    }
}

/// The wire `source` object of an image or document block.
#[derive(Serialize)]
struct Base64Source<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: &'a str,
    data: &'a str,
}

fn base64_source<'a>(media_type: &'a str, data: &'a str) -> Base64Source<'a> {
    Base64Source {
        kind: "base64",
        media_type,
        data,
    }
}

impl ContentBlock {
    fn size_hint(&self) -> usize {
        match self {
            Self::Text { text, .. } => text.len() + 48,
            Self::Image { data, .. } | Self::Document { data, .. } => data.len() + 96,
            Self::ToolUse { id, name, .. } => id.len() + name.len() + 160,
            Self::ToolResult { content, .. } => match content {
                ToolResultContent::Text(t) => t.len() + 96,
                ToolResultContent::Blocks(b) => b.iter().map(Self::size_hint).sum::<usize>() + 96,
            },
            Self::Thinking {
                thinking,
                signature,
            } => thinking.len() + signature.len() + 48,
            Self::RedactedThinking { data } => data.len() + 48,
            Self::Raw(raw) => {
                raw.0
                    .iter()
                    .map(|(k, v)| k.len() + v.get().len() + 4)
                    .sum::<usize>()
                    + 64
            }
        }
    }

    /// Returns the wire `type` of the block.
    pub fn kind(&self) -> &str {
        match self {
            Self::Text { .. } => "text",
            Self::Image { .. } => "image",
            Self::Document { .. } => "document",
            Self::ToolUse { .. } => "tool_use",
            Self::ToolResult { .. } => "tool_result",
            Self::Thinking { .. } => "thinking",
            Self::RedactedThinking { .. } => "redacted_thinking",
            Self::Raw(raw) => raw.kind().unwrap_or(""),
        }
    }

    /// Attaches a cache marker; returns false for blocks that cannot carry one.
    pub fn set_cache_control(&mut self, cc: CacheControl) -> bool {
        match self {
            Self::Text { cache_control, .. }
            | Self::Image { cache_control, .. }
            | Self::Document { cache_control, .. }
            | Self::ToolUse { cache_control, .. }
            | Self::ToolResult { cache_control, .. } => *cache_control = Some(cc),
            Self::Thinking { .. } | Self::RedactedThinking { .. } => return false,
            Self::Raw(raw) => raw.set_cache_control(&cc),
        }
        true
    }

    /// Returns the cache marker the block carries, if any.
    pub fn cache_control(&self) -> Option<&CacheControl> {
        match self {
            Self::Text { cache_control, .. }
            | Self::Image { cache_control, .. }
            | Self::Document { cache_control, .. }
            | Self::ToolUse { cache_control, .. }
            | Self::ToolResult { cache_control, .. } => cache_control.as_ref(),
            Self::Thinking { .. } | Self::RedactedThinking { .. } => None,
            Self::Raw(raw) => {
                let marker = raw.get("cache_control")?;
                let one_hour = serde_json::from_str::<RawTtl<'_>>(marker.get())
                    .is_ok_and(|m| m.ttl == Some("1h"));
                Some(if one_hour {
                    &CacheControl::ONE_HOUR
                } else {
                    &CacheControl::FIVE_MINUTES
                })
            }
        }
    }
}

#[derive(Deserialize)]
struct RawTtl<'a> {
    #[serde(borrow)]
    ttl: Option<&'a str>,
}

impl Serialize for ContentBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Text {
                text,
                citations,
                cache_control,
            } => {
                let mut m = serializer.serialize_map(None)?;
                m.serialize_entry("type", "text")?;
                m.serialize_entry("text", text)?;
                if let Some(c) = citations {
                    m.serialize_entry("citations", c)?;
                }
                if let Some(c) = cache_control {
                    m.serialize_entry("cache_control", c)?;
                }
                m.end()
            }
            Self::Image {
                media_type,
                data,
                cache_control,
            } => serialize_media(
                serializer,
                "image",
                media_type,
                data,
                cache_control.as_ref(),
            ),
            Self::Document {
                media_type,
                data,
                cache_control,
            } => serialize_media(
                serializer,
                "document",
                media_type,
                data,
                cache_control.as_ref(),
            ),
            Self::ToolUse {
                id,
                name,
                input,
                cache_control,
            } => {
                let mut m = serializer.serialize_map(None)?;
                m.serialize_entry("type", "tool_use")?;
                m.serialize_entry("id", id)?;
                m.serialize_entry("name", name)?;
                m.serialize_entry("input", input)?;
                if let Some(c) = cache_control {
                    m.serialize_entry("cache_control", c)?;
                }
                m.end()
            }
            Self::ToolResult {
                tool_use_id,
                content,
                is_error,
                cache_control,
            } => {
                let mut m = serializer.serialize_map(None)?;
                m.serialize_entry("tool_use_id", tool_use_id)?;
                m.serialize_entry("type", "tool_result")?;
                m.serialize_entry("content", content)?;
                if let Some(e) = is_error {
                    m.serialize_entry("is_error", e)?;
                }
                if let Some(c) = cache_control {
                    m.serialize_entry("cache_control", c)?;
                }
                m.end()
            }
            Self::Thinking {
                thinking,
                signature,
            } => {
                let mut m = serializer.serialize_map(Some(3))?;
                m.serialize_entry("type", "thinking")?;
                m.serialize_entry("thinking", thinking)?;
                m.serialize_entry("signature", signature)?;
                m.end()
            }
            Self::RedactedThinking { data } => {
                let mut m = serializer.serialize_map(Some(2))?;
                m.serialize_entry("type", "redacted_thinking")?;
                m.serialize_entry("data", data)?;
                m.end()
            }
            Self::Raw(raw) => raw.serialize(serializer),
        }
    }
}

fn serialize_media<S>(
    serializer: S,
    kind: &str,
    media_type: &str,
    data: &str,
    cache_control: Option<&CacheControl>,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut m = serializer.serialize_map(None)?;
    m.serialize_entry("type", kind)?;
    m.serialize_entry("source", &base64_source(media_type, data))?;
    if let Some(c) = cache_control {
        m.serialize_entry("cache_control", c)?;
    }
    m.end()
}

/// A cache breakpoint marker: `{type:"ephemeral"}` plus an optional `ttl`.
///
/// Claude Code's `G8` also writes `scope:"global"` for the global scope; Gents
/// never uses it, and the client never writes `ttl:"5m"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheControl {
    /// The lifetime; `None` and `FiveMinutes` both omit the `ttl` key.
    pub ttl: Option<CacheTtl>,
}

impl CacheControl {
    const FIVE_MINUTES: Self = Self { ttl: None };
    const ONE_HOUR: Self = Self {
        ttl: Some(CacheTtl::OneHour),
    };

    /// Builds the marker for a TTL.
    pub fn for_ttl(ttl: CacheTtl) -> Self {
        match ttl {
            CacheTtl::FiveMinutes => Self::FIVE_MINUTES,
            CacheTtl::OneHour => Self::ONE_HOUR,
        }
    }

    fn one_hour(&self) -> bool {
        self.ttl == Some(CacheTtl::OneHour)
    }

    fn to_raw(&self) -> Box<RawValue> {
        RawValue::from_string(
            if self.one_hour() {
                r#"{"type":"ephemeral","ttl":"1h"}"#
            } else {
                r#"{"type":"ephemeral"}"#
            }
            .to_owned(),
        )
        // The two literals above are valid JSON.
        .unwrap_or_else(|_| RawValue::NULL.to_owned())
    }
}

impl Serialize for CacheControl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let one_hour = self.one_hour();
        let mut m = serializer.serialize_map(Some(1 + usize::from(one_hour)))?;
        m.serialize_entry("type", "ephemeral")?;
        if one_hour {
            m.serialize_entry("ttl", "1h")?;
        }
        m.end()
    }
}

/// A cache lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTtl {
    /// The default lifetime: no `ttl` key on the wire.
    FiveMinutes,
    /// One hour: `ttl:"1h"`.
    OneHour,
}

/// A system prompt block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemBlock {
    /// The block text.
    pub text: String,
    /// The cache marker.
    pub cache_control: Option<CacheControl>,
}

impl Serialize for SystemBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut m = serializer.serialize_map(None)?;
        m.serialize_entry("type", "text")?;
        m.serialize_entry("text", &self.text)?;
        if let Some(c) = &self.cache_control {
            m.serialize_entry("cache_control", c)?;
        }
        m.end()
    }
}

/// A tool definition.
///
/// Claude Code's `Q0` builds `{name, description, input_schema, strict?,
/// eager_input_streaming?, defer_loading?, cache_control?, citations?}`; the
/// optional keys are off by default on first party and are not modeled. Names
/// are sent verbatim (Claude Code 2.1.311 does not prefix them).
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    /// The tool name.
    pub name: String,
    /// The tool description.
    pub description: String,
    /// The JSON schema, kept as the caller's bytes.
    pub input_schema: Box<RawValue>,
}

impl PartialEq for Tool {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.description == other.description
            && raw_eq(&self.input_schema, &other.input_schema)
    }
}

/// The request metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Metadata {
    /// The serialized user id.
    pub user_id: String,
}

/// How much of the thinking text the server returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingDisplay {
    /// Streamed thinking updates.
    Updates,
}

/// The thinking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    /// `{type:"adaptive", display?}`.
    Adaptive {
        /// The display mode.
        display: Option<ThinkingDisplay>,
    },
    /// `{budget_tokens, type:"enabled", display?}` (Claude Code's `sp` literal).
    Enabled {
        /// The thinking token budget.
        budget_tokens: u64,
        /// The display mode.
        display: Option<ThinkingDisplay>,
    },
    /// `{type:"disabled"}`.
    Disabled,
}

impl Serialize for Thinking {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut m = serializer.serialize_map(None)?;
        match self {
            Self::Adaptive { display } => {
                m.serialize_entry("type", "adaptive")?;
                if let Some(d) = display {
                    m.serialize_entry("display", d)?;
                }
            }
            Self::Enabled {
                budget_tokens,
                display,
            } => {
                m.serialize_entry("budget_tokens", budget_tokens)?;
                m.serialize_entry("type", "enabled")?;
                if let Some(d) = display {
                    m.serialize_entry("display", d)?;
                }
            }
            Self::Disabled => m.serialize_entry("type", "disabled")?,
        }
        m.end()
    }
}

/// The context-management block.
///
/// `clear_tool_uses_20250919` is server-flag gated and never emitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextManagement {
    /// The edits.
    pub edits: Vec<ContextEdit>,
}

/// A context-management edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextEdit {
    /// `{type:"clear_thinking_20251015", keep:"all"}`, Claude Code's default for
    /// a thinking model with the context-management capability.
    ClearThinking,
}

impl Serialize for ContextEdit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut m = serializer.serialize_map(Some(2))?;
        m.serialize_entry("type", "clear_thinking_20251015")?;
        m.serialize_entry("keep", "all")?;
        m.end()
    }
}

/// The output configuration; callers set the whole block to `None` when both fields are.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputConfig {
    /// The effort level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The structured output format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<Value>,
}

/// The diagnostics block; `previous_message_id` is always present, `null` when unset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostics {
    /// The previous assistant message id.
    pub previous_message_id: Option<String>,
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
