use crate::message::{
    AssistantContent, Message, Reasoning, ReasoningContent, Text, ToolResult, ToolResultContent,
    UserContent,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentedMessageRole {
    User,
    Assistant,
    Tool,
}

impl PresentedMessageRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "USER",
            Self::Assistant => "ASSISTANT",
            Self::Tool => "TOOL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedMessagePresentation {
    pub role: PresentedMessageRole,
    pub body_markdown: String,
    pub reasoning_markdown: Option<String>,
    pub has_tool_calls: bool,
    pub has_tool_results: bool,
}

impl PersistedMessagePresentation {
    pub fn has_visible_body(&self) -> bool {
        !self.body_markdown.trim().is_empty()
    }
}

pub fn normalize_markdown_text(text: &str) -> String {
    let mut normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    while normalized.contains("\n\n\n") {
        normalized = normalized.replace("\n\n\n", "\n\n");
    }
    normalized.trim().to_string()
}

pub fn decode_persisted_message(role: &str, content: &str) -> Message {
    if let Ok(message) = serde_json::from_str::<Message>(content) {
        return message;
    }

    if role == "assistant" {
        if let Ok(content) = serde_json::from_str::<Vec<AssistantContent>>(content) {
            return Message::Assistant { id: None, content };
        }
    }

    if role == "user" {
        if let Ok(content) = serde_json::from_str::<Vec<UserContent>>(content) {
            return Message::User { content };
        }
    }

    match role {
        "assistant" => Message::Assistant {
            id: None,
            content: vec![AssistantContent::Text(Text {
                text: content.to_string(),
            })],
        },
        _ => Message::User {
            content: vec![UserContent::Text(Text {
                text: content.to_string(),
            })],
        },
    }
}

pub fn present_persisted_message(role: &str, content: &str) -> PersistedMessagePresentation {
    let message = decode_persisted_message(role, content);
    let has_tool_calls = match &message {
        Message::Assistant { content, .. } => content
            .iter()
            .any(|item| matches!(item, AssistantContent::ToolCall(_))),
        Message::User { .. } | Message::System { .. } => false,
    };
    let has_tool_results = match &message {
        Message::User { content } => content
            .iter()
            .any(|item| matches!(item, UserContent::ToolResult(_))),
        Message::Assistant { .. } | Message::System { .. } => false,
    };

    let role = match &message {
        Message::Assistant { .. } | Message::System { .. } => PresentedMessageRole::Assistant,
        Message::User { content } => {
            let has_text = content.iter().any(
                |item| matches!(item, UserContent::Text(text) if !text.text.trim().is_empty()),
            );
            if has_text {
                PresentedMessageRole::User
            } else if has_tool_results {
                PresentedMessageRole::Tool
            } else {
                PresentedMessageRole::User
            }
        }
    };

    PersistedMessagePresentation {
        role,
        body_markdown: render_message_body_markdown(&message),
        reasoning_markdown: extract_message_reasoning(&message),
        has_tool_calls,
        has_tool_results,
    }
}

