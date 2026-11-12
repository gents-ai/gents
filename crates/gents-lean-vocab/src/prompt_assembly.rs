use crate::generated::*;

pub fn native_claude_reasoning_parts(
    parts: &[LeanClaudeReplayInputPart],
) -> Vec<gents_protocol::message::ReasoningContent> {
    use gents_protocol::message::ReasoningContent;
    parts
        .iter()
        .map(|part| {
            let payload = String::from_utf8(part.payload.clone()).expect("reconstructed UTF-8");
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

pub fn native_claude_replay_content(
    blocks: &[LeanClaudeReplayInputBlock],
) -> Vec<gents_protocol::message::AssistantContent> {
    use gents_protocol::message::{
        AssistantContent, Image, Reasoning, Text, ToolCall, ToolFunction,
    };
    let mut content = Vec::new();
    for block in blocks {
        match block.kind.as_str() {
            "text" => content.push(AssistantContent::Text(Text {
                text: String::from_utf8(block.payload.clone().expect("text payload"))
                    .expect("reconstructed UTF-8"),
            })),
            "reasoning" => {
                let parts =
                    native_claude_reasoning_parts(block.parts.as_ref().expect("reasoning parts"));
                content.push(AssistantContent::Reasoning(Reasoning {
                    id: block.id.clone(),
                    content: parts,
                }));
            }
            "toolCall" => {
                let arguments = String::from_utf8(block.arguments.clone().expect("arguments"))
                    .expect("reconstructed UTF-8");
                content.push(AssistantContent::ToolCall(ToolCall {
                    raw_arguments: None,
                    id: block.id.clone().expect("provider tool id"),
                    call_id: block.call_id.clone(),
                    function: ToolFunction {
                        name: block.name.clone().expect("tool name"),
                        arguments: serde_json::from_str(&arguments)
                            .expect("modeled JSON arguments"),
                    },
                    signature: block.signature.clone(),
                    additional_params: block.additional_params.as_ref().map(|value| {
                        serde_json::from_str(value).expect("modeled additional params JSON")
                    }),
                }));
            }
            "unsupportedMedia" => content.push(AssistantContent::Image(Image::default())),
            other => panic!("unsupported modeled replay input block {other}"),
        }
    }
    content
}
