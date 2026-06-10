//! Native LLM message family (issue #425, Layer B of the rig shed).
//!
//! Mirrors `rig::completion::message` field-for-field and serde-shape-for-
//! serde-shape, with one deliberate deviation: every `OneOrMany<T>` position
//! is a plain `Vec<T>`. rig's `OneOrMany` serializes as a plain JSON array
//! (custom `serialize_seq`), so `Vec` is byte-compatible while dropping the
//! custom container; the compile-time non-empty invariant is documented at
//! use sites instead.
//!
//! BYTE COMPATIBILITY IS THE CONTRACT: persisted `AgentMessage.content` is
//! `serde_json::to_string(&Message)`, and these types must produce exactly
//! the bytes rig produced so existing transcripts reload without migration.
//! The golden tests at the bottom serialize each persisted shape through BOTH
//! families and assert byte equality, plus deserialize recorded rig-era
//! literals. When Layer A lands and rig leaves the tree, the live-rig halves
//! of those tests are deleted and the recorded literals remain the contract.
//!
//! Serde subtleties worth naming (all mirrored exactly):
//! - `Message` is `tag = "role"`, lowercase; `Assistant.id` has NO
//!   skip-if-none (serializes `"id":null`).
//! - `AssistantContent` is UNTAGGED — variants are discriminated
//!   structurally.
//! - `ToolCall.call_id` serializes `null`; `ToolResult.call_id` is
//!   skip-if-none. Asymmetric in rig; asymmetric here.
//! - `ReasoningContent` is adjacently tagged (`type`/`content`, snake_case).

use serde::{Deserialize, Serialize};

/// One message in a conversation. Mirrors `rig::completion::message::Message`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    /// System message containing instruction text.
    System { content: String },
    /// User message; `content` is non-empty by convention (was `OneOrMany`).
    User { content: Vec<UserContent> },
    /// Assistant message; `content` is non-empty by convention.
    Assistant {
        id: Option<String>,
        content: Vec<AssistantContent>,
    },
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Message::System {
            content: text.into(),
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Message::User {
            content: vec![UserContent::text(text)],
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::text(text)],
        }
    }

    pub fn assistant_with_id(id: String, text: impl Into<String>) -> Self {
        Message::Assistant {
            id: Some(id),
            content: vec![AssistantContent::text(text)],
        }
    }

    /// First user text block, mirroring rig's `Message::rag_text` (which is
    /// `pub(crate)` in rig — the public mirror here replaces the local copies
    /// call sites kept).
    pub fn rag_text(&self) -> Option<String> {
        if let Message::User { content } = self {
            for item in content {
                if let UserContent::Text(Text { text }) = item {
                    return Some(text.clone());
                }
            }
        }
        None
    }
}

/// User-side content. Mirrors rig's `UserContent`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum UserContent {
    Text(Text),
    ToolResult(ToolResult),
    Image(Image),
    Audio(Audio),
    Video(Video),
    Document(Document),
}

impl UserContent {
    pub fn text(text: impl Into<String>) -> Self {
        UserContent::Text(Text { text: text.into() })
    }

    pub fn tool_result(id: impl Into<String>, content: Vec<ToolResultContent>) -> Self {
        UserContent::ToolResult(ToolResult {
            id: id.into(),
            call_id: None,
            content,
        })
    }

    pub fn tool_result_with_call_id(
        id: impl Into<String>,
        call_id: String,
        content: Vec<ToolResultContent>,
    ) -> Self {
        UserContent::ToolResult(ToolResult {
            id: id.into(),
            call_id: Some(call_id),
            content,
        })
    }
}

/// Assistant-side content. Mirrors rig's `AssistantContent` — UNTAGGED, so
/// variants are discriminated by structure; variant order matters for
/// deserialization ambiguity and mirrors rig's.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum AssistantContent {
    Text(Text),
    ToolCall(ToolCall),
    Reasoning(Reasoning),
    Image(Image),
}