pub fn render_message_body_markdown(message: &Message) -> String {
    match message {
        Message::System { content } => normalize_markdown_text(content),
        Message::User { content } => content
            .iter()
            .filter_map(|item| match item {
                UserContent::Text(text) if !text.text.trim().is_empty() => {
                    Some(normalize_markdown_text(&text.text))
                }
                UserContent::ToolResult(tool_result) => {
                    let rendered = render_tool_result(tool_result);
                    (!rendered.trim().is_empty()).then_some(rendered)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        Message::Assistant { content, .. } => content
            .iter()
            .filter_map(|item| match item {
                AssistantContent::Text(text) if !text.text.trim().is_empty() => {
                    let normalized = normalize_markdown_text(&text.text);
                    (!looks_like_tool_call_markup(&normalized)).then_some(normalized)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

pub fn extract_message_reasoning(message: &Message) -> Option<String> {
    let Message::Assistant { content, .. } = message else {
        return None;
    };

    let chunks = content
        .iter()
        .filter_map(|item| match item {
            AssistantContent::Reasoning(reasoning) => {
                let rendered = render_reasoning_summary(reasoning);
                (!rendered.trim().is_empty()).then_some(rendered)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    (!chunks.is_empty()).then_some(normalize_markdown_text(&chunks.join("\n\n")))
}

fn render_tool_result(tool_result: &ToolResult) -> String {
    tool_result
        .content
        .iter()
        .filter_map(|item| match item {
            ToolResultContent::Text(text) if !text.text.trim().is_empty() => {
                Some(normalize_markdown_text(&text.text))
            }
            ToolResultContent::Text(_) => None,
            _ => Some("[opaque tool result]".to_string()),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_reasoning_summary(reasoning: &Reasoning) -> String {
    let mut out = String::new();
    for item in &reasoning.content {
        let piece = match item {
            ReasoningContent::Text { text, .. } | ReasoningContent::Summary(text) => text.as_str(),
            ReasoningContent::Encrypted(_) => "[encrypted reasoning]",
            ReasoningContent::Redacted { .. } => "[redacted reasoning]",
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(piece);
    }
    normalize_markdown_text(&out)
}

fn looks_like_tool_call_markup(text: &str) -> bool {
    let trimmed = text.trim();
    [
        "<tool_call>",
        "</tool_call>",
        "<arg_key>",
        "</arg_key>",
        "<arg_value>",
        "</arg_value>",
    ]
    .iter()
    .any(|marker| trimmed.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_messages_present_as_tool_rows() {
        let tool_result_message = Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: "tool-1".to_string(),
                call_id: Some("call-1".to_string()),
                content: vec![ToolResultContent::Text(Text {
                    text: "src/app.rs: audit target live".to_string(),
                })],
            })],
        };

        let presentation = present_persisted_message(
            "user",
            &serde_json::to_string(&tool_result_message).expect("serialize tool result"),
        );

        assert_eq!(presentation.role, PresentedMessageRole::Tool);
        assert!(presentation.has_tool_results);
        assert_eq!(presentation.body_markdown, "src/app.rs: audit target live");
    }

    #[test]
    fn assistant_reasoning_is_extracted_from_persisted_messages() {
        let message = Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::Reasoning(Reasoning::new("Need to inspect the CLI flow first")),
                AssistantContent::Text(Text {
                    text: "I checked the CLI flow.".to_string(),
                }),
            ],
        };

        let presentation = present_persisted_message(
            "assistant",
            &serde_json::to_string(&message).expect("serialize assistant message"),
        );

        assert_eq!(presentation.role, PresentedMessageRole::Assistant);
        assert_eq!(presentation.body_markdown, "I checked the CLI flow.");
        assert!(presentation
            .reasoning_markdown
            .as_deref()
            .is_some_and(|reasoning| reasoning.contains("Need to inspect the CLI flow first")));
    }

    #[test]
    fn plain_text_fallback_is_preserved() {
        let presentation = present_persisted_message("assistant", "hello markdown");

        assert_eq!(presentation.role, PresentedMessageRole::Assistant);
        assert_eq!(presentation.body_markdown, "hello markdown");
    }

    #[test]
    fn assistant_tool_call_markup_is_hidden_from_visible_body() {
        let presentation = present_persisted_message(
            "assistant",
            "<tool_call>list_files<arg_key>path</arg_key><arg_value>/repo</arg_value></tool_call>",
        );

        assert_eq!(presentation.role, PresentedMessageRole::Assistant);
        assert_eq!(presentation.body_markdown, "");
    }

    #[test]
    fn assistant_partial_tool_call_markup_is_hidden_from_visible_body() {
        let presentation = present_persisted_message(
            "assistant",
            "recursive</arg_key><arg_value>false</arg_value></tool_call>",
        );

        assert_eq!(presentation.role, PresentedMessageRole::Assistant);
        assert_eq!(presentation.body_markdown, "");
    }

    #[test]
    fn normalize_markdown_text_collapses_excess_blank_lines() {
        assert_eq!(
            normalize_markdown_text("line 1\n\n\nline 2\r\n\r\n\r\nline 3\n"),
            "line 1\n\nline 2\n\nline 3"
        );
    }
}
