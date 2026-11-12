use gents_protocol::message::{
    AssistantContent, Image, Reasoning, ReasoningContent, Text, ToolCall, ToolFunction,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub kind: String,
    pub payload: Vec<u64>,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputBlock {
    pub kind: String,
    pub payload: Option<Vec<u64>>,
    pub id: Option<String>,
    pub parts: Option<Vec<Part>>,
    pub doc_id: Option<u64>,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<Vec<u64>>,
    pub signature: Option<String>,
    pub additional_params: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub kind: String,
    pub payload: Option<Vec<u64>>,
    pub signature: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<Vec<u64>>,
}

pub fn text_of(bytes: &[u64]) -> String {
    String::from_utf8(
        bytes
            .iter()
            .map(|byte| u8::try_from(*byte).expect("modeled byte"))
            .collect(),
    )
    .expect("reconstructed UTF-8")
}

pub fn bytes_of(text: &str) -> Vec<u64> {
    text.bytes().map(u64::from).collect()
}

pub fn reasoning_parts(parts: &[Part]) -> Vec<ReasoningContent> {
    parts
        .iter()
        .map(|part| {
            let payload = text_of(&part.payload);
            match part.kind.as_str() {
                "text" => ReasoningContent::Text {
                    text: payload,
                    signature: part.signature.clone(),
                },
                "redacted" => ReasoningContent::Redacted { data: payload },
                "encrypted" => ReasoningContent::Encrypted(payload),
                "summary" => ReasoningContent::Summary(payload),
                other => panic!("unknown modeled reasoning part {other}"),
            }
        })
        .collect()
}

pub fn assistant_content(blocks: &[InputBlock]) -> Vec<AssistantContent> {
    blocks
        .iter()
        .map(|block| match block.kind.as_str() {
            "text" => AssistantContent::Text(Text {
                text: text_of(block.payload.as_deref().expect("text payload")),
            }),
            "reasoning" => AssistantContent::Reasoning(Reasoning {
                id: block.id.clone(),
                content: reasoning_parts(block.parts.as_deref().expect("reasoning parts")),
            }),
            "toolCall" => AssistantContent::ToolCall(ToolCall {
                raw_arguments: None,
                id: block.id.clone().expect("provider tool id"),
                call_id: block.call_id.clone(),
                function: ToolFunction {
                    name: block.name.clone().expect("tool name"),
                    arguments: serde_json::from_str(&text_of(
                        block.arguments.as_deref().expect("arguments"),
                    ))
                    .expect("modeled JSON arguments"),
                },
                signature: block.signature.clone(),
                additional_params: block.additional_params.as_ref().map(|value| {
                    serde_json::from_str(value).expect("modeled additional params JSON")
                }),
            }),
            "unsupportedMedia" => AssistantContent::Image(Image::default()),
            other => panic!("unsupported modeled replay input block {other}"),
        })
        .collect()
}

impl Block {
    pub fn native_json(&self) -> Value {
        let payload = || text_of(self.payload.as_deref().expect("replay payload"));
        match self.kind.as_str() {
            "text" => serde_json::json!({"type": "text", "text": payload()}),
            "signedThinking" => serde_json::json!({
                "type": "thinking",
                "thinking": payload(),
                "signature": self.signature.as_deref().expect("modeled signature"),
            }),
            "redactedThinking" => serde_json::json!({
                "type": "redacted_thinking",
                "data": payload(),
            }),
            "toolUse" => serde_json::json!({
                "type": "tool_use",
                "id": self.id.as_deref().expect("provider id"),
                "name": self.name.as_deref().expect("tool name"),
                "input": serde_json::from_str::<Value>(&text_of(
                    self.arguments.as_deref().expect("arguments"),
                ))
                .expect("modeled JSON arguments"),
            }),
            other => panic!("unknown modeled replay output {other}"),
        }
    }

    pub fn of_native_json(value: &Value) -> Self {
        let field = |name: &str| {
            value[name]
                .as_str()
                .unwrap_or_else(|| panic!("content block lacks {name}: {value}"))
                .to_owned()
        };
        let bare = |kind: &str| Block {
            kind: kind.to_owned(),
            payload: None,
            signature: None,
            id: None,
            name: None,
            arguments: None,
        };
        let block = match value["type"].as_str() {
            Some("text") => Block {
                payload: Some(bytes_of(&field("text"))),
                ..bare("text")
            },
            Some("thinking") => Block {
                payload: Some(bytes_of(&field("thinking"))),
                signature: Some(field("signature")),
                ..bare("signedThinking")
            },
            Some("redacted_thinking") => Block {
                payload: Some(bytes_of(&field("data"))),
                ..bare("redactedThinking")
            },
            Some("tool_use") => Block {
                id: Some(field("id")),
                name: Some(field("name")),
                arguments: Some(bytes_of(&value["input"].to_string())),
                ..bare("toolUse")
            },
            _ => panic!("unmodeled Messages content block {value}"),
        };
        assert_eq!(
            &block.native_json(),
            value,
            "the content block carries more than its replay block"
        );
        block
    }
}
