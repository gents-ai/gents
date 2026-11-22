use gents_protocol::message::{Message, Text, ToolResult, ToolResultContent, UserContent};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lean `ToolResultText.Parts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parts {
    None,
    One,
    Many,
}

/// Lean `ToolResultText.Wire`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wire {
    AsIs,
    Wrapped,
}

/// Lean `ToolResultText.wire`.
pub fn wire(parts: Parts, open_brace: bool, one_json_container: bool) -> Wire {
    if parts == Parts::None || !open_brace || (parts == Parts::One && one_json_container) {
        Wire::AsIs
    } else {
        Wire::Wrapped
    }
}

/// Every tool result in `message` in the shape Lean `ToolResultText.wire`
/// chooses for its text.
pub fn for_provider(mut message: Message) -> Message {
    if let Message::User { content } = &mut message {
        for part in content.iter_mut() {
            if let UserContent::ToolResult(result) = part {
                shape(result);
            }
        }
    }
    message
}

fn shape(result: &mut ToolResult) {
    let texts: Vec<&str> = result
        .content
        .iter()
        .filter_map(|part| match part {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            ToolResultContent::Image(_) => None,
        })
        .collect();
    let parts = match texts.len() {
        0 => Parts::None,
        1 => Parts::One,
        _ => Parts::Many,
    };
    let open_brace = texts.iter().any(|text| text.contains('{'));
    let one_json_container = matches!(
        texts.as_slice(),
        [text] if matches!(
            serde_json::from_str::<Value>(text),
            Ok(Value::Object(_) | Value::Array(_))
        )
    );
    if wire(parts, open_brace, one_json_container) == Wire::AsIs {
        return;
    }
    let wrapped = serde_json::json!({ "text": texts.join("\n") }).to_string();
    let mut wrapped = Some(ToolResultContent::Text(Text { text: wrapped }));
    result.content = std::mem::take(&mut result.content)
        .into_iter()
        .filter_map(|part| match part {
            ToolResultContent::Text(_) => wrapped.take(),
            image @ ToolResultContent::Image(_) => Some(image),
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::{for_provider, wire};
    use gents_protocol::message::{Message, ToolResult, ToolResultContent, UserContent};

    #[test]
    fn tool_result_wire_matches_lean() {
        gents_lean_vocab::families::tool_result_text_wire::replay(|i| {
            wire(i.parts, i.open_brace, i.one_json_container)
        });
    }

    fn sent(text: &str) -> String {
        let message = Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: "call_1".to_owned(),
                call_id: None,
                content: vec![ToolResultContent::text(text)],
            })],
        };
        let Message::User { content } = for_provider(message) else {
            unreachable!("a user message stays a user message")
        };
        let [UserContent::ToolResult(result)] = content.as_slice() else {
            unreachable!("one tool result stays one tool result")
        };
        let [ToolResultContent::Text(text)] = result.content.as_slice() else {
            unreachable!("one text part stays one text part")
        };
        text.text.clone()
    }

    #[test]
    fn a_sentence_and_its_next_call_reach_the_provider_as_one_object() {
        let error = r#"no tool call x in session y; next call: sessions {"action":"transcript"}"#;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&sent(error)).unwrap(),
            serde_json::json!({ "text": error })
        );
        let read = "gents_fs: {\"ok\":true}\ncontent:\nL1: hello";
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&sent(read)).unwrap(),
            serde_json::json!({ "text": read })
        );
    }

    #[test]
    fn plain_text_and_whole_json_reach_the_provider_unchanged() {
        for text in [
            "L1: hello\nL2: world",
            r#"{"ok":true,"content":"L1: {}"}"#,
            "[1,{}]",
        ] {
            assert_eq!(sent(text), text);
        }
    }
}