impl AssistantContent {
    pub fn text(text: impl Into<String>) -> Self {
        AssistantContent::Text(Text { text: text.into() })
    }
}

/// A typed reasoning block. Mirrors rig's `ReasoningContent`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", content = "content", rename_all = "snake_case")]
pub enum ReasoningContent {
    /// Plain reasoning text with an optional provider signature.
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// Provider-encrypted reasoning payload.
    Encrypted(String),
    /// Redacted reasoning payload preserved as opaque data.
    Redacted { data: String },
    /// Provider-generated reasoning summary text.
    Summary(String),
}

/// Assistant reasoning payload. Mirrors rig's `Reasoning`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Reasoning {
    /// Provider reasoning identifier, when supplied by the upstream API.
    pub id: Option<String>,
    /// Ordered reasoning content blocks.
    pub content: Vec<ReasoningContent>,
}

impl Reasoning {
    pub fn new(input: &str) -> Self {
        Self::new_with_signature(input, None)
    }

    pub fn new_with_signature(input: &str, signature: Option<String>) -> Self {
        Self {
            id: None,
            content: vec![ReasoningContent::Text {
                text: input.to_string(),
                signature,
            }],
        }
    }

    pub fn with_id(mut self, id: String) -> Self {
        self.id = Some(id);
        self
    }
}

/// A tool's result paired back to its call. Mirrors rig's `ToolResult`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ToolResult {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub content: Vec<ToolResultContent>,
}

/// Tool-result content. Mirrors rig's `ToolResultContent`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolResultContent {
    Text(Text),
    Image(Image),
}

impl ToolResultContent {
    pub fn text(text: impl Into<String>) -> Self {
        ToolResultContent::Text(Text { text: text.into() })
    }

    /// Parse a tool output string into tool-result content(s), mirroring
    /// rig's `ToolResultContent::from_tool_output` exactly (simple text,
    /// single-image JSON, and hybrid `{"response", "parts"}` JSON).
    pub fn from_tool_output(output: impl Into<String>) -> Vec<ToolResultContent> {
        let output_str = output.into();

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&output_str) {
            if json.get("response").is_some() || json.get("parts").is_some() {
                let mut results: Vec<ToolResultContent> = Vec::new();

                if let Some(response) = json.get("response") {
                    results.push(ToolResultContent::Text(Text {
                        text: response.to_string(),
                    }));
                }

                if let Some(parts) = json.get("parts").and_then(|p| p.as_array()) {
                    for part in parts {
                        let is_image = part
                            .get("type")
                            .and_then(|t| t.as_str())
                            .is_some_and(|t| t == "image");
                        if !is_image {
                            continue;
                        }
                        if let (Some(data), Some(mime_type)) = (
                            part.get("data").and_then(|v| v.as_str()),
                            part.get("mimeType").and_then(|v| v.as_str()),
                        ) {
                            results.push(ToolResultContent::Image(Image {
                                data: DocumentSourceKind::from_data(data),
                                media_type: ImageMediaType::from_mime_type(mime_type),
                                detail: None,
                                additional_params: None,
                            }));
                        }
                    }
                }

                if !results.is_empty() {
                    return results;
                }
            }

            let is_image = json
                .get("type")
                .and_then(|v| v.as_str())
                .is_some_and(|t| t == "image");
            if is_image {
                if let (Some(data), Some(mime_type)) = (
                    json.get("data").and_then(|v| v.as_str()),
                    json.get("mimeType").and_then(|v| v.as_str()),
                ) {
                    return vec![ToolResultContent::Image(Image {
                        data: DocumentSourceKind::from_data(data),
                        media_type: ImageMediaType::from_mime_type(mime_type),
                        detail: None,
                        additional_params: None,
                    })];
                }
            }
        }

        vec![ToolResultContent::Text(Text { text: output_str })]
    }
}

