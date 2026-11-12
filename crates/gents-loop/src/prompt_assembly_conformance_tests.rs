use gents_lean_vocab::family::recode;
use gents_protocol::message::{
    AssistantContent, Message, Reasoning, ReasoningContent, Text, ToolCall, ToolFunction,
    ToolResult, ToolResultContent, UserContent,
};
use serde::{Deserialize, Serialize};

use crate::provider_input::ProviderInputProfile;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Item {
    item: String,
    value: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Row {
    role: String,
    kind: String,
    call_ids: Vec<u64>,
    content: Vec<Item>,
}

fn call_key(id: u64) -> String {
    format!("call-{id}")
}

fn text_body(index: u64) -> String {
    format!("text-{index}")
}

fn reasoning_body(index: u64) -> String {
    format!("reasoning-{index}")
}

fn tool_call(id: u64) -> ToolCall {
    ToolCall {
        raw_arguments: None,
        id: call_key(id),
        call_id: Some(call_key(id)),
        function: ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
        signature: None,
        additional_params: None,
    }
}

fn message_of_row(row: &Row) -> Message {
    match (row.role.as_str(), row.kind.as_str()) {
        ("assistant", _) => Message::Assistant {
            request_id: None,
            id: None,
            content: row.content.iter().map(assistant_item).collect(),
        },
        ("user", "toolResult") => {
            let [id] = row.call_ids.as_slice() else {
                panic!("a toolResult row must carry exactly one call id: {row:?}");
            };
            Message::User {
                content: vec![UserContent::ToolResult(ToolResult {
                    id: call_key(*id),
                    call_id: Some(call_key(*id)),
                    content: vec![ToolResultContent::Text(Text {
                        text: format!("{}-result", call_key(*id)),
                    })],
                })],
            }
        }
        ("user", _) => Message::User {
            content: row.content.iter().map(user_item).collect(),
        },
        (role, kind) => panic!("generated row has an unmodeled role/kind: {role}/{kind}"),
    }
}

fn assistant_item(item: &Item) -> AssistantContent {
    match item.item.as_str() {
        "text" => AssistantContent::Text(Text {
            text: text_body(item.value),
        }),
        "other" => AssistantContent::Reasoning(Reasoning::new(&reasoning_body(item.value))),
        "call" => AssistantContent::ToolCall(tool_call(item.value)),
        other => panic!("generated content item has an unmodeled tag: {other}"),
    }
}

fn user_item(item: &Item) -> UserContent {
    match item.item.as_str() {
        "text" => UserContent::Text(Text {
            text: text_body(item.value),
        }),
        other => panic!("generated user content item has an unmodeled tag: {other}"),
    }
}

fn row_of_message(message: &Message) -> Row {
    match message {
        Message::Assistant { content, .. } => {
            let items = content.iter().map(assistant_item_of).collect::<Vec<_>>();
            let call_ids = items
                .iter()
                .filter(|item| item.item == "call")
                .map(|item| item.value)
                .collect::<Vec<_>>();
            Row {
                role: "assistant".to_string(),
                kind: if call_ids.is_empty() {
                    "ordinary".to_string()
                } else {
                    "assistantToolCalls".to_string()
                },
                call_ids,
                content: items,
            }
        }
        Message::User { content } => {
            let tool_results = content
                .iter()
                .filter_map(|item| match item {
                    UserContent::ToolResult(tool_result) => Some(tool_result),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if tool_results.is_empty() {
                Row {
                    role: "user".to_string(),
                    kind: "ordinary".to_string(),
                    call_ids: Vec::new(),
                    content: content.iter().map(user_item_of).collect(),
                }
            } else {
                assert_eq!(
                    tool_results.len(),
                    1,
                    "production threads exactly one tool result per user message \
                     (loop_stream.rs); the model's row projection assumes it"
                );
                Row {
                    role: "user".to_string(),
                    kind: "toolResult".to_string(),
                    call_ids: vec![parse_call_key(&result_key(tool_results[0]))],
                    content: Vec::new(),
                }
            }
        }
        Message::System { .. } => {
            panic!("the PromptAssembly model has no system-row projection")
        }
    }
}

fn assistant_item_of(content: &AssistantContent) -> Item {
    match content {
        AssistantContent::Text(text) => Item {
            item: "text".to_string(),
            value: parse_suffix(&text.text, "text-"),
        },
        AssistantContent::ToolCall(tool_call) => Item {
            item: "call".to_string(),
            value: parse_call_key(&call_key_of(tool_call)),
        },
        AssistantContent::Reasoning(reasoning) => Item {
            item: "other".to_string(),
            value: parse_suffix(&reasoning_text(reasoning), "reasoning-"),
        },
        AssistantContent::Image(_) => {
            panic!("no witness row builds an image; projection would be ambiguous")
        }
    }
}

fn user_item_of(content: &UserContent) -> Item {
    match content {
        UserContent::Text(text) => Item {
            item: "text".to_string(),
            value: parse_suffix(&text.text, "text-"),
        },
        other => panic!("no witness row builds {other:?}; projection would be ambiguous"),
    }
}

fn reasoning_text(reasoning: &Reasoning) -> String {
    reasoning
        .content
        .iter()
        .map(|block| match block {
            ReasoningContent::Text { text, .. } => text.clone(),
            other => panic!("unexpected reasoning block in a witness row: {other:?}"),
        })
        .collect::<Vec<_>>()
        .join("")
}

fn call_key_of(tool_call: &ToolCall) -> String {
    tool_call
        .call_id
        .clone()
        .unwrap_or_else(|| tool_call.id.clone())
}

fn result_key(tool_result: &ToolResult) -> String {
    tool_result
        .call_id
        .clone()
        .unwrap_or_else(|| tool_result.id.clone())
}

fn parse_call_key(key: &str) -> u64 {
    parse_suffix(key, "call-")
}

fn parse_suffix(value: &str, prefix: &str) -> u64 {
    value
        .strip_prefix(prefix)
        .and_then(|rest| rest.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("value {value:?} is not a generated {prefix}* identity"))
}

fn rows_of(messages: &[Message]) -> Vec<Row> {
    messages.iter().map(row_of_message).collect()
}

fn messages_of(rows: &[Row]) -> Vec<Message> {
    rows.iter().map(message_of_row).collect()
}

fn profile_of_order(mode: &str) -> ProviderInputProfile {
    match mode {
        "grouped" => ProviderInputProfile::OpenAiChatCompletions,
        "native_preserved" => ProviderInputProfile::ClaudeMessages,
        other => panic!("unmodeled assistant order mode {other}"),
    }
}

#[test]
fn generated_assistant_order_cases_bind_selected_provider_order() {
    use crate::compaction::history::normalize_assistant_content_order;
    use gents_lean_vocab::families::contracts_prompt_assembly_assistant_order as family;
    family::replay::<String>(|case| {
        let profile = profile_of_order(&case.order_mode);
        let input = vec![Message::Assistant {
            request_id: None,
            id: None,
            content: recode::<_, Vec<Item>>(&case.input)
                .iter()
                .map(assistant_item)
                .collect(),
        }];
        let once = normalize_assistant_content_order(profile, input);
        let [once_row] = once.as_slice() else {
            panic!("{}: one assistant message stays one: {once:?}", case.name);
        };
        let expected = row_of_message(once_row).content;
        let twice = normalize_assistant_content_order(profile, once);
        let [twice_row] = twice.as_slice() else {
            panic!("{}: one assistant message stays one: {twice:?}", case.name);
        };
        family::AssistantOrderOutput {
            expected: recode(&expected),
            twice: recode(&row_of_message(twice_row).content),
        }
    });
}

#[test]
fn generated_sanitize_cases_drive_the_production_sanitizer() {
    use crate::compaction::sanitize_history_for_provider;
    use gents_lean_vocab::families::contracts_prompt_assembly_sanitize as family;
    use gents_lean_vocab::families::contracts_prompt_assembly_sanitize_split as split;
    family::replay(|case| {
        let input = recode::<_, Vec<Row>>(&case.input);
        assert_eq!(
            rows_of(&messages_of(&input)),
            input,
            "row -> Message -> row must be the identity ({})",
            case.name
        );
        let once = sanitize_history_for_provider(
            ProviderInputProfile::OpenAiChatCompletions,
            messages_of(&input),
        );
        let twice = sanitize_history_for_provider(
            ProviderInputProfile::OpenAiChatCompletions,
            once.clone(),
        );
        family::SanitizeOutput {
            expected: recode(&rows_of(&once)),
            twice: recode(&rows_of(&twice)),
        }
    });
    split::replay(|case| {
        let input = recode::<_, Vec<Row>>(&case.input);
        let index = usize::try_from(case.index).expect("split index fits");
        let suffix = input
            .get(index..)
            .unwrap_or_else(|| panic!("{}: split {index} exceeds the input", case.name));
        recode::<_, Vec<split::PromptAssemblyRowCase>>(&rows_of(&sanitize_history_for_provider(
            ProviderInputProfile::OpenAiChatCompletions,
            messages_of(suffix),
        )))
    });
}

fn physical_replay_tag(row: usize) -> crate::claude_messages_body::ReplayTag {
    crate::claude_messages_body::ReplayTag {
        request_doc_id: "request-doc".to_string(),
        source: gents_protocol::output::OutputSource::ProviderTurn {
            scope: "inference.1".parse().expect("modeled provider scope"),
            turn_index: row.try_into().expect("witness row fits"),
            attempt: 0,
        },
    }
}

#[test]
fn generated_mode_sanitize_cases_bind_composed_provider_view() {
    use crate::compaction::sanitize_history_for_provider;
    use crate::loop_stream::{
        provider_messages, provider_view_tagged, sanitize_tagged_history, TaggedMessage,
    };
    use gents_lean_vocab::families::contracts_prompt_assembly_mode_sanitize as family;
    family::replay::<String>(|case| {
        let profile = profile_of_order(&case.order_mode);
        let input = recode::<_, Vec<Row>>(&case.input);
        let once = sanitize_history_for_provider(profile, messages_of(&input));
        let twice = sanitize_history_for_provider(profile, once.clone());

        let tagged = messages_of(&input)
            .into_iter()
            .enumerate()
            .map(|(row, message)| match &message {
                Message::Assistant { content, .. } => TaggedMessage {
                    block_indices: (0..content.len()).collect(),
                    source: Some(physical_replay_tag(row)),
                    physical_header: Some(format!("header-{row}")),
                    message,
                },
                _ => TaggedMessage::unassociated(message),
            })
            .collect::<Vec<_>>();
        let daemon = provider_view_tagged(tagged)
            .unwrap_or_else(|error| panic!("{}: daemon projection: {error}", case.name));
        let entry = sanitize_tagged_history(daemon.clone())
            .unwrap_or_else(|error| panic!("{}: loop entry projection: {error}", case.name));
        assert_eq!(entry, daemon, "{}: second tagged pass", case.name);
        assert!(
            entry
                .iter()
                .all(|row| row.source.is_some() == matches!(row.message, Message::Assistant { .. })),
            "{}",
            case.name
        );
        assert_eq!(
            rows_of(&provider_messages(profile, &entry)),
            rows_of(&once),
            "{}: the tagged provider view and the sanitizer must agree",
            case.name
        );
        let native = entry
            .iter()
            .map(|row| row.message.clone())
            .collect::<Vec<_>>();
        family::ModeSanitizeOutput {
            expected: recode(&rows_of(&once)),
            twice: recode(&rows_of(&twice)),
            association: recode(&rows_of(&native)),
            association_indices: entry
                .iter()
                .map(|row| {
                    row.block_indices
                        .iter()
                        .map(|index| u64::try_from(*index).expect("block index fits"))
                        .collect()
                })
                .collect(),
        }
    });
}
