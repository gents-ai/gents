//! Native LLM message family mirroring `rig::completion::message`
//! field-for-field and serde-shape-for-serde-shape, with one deliberate
//! deviation: every `OneOrMany<T>` position is a plain `Vec<T>` (rig
//! serializes it as a plain JSON array, so bytes are unchanged).
//!
//! BYTE COMPATIBILITY IS THE PROVIDER CONTRACT: these native types preserve
//! Rig's message serialization. Canonical transcript headers and output
//! segments reconstruct this message family; they do not store a serialized
//! message in an `AgentMessage.content` column.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: Vec<UserContent>,
    },
    Assistant {
        id: Option<String>,
        /// The provider's response request id (for Claude, the `request-id`
        /// header); `None` when the provider reports none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
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
            request_id: None,
            content: vec![AssistantContent::text(text)],
        }
    }

    pub fn assistant_with_id(id: String, text: impl Into<String>) -> Self {
        Message::Assistant {
            id: Some(id),
            request_id: None,
            content: vec![AssistantContent::text(text)],
        }
    }

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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", content = "content", rename_all = "snake_case")]
pub enum ReasoningContent {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    Encrypted(String),
    Redacted {
        data: String,
    },
    Summary(String),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Reasoning {
    pub id: Option<String>,
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ToolResult {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub content: Vec<ToolResultContent>,
}

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
    /// rig's `ToolResultContent::from_tool_output`.
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub call_id: Option<String>,
    pub function: ToolFunction,
    pub signature: Option<String>,
    pub additional_params: Option<serde_json::Value>,
    /// The provider's streamed argument text, kept only when it parses to
    /// `function.arguments` yet differs from its compact serialization (key
    /// order or spacing); set through [`ToolCall::raw_arguments_for`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_arguments: Option<String>,
}

impl ToolCall {
    pub fn new(id: String, function: ToolFunction) -> Self {
        Self {
            id,
            call_id: None,
            function,
            signature: None,
            additional_params: None,
            raw_arguments: None,
        }
    }

    /// The one rule for [`ToolCall::raw_arguments`]: `text` is kept only when
    /// it parses to exactly `arguments` and is not already their compact form.
    pub fn raw_arguments_for(arguments: &serde_json::Value, text: &str) -> Option<String> {
        let parsed = serde_json::from_str::<serde_json::Value>(text).ok()?;
        (parsed == *arguments && serde_json::to_string(arguments).ok().as_deref() != Some(text))
            .then(|| text.to_owned())
    }

    pub fn with_call_id(mut self, call_id: String) -> Self {
        self.call_id = Some(call_id);
        self
    }
}

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

#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Audio {
    pub data: DocumentSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<AudioMediaType>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
}

#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Video {
    pub data: DocumentSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<VideoMediaType>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
}

#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Document {
    pub data: DocumentSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<DocumentMediaType>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
}

#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum DocumentSourceKind {
    Url(String),
    Base64(String),
    Raw(Vec<u8>),
    String(String),
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
            assert_eq!(
                serde_json::to_string(&message).unwrap(),
                literal,
                "native re-serialization must reproduce the recorded bytes"
            );
        }
    }

    #[test]
    fn raw_arguments_are_kept_only_for_a_distinct_exact_text() {
        let arguments = serde_json::json!({"b": 1, "a": [true, null]});
        let canonical = serde_json::to_string(&arguments).unwrap();
        // Same value, different spelling: kept.
        for text in [
            r#"{"b":1,"a":[true,null]}"#,
            r#"{ "b": 1, "a": [true, null] }"#,
        ] {
            assert_eq!(
                ToolCall::raw_arguments_for(&arguments, text).as_deref(),
                (text != canonical).then_some(text),
                "{text}"
            );
        }
        // Already the compact serialization, a different value, or not JSON: dropped.
        assert_eq!(ToolCall::raw_arguments_for(&arguments, &canonical), None);
        assert_eq!(
            ToolCall::raw_arguments_for(&arguments, r#"{"b":2,"a":[true,null]}"#),
            None
        );
        assert_eq!(ToolCall::raw_arguments_for(&arguments, "{"), None);
        assert_eq!(ToolCall::raw_arguments_for(&arguments, ""), None);
    }

    #[test]
    fn raw_arguments_and_request_id_are_invisible_when_absent() {
        let call = ToolCall::new(
            "c".into(),
            ToolFunction::new("t".into(), serde_json::json!({})),
        );
        assert!(!serde_json::to_string(&call)
            .unwrap()
            .contains("raw_arguments"));
        let message = Message::assistant("hi");
        assert!(!serde_json::to_string(&message)
            .unwrap()
            .contains("request_id"));
        let with = Message::Assistant {
            id: None,
            request_id: Some("req_1".into()),
            content: vec![],
        };
        let json = serde_json::to_string(&with).unwrap();
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), with);
    }
}