/// A model-issued tool call. Mirrors rig's `ToolCall` — note `call_id`,
/// `signature`, and `additional_params` have NO skip-if-none (they serialize
/// `null`), unlike `ToolResult.call_id`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub call_id: Option<String>,
    pub function: ToolFunction,
    pub signature: Option<String>,
    pub additional_params: Option<serde_json::Value>,
}

impl ToolCall {
    pub fn new(id: String, function: ToolFunction) -> Self {
        Self {
            id,
            call_id: None,
            function,
            signature: None,
            additional_params: None,
        }
    }

    pub fn with_call_id(mut self, call_id: String) -> Self {
        self.call_id = Some(call_id);
        self
    }
}

/// The function half of a tool call. Mirrors rig's `ToolFunction`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ToolFunction {
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ToolFunction {
    pub fn new(name: String, arguments: serde_json::Value) -> Self {
        Self { name, arguments }
    }
}

/// Basic text content. Mirrors rig's `Text`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Text {
    pub text: String,
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.text)
    }
}

impl From<String> for Text {
    fn from(text: String) -> Self {
        Self { text }
    }
}

impl From<&str> for Text {
    fn from(text: &str) -> Self {
        Self {
            text: text.to_string(),
        }
    }
}

/// Image content. Mirrors rig's `Image`.
#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Image {
    pub data: DocumentSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<ImageMediaType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<ImageDetail>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
}

/// Audio content. Mirrors rig's `Audio`.
#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Audio {
    pub data: DocumentSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<AudioMediaType>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
}

/// Video content. Mirrors rig's `Video`.
#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Video {
    pub data: DocumentSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<VideoMediaType>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
}

/// Document content. Mirrors rig's `Document`.
#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Document {
    pub data: DocumentSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<DocumentMediaType>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
}

/// The kind of multimodal data source. Mirrors rig's `DocumentSourceKind`.
#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum DocumentSourceKind {
    /// A file URL/URI.
    Url(String),
    /// A base-64 encoded string.
    Base64(String),
    /// Raw bytes.
    Raw(Vec<u8>),
    /// A string (or a string literal).
    String(String),
    /// An unknown file source (there's nothing there).
    #[default]
    Unknown,
}

impl DocumentSourceKind {
    /// rig's URL-vs-base64 sniff used by `from_tool_output`.
    fn from_data(data: &str) -> Self {
        if data.starts_with("http://") || data.starts_with("https://") {
            DocumentSourceKind::Url(data.to_string())
        } else {
            DocumentSourceKind::Base64(data.to_string())
        }
    }
}

/// Mirrors rig's `ImageMediaType`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[allow(clippy::upper_case_acronyms)]
pub enum ImageMediaType {
    JPEG,
    PNG,
    GIF,
    WEBP,
    HEIC,
    HEIF,
    SVG,
}

impl ImageMediaType {
    /// Mirrors rig's `MimeType::from_mime_type` for images.
    pub fn from_mime_type(mime_type: &str) -> Option<Self> {
        match mime_type {
            "image/jpeg" => Some(Self::JPEG),
            "image/png" => Some(Self::PNG),
            "image/gif" => Some(Self::GIF),
            "image/webp" => Some(Self::WEBP),
            "image/heic" => Some(Self::HEIC),
            "image/heif" => Some(Self::HEIF),
            "image/svg+xml" => Some(Self::SVG),
            _ => None,
        }
    }
}

/// Mirrors rig's `DocumentMediaType`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[allow(clippy::upper_case_acronyms)]
pub enum DocumentMediaType {
    PDF,
    TXT,
    RTF,
    HTML,
    CSS,
    MARKDOWN,
    CSV,
    XML,
    Javascript,
    Python,
}

/// Mirrors rig's `AudioMediaType`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[allow(clippy::upper_case_acronyms)]
pub enum AudioMediaType {
    WAV,
    MP3,
    AIFF,
    AAC,
    OGG,
    FLAC,
    M4A,
    PCM16,
    PCM24,
}

/// Mirrors rig's `VideoMediaType`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[allow(clippy::upper_case_acronyms)]
pub enum VideoMediaType {
    AVI,
    MP4,
    MPEG,
    MOV,
    WEBM,
}

/// Mirrors rig's `ImageDetail`.
#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Low,
    High,
    #[default]
    Auto,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize one native and one rig value, assert byte equality. The rig
    /// half of these tests is deleted when Layer A removes rig; the recorded
    /// literals below remain the contract.
    fn assert_bytes_match(native: &Message, rig: &rig::completion::Message) {
        let native_json = serde_json::to_string(native).expect("native serializes");
        let rig_json = serde_json::to_string(rig).expect("rig serializes");
        assert_eq!(native_json, rig_json, "native and rig bytes must match");
        // And the rig-produced bytes deserialize into the native family.
        let roundtrip: Message =
            serde_json::from_str(&rig_json).expect("rig bytes deserialize natively");
        assert_eq!(&roundtrip, native);
    }

    fn rig_tool_call(id: &str, call_id: Option<&str>) -> rig::completion::message::ToolCall {
        rig::completion::message::ToolCall {
            id: id.to_string(),
            call_id: call_id.map(str::to_string),
            function: rig::completion::message::ToolFunction {
                name: "echo".to_string(),
                arguments: serde_json::json!({"path": "x"}),
            },
            signature: None,
            additional_params: None,
        }
    }

    fn native_tool_call(id: &str, call_id: Option<&str>) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            call_id: call_id.map(str::to_string),
            function: ToolFunction {
                name: "echo".to_string(),
                arguments: serde_json::json!({"path": "x"}),
            },
            signature: None,
            additional_params: None,
        }
    }

    #[test]
    fn golden_system_and_simple_text_messages() {
        assert_bytes_match(
            &Message::system("be brief"),
            &rig::completion::Message::system("be brief"),
        );
        assert_bytes_match(
            &Message::user("hello"),
            &rig::completion::Message::user("hello"),
        );
        assert_bytes_match(
            &Message::assistant("hi"),
            &rig::completion::Message::assistant("hi"),
        );
        assert_bytes_match(
            &Message::assistant_with_id("msg_1".to_string(), "hi"),
            &rig::completion::Message::assistant_with_id("msg_1".to_string(), "hi"),
        );
    }

    #[test]
    fn golden_multi_content_assistant_turn() {
        // The shape the owned loop persists: text + reasoning + two tool calls.
        let native = Message::Assistant {
            id: Some("msg_abc".to_string()),
            content: vec![
                AssistantContent::text("let me check"),
                AssistantContent::Reasoning(
                    Reasoning::new_with_signature("why", Some("sig".to_string()))
                        .with_id("r1".to_string()),
                ),
                AssistantContent::ToolCall(native_tool_call("call-1", Some("call-1"))),
                AssistantContent::ToolCall(native_tool_call("call-2", None)),
            ],
        };
        let rig = rig::completion::Message::Assistant {
            id: Some("msg_abc".to_string()),
            content: rig::one_or_many::OneOrMany::many(vec![
                rig::completion::message::AssistantContent::text("let me check"),
                rig::completion::message::AssistantContent::Reasoning(
                    rig::completion::message::Reasoning::new_with_signature(
                        "why",
                        Some("sig".to_string()),
                    )
                    .with_id("r1".to_string()),
                ),
                rig::completion::message::AssistantContent::ToolCall(rig_tool_call(
                    "call-1",
                    Some("call-1"),
                )),
                rig::completion::message::AssistantContent::ToolCall(rig_tool_call("call-2", None)),
            ])
            .unwrap(),
        };
        assert_bytes_match(&native, &rig);
    }

    #[test]
    fn golden_tool_result_messages() {
        let native = Message::User {
            content: vec![UserContent::tool_result_with_call_id(
                "call-1",
                "call-1".to_string(),
                ToolResultContent::from_tool_output("plain output"),
            )],
        };
        let rig = rig::completion::Message::User {
            content: rig::one_or_many::OneOrMany::one(
                rig::completion::message::UserContent::tool_result_with_call_id(
                    "call-1",
                    "call-1".to_string(),
                    rig::completion::message::ToolResultContent::from_tool_output("plain output"),
                ),
            ),
        };
        assert_bytes_match(&native, &rig);

        // Without call_id (the skip-if-none asymmetry).
        let native = Message::User {
            content: vec![UserContent::tool_result(
                "call-2",
                vec![ToolResultContent::text("out")],
            )],
        };
        let rig = rig::completion::Message::User {
            content: rig::one_or_many::OneOrMany::one(
                rig::completion::message::UserContent::tool_result(
                    "call-2",
                    rig::one_or_many::OneOrMany::one(
                        rig::completion::message::ToolResultContent::text("out"),
                    ),
                ),
            ),
        };
        assert_bytes_match(&native, &rig);
    }

    #[test]
    fn golden_reasoning_content_variants() {
        for (native_rc, rig_rc) in [
            (
                ReasoningContent::Encrypted("blob".to_string()),
                rig::completion::message::ReasoningContent::Encrypted("blob".to_string()),
            ),
            (
                ReasoningContent::Redacted {
                    data: "blob".to_string(),
                },
                rig::completion::message::ReasoningContent::Redacted {
                    data: "blob".to_string(),
                },
            ),
            (
                ReasoningContent::Summary("sum".to_string()),
                rig::completion::message::ReasoningContent::Summary("sum".to_string()),
            ),
        ] {
            assert_eq!(
                serde_json::to_string(&native_rc).unwrap(),
                serde_json::to_string(&rig_rc).unwrap(),
            );
        }
    }

    #[test]
    fn from_tool_output_matches_rig_semantics() {
        for output in [
            "plain text",
            r#"{"ok":true,"result":"x"}"#,
            r#"{"response":{"a":1},"parts":[{"type":"image","data":"abc","mimeType":"image/png"}]}"#,
            r#"{"type":"image","data":"https://x/y.png","mimeType":"image/png"}"#,
            "",
        ] {
            let native = ToolResultContent::from_tool_output(output);
            let rig = rig::completion::message::ToolResultContent::from_tool_output(output);
            let rig_vec: Vec<_> = rig.into_iter().collect();
            assert_eq!(
                serde_json::to_string(&native).unwrap(),
                serde_json::to_string(&rig_vec).unwrap(),
                "from_tool_output divergence for {output:?}"
            );
        }
    }

    /// Recorded rig-era literals: the persisted-format contract that outlives
    /// rig. These strings were produced by rig 0.35 serialization of the
    /// shapes the runtime persists today.
    #[test]
    fn recorded_persisted_literals_deserialize_natively() {
        let literals = [
            r#"{"role":"system","content":"be brief"}"#,
            r#"{"role":"user","content":[{"type":"text","text":"hello"}]}"#,
            r#"{"role":"assistant","id":null,"content":[{"text":"hi"}]}"#,
            r#"{"role":"assistant","id":"msg_1","content":[{"text":"t"},{"id":"call-1","call_id":"call-1","function":{"name":"echo","arguments":{}},"signature":null,"additional_params":null}]}"#,
            r#"{"role":"user","content":[{"type":"toolresult","id":"call-1","call_id":"call-1","content":[{"type":"text","text":"out"}]}]}"#,
        ];
        for literal in literals {
            let message: Message = serde_json::from_str(literal)
                .unwrap_or_else(|error| panic!("literal failed natively: {error}\n{literal}"));
            // Re-serialization is byte-identical (no field reordering/loss).
            assert_eq!(
                serde_json::to_string(&message).unwrap(),
                literal,
                "native re-serialization must reproduce the recorded bytes"
            );
        }
    }
}
