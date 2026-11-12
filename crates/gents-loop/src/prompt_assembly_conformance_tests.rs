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
    gents_lean_vocab::families::contracts_prompt_assembly_assistant_order::replay::<String>(
        |case| {
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
            gents_lean_vocab::families::contracts_prompt_assembly_assistant_order::AssistantOrderOutput {
            expected: recode(&expected),
            twice: recode(&row_of_message(twice_row).content),
        }
        },
    );
}

#[test]
fn generated_sanitize_cases_drive_the_production_sanitizer() {
    use crate::compaction::sanitize_history_for_provider;
    gents_lean_vocab::families::contracts_prompt_assembly_sanitize::replay(|case| {
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
        gents_lean_vocab::families::contracts_prompt_assembly_sanitize::SanitizeOutput {
            expected: recode(&rows_of(&once)),
            twice: recode(&rows_of(&twice)),
        }
    });
    gents_lean_vocab::families::contracts_prompt_assembly_sanitize_split::replay(|case| {
        let input = recode::<_, Vec<Row>>(&case.input);
        let index = usize::try_from(case.index).expect("split index fits");
        let suffix = input
            .get(index..)
            .unwrap_or_else(|| panic!("{}: split {index} exceeds the input", case.name));
        recode::<_, Vec<gents_lean_vocab::families::contracts_prompt_assembly_sanitize_split::PromptAssemblyRowCase>>(&rows_of(&sanitize_history_for_provider(
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
    gents_lean_vocab::families::contracts_prompt_assembly_mode_sanitize::replay::<String>(|case| {
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
        gents_lean_vocab::families::contracts_prompt_assembly_mode_sanitize::ModeSanitizeOutput {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FlatItem {
    kind: String,
    bytes: Vec<u64>,
}

fn native_flat_items(items: &[FlatItem]) -> Vec<crate::provider_input::replay_frontier::FlatItem> {
    use crate::provider_input::replay_frontier::FlatItem as Native;
    items
        .iter()
        .map(|item| {
            let bytes = item
                .bytes
                .iter()
                .map(|byte| u8::try_from(*byte).expect("modeled byte"))
                .collect();
            match item.kind.as_str() {
                "ordinary" => Native::Ordinary(bytes),
                "reasoning" => Native::Reasoning(bytes),
                other => panic!("unmodeled flat item kind {other}"),
            }
        })
        .collect()
}

#[test]
fn generated_replay_prefix_cases_bind_acceptance_checks() {
    use crate::provider_input::replay_frontier::{anchored, drop_leading_reasoning, ords};
    gents_lean_vocab::families::contracts_prompt_assembly_replay_prefix::replay::<String>(|case| {
        let captured = native_flat_items(&recode::<_, Vec<FlatItem>>(&case.captured));
        let current = native_flat_items(&recode::<_, Vec<FlatItem>>(&case.current));
        let captured_anchored = anchored(&captured);
        let current_anchored = anchored(&current);
        gents_lean_vocab::families::contracts_prompt_assembly_replay_prefix::ReplayPrefixOutput {
            ordinary_equal: ords(&current) == ords(&captured),
            reasoning_suffix: captured_anchored.ends_with(&current_anchored),
            leading_removal: current
                == drop_leading_reasoning(
                    captured_anchored
                        .len()
                        .saturating_sub(current_anchored.len()),
                    &captured,
                ),
        }
    });
}

fn claude_request(history: &[Message]) -> rig::completion::CompletionRequest {
    rig::completion::CompletionRequest {
        model: None,
        preamble: None,
        chat_history: rig::OneOrMany::many(crate::rig_compat::to_rig_messages(history))
            .expect("at least one history row"),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(128),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    }
}

fn native_messages_body(
    history: &[Message],
) -> Result<serde_json::Value, crate::claude_messages_body::MessagesBodyError> {
    if history.is_empty() {
        return Ok(serde_json::json!({"messages": []}));
    }
    crate::claude_messages_body::build_messages_body("claude-sonnet-5", &claude_request(history))
}

fn replay_codec_outcome(error: &crate::claude_messages_body::AssistantCodecError) -> &'static str {
    use crate::claude_messages_body::AssistantCodecError as Error;
    match error {
        Error::UnsupportedImage => "unsupportedReplayBlock",
        Error::MissingSignature => "missingSignature",
        Error::EmptyRedacted => "malformedRedacted",
        Error::UnsupportedReasoning => "unsupportedReasoning",
        Error::UnencodableToolInput => "unencodableToolInput",
    }
}

fn body_replay_blocks(message: &serde_json::Value) -> Vec<gents_lean_vocab::claude_replay::Block> {
    let blocks = message["content"]
        .as_array()
        .unwrap_or_else(|| panic!("assistant content: {message}"));
    assert!(
        !blocks.is_empty(),
        "an assistant message is never sent empty: {message}"
    );
    blocks
        .iter()
        .map(|block| {
            let mut block = block.clone();
            block
                .as_object_mut()
                .expect("content object")
                .remove("cache_control");
            gents_lean_vocab::claude_replay::Block::of_native_json(&block)
        })
        .collect()
}

#[test]
fn generated_claude_replay_cases_drive_native_messages_body() {
    gents_lean_vocab::families::contracts_prompt_assembly_claude_replay::replay(|case| {
        let content = gents_lean_vocab::claude_replay::assistant_content(&recode::<
            _,
            Vec<gents_lean_vocab::claude_replay::InputBlock>,
        >(&case.blocks));
        let history = [Message::Assistant {
            request_id: None,
            id: None,
            content,
        }];
        let (outcome, replay) = match native_messages_body(&history) {
            Err(crate::claude_messages_body::MessagesBodyError::Codec(error)) => {
                (replay_codec_outcome(&error).to_owned(), Vec::new())
            }
            Err(other) => panic!("{}: replay failed outside the codec: {other}", case.name),
            Ok(body) => {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                assert!(
                    messages.len() <= 1,
                    "{}: one assistant turn: {body}",
                    case.name
                );
                (
                    "ok".to_owned(),
                    messages.first().map(body_replay_blocks).unwrap_or_default(),
                )
            }
        };
        gents_lean_vocab::families::contracts_prompt_assembly_claude_replay::ClaudeReplayOutput {
            outcome,
            replay: recode(&replay),
        }
    });
}

fn native_replay_tag(
    tag: &gents_lean_vocab::claude_replay::Tag,
) -> crate::claude_messages_body::ReplayTag {
    use gents_protocol::output::OutputSource;
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
    let source = &tag.source;
    let number = |value: Option<u64>, name: &str| {
        value.unwrap_or_else(|| panic!("modeled {} source lacks {name}", source.kind))
    };
    let turn = || u32::try_from(number(source.turn, "turn")).expect("modeled turn fits");
    let attempt =
        || u32::try_from(number(source.attempt, "attempt")).expect("modeled attempt fits");
    let native = match source.kind.as_str() {
        "provider" => OutputSource::ProviderTurn {
            scope: format!(
                "inference.{}",
                number(source.scope, "scope")
                    .checked_add(1)
                    .expect("scope fits")
            )
            .parse()
            .expect("modeled provider scope"),
            turn_index: turn(),
            attempt: attempt(),
        },
        "auxiliary" => OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: match source.auxiliary_kind.as_deref() {
                    Some("compaction") => CaptureScopeKind::Compaction,
                    Some("compaction_fallback") => CaptureScopeKind::CompactionFallback,
                    Some("title") => CaptureScopeKind::Title,
                    Some("access_review") => CaptureScopeKind::AccessReview,
                    other => panic!("unmodeled auxiliary kind {other:?}"),
                },
                seq: number(source.scope, "scope"),
            },
            turn_index: turn(),
            attempt: attempt(),
        },
        "tool" => OutputSource::ToolCall {
            tool_call_doc_id: format!("tool-{}", number(source.call, "call")),
        },
        "authored" => OutputSource::Authored {
            key: format!("authored-{}", number(source.key, "key")),
        },
        other => panic!("unmodeled replay source {other}"),
    };
    crate::claude_messages_body::ReplayTag {
        request_doc_id: format!("request-{}", tag.request),
        source: native,
    }
}

fn tag_of(
    native: &crate::claude_messages_body::ReplayTag,
    known: &[gents_lean_vocab::claude_replay::Tag],
) -> gents_lean_vocab::claude_replay::Tag {
    known
        .iter()
        .find(|tag| native_replay_tag(tag) == *native)
        .cloned()
        .unwrap_or_else(|| panic!("production invented the replay tag {native:?}"))
}

fn known_tags(
    rows: &[gents_lean_vocab::claude_replay::Row],
) -> Vec<gents_lean_vocab::claude_replay::Tag> {
    rows.iter().filter_map(|row| row.source.clone()).collect()
}

fn known_blocks(
    rows: &[gents_lean_vocab::claude_replay::Row],
) -> Vec<gents_lean_vocab::claude_replay::InputBlock> {
    rows.iter().flat_map(|row| row.blocks.clone()).collect()
}

fn wire_blocks(
    content: &[AssistantContent],
    known: &[gents_lean_vocab::claude_replay::InputBlock],
) -> Vec<gents_lean_vocab::claude_replay::InputBlock> {
    let blocks = gents_lean_vocab::claude_replay::input_blocks_of(content, known);
    assert_eq!(
        gents_lean_vocab::claude_replay::assistant_content(&blocks),
        content,
        "the wire form of production content renders back to it"
    );
    blocks
}

fn native_tagged_message(
    row: &gents_lean_vocab::claude_replay::Row,
) -> crate::loop_stream::TaggedMessage {
    crate::loop_stream::TaggedMessage {
        message: Message::Assistant {
            request_id: None,
            id: None,
            content: gents_lean_vocab::claude_replay::assistant_content(&row.blocks),
        },
        source: row.source.as_ref().map(native_replay_tag),
        physical_header: row.physical_header.clone(),
        block_indices: row.block_indices(),
    }
}

fn row_of_tagged_message(
    tagged: &crate::loop_stream::TaggedMessage,
    known: &[gents_lean_vocab::claude_replay::Row],
) -> gents_lean_vocab::claude_replay::Row {
    let Message::Assistant {
        request_id: None,
        id: None,
        content,
    } = &tagged.message
    else {
        panic!("a tagged replay row is an assistant message without ids: {tagged:?}");
    };
    gents_lean_vocab::claude_replay::Row {
        source: tagged
            .source
            .as_ref()
            .map(|tag| tag_of(tag, &known_tags(known))),
        physical_header: tagged.physical_header.clone(),
        block_indices: tagged
            .block_indices
            .iter()
            .map(|index| u64::try_from(*index).expect("block index fits"))
            .collect(),
        blocks: wire_blocks(content, &known_blocks(known)),
    }
}

fn replay_checkpoint_outcome(
    error: &crate::claude_messages_body::ReplayCheckpointError,
) -> &'static str {
    use crate::claude_messages_body::ReplayCheckpointError as Error;
    match error {
        Error::InvalidSplit => "invalidReplaySplit",
        Error::InvalidAssociation => "invalidReplayAssociation",
        Error::DuplicateAssociation => "duplicateReplayAssociation",
        Error::RequiredInPrefix => "requiredReplayInPrefix",
        Error::MissingRequired => "missingRequiredReplay",
        Error::Codec(error) => replay_codec_outcome(error),
    }
}

#[test]
fn generated_replay_shape_cases_bind_source_index_projection() {
    use crate::loop_stream::select_tagged_assistant_blocks;
    gents_lean_vocab::families::contracts_prompt_assembly_replay_shape::replay(|case| {
        let source_row: gents_lean_vocab::claude_replay::Row = recode(&case.source);
        let retained = case
            .retained_indices
            .iter()
            .map(|index| usize::try_from(*index).expect("index fits"))
            .collect::<Vec<_>>();
        match select_tagged_assistant_blocks(&native_tagged_message(&source_row), &retained) {
            Err(error) => gents_lean_vocab::families::contracts_prompt_assembly_replay_shape::ReplayShapeOutput {
                outcome: replay_checkpoint_outcome(&error).to_owned(),
                shaped: None,
            },
            Ok(shaped) => gents_lean_vocab::families::contracts_prompt_assembly_replay_shape::ReplayShapeOutput {
                outcome: "ok".to_owned(),
                shaped: Some(recode(&row_of_tagged_message(
                    &shaped,
                    std::slice::from_ref(&source_row),
                ))),
            },
        }
    });
}

fn wire_assistant_rows(
    rows: &[crate::claude_messages_body::TaggedAssistantRow],
    known: &[gents_lean_vocab::claude_replay::Row],
    tags: &[gents_lean_vocab::claude_replay::Tag],
) -> Vec<(gents_lean_vocab::claude_replay::Row, Option<String>)> {
    rows.iter()
        .map(|row| {
            (
                gents_lean_vocab::claude_replay::Row {
                    source: row.source.as_ref().map(|tag| tag_of(tag, tags)),
                    physical_header: row.physical_header.clone(),
                    block_indices: row
                        .block_indices
                        .iter()
                        .map(|index| u64::try_from(*index).expect("block index fits"))
                        .collect(),
                    blocks: wire_blocks(&row.content, &known_blocks(known)),
                },
                row.id.clone(),
            )
        })
        .collect()
}

#[test]
fn generated_claude_checkpoint_cases_bind_selected_assistant_projection() {
    use crate::claude_messages_body::{
        prepare_replay_checkpoint, restore_contiguous_replay, TaggedAssistantRow,
    };
    gents_lean_vocab::families::contracts_prompt_assembly_claude_checkpoint::replay(|case| {
        let rows: Vec<gents_lean_vocab::claude_replay::Row> = recode(&case.rows);
        let required: Vec<gents_lean_vocab::claude_replay::Tag> = recode(&case.required);
        assert_eq!(case.carrier_ids.len(), rows.len(), "{}", case.name);
        let mut tags = known_tags(&rows);
        tags.extend(required.iter().cloned());
        let native_rows = rows
            .iter()
            .zip(&case.carrier_ids)
            .map(|(row, id)| TaggedAssistantRow {
                source: row.source.as_ref().map(native_replay_tag),
                physical_header: row.physical_header.clone(),
                block_indices: row.block_indices(),
                id: Some(id.clone()),
                content: gents_lean_vocab::claude_replay::assistant_content(&row.blocks),
            })
            .collect();
        let checkpoint = match prepare_replay_checkpoint(
            required.iter().map(native_replay_tag).collect(),
            native_rows,
            usize::try_from(case.split).expect("split fits"),
        ) {
            Err(error) => {
                return gents_lean_vocab::families::contracts_prompt_assembly_claude_checkpoint::ClaudeCheckpointOutput {
                    outcome: replay_checkpoint_outcome(&error).to_owned(),
                    prefix_rows: Vec::new(),
                    prefix_carriers: Vec::new(),
                    retained: Vec::new(),
                    retained_carriers: Vec::new(),
                    replay: Vec::new(),
                };
            }
            Ok(checkpoint) => checkpoint,
        };
        let rows_wire = rows.clone();
        let carried = |rows: &[TaggedAssistantRow]| {
            let (rows, carriers): (Vec<_>, Vec<_>) = wire_assistant_rows(rows, &rows_wire, &tags)
                .into_iter()
                .unzip();
            (
                rows,
                carriers
                    .into_iter()
                    .map(|carrier| {
                        carrier
                            .unwrap_or_else(|| panic!("{}: a row lost its message id", case.name))
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let (prefix_rows, prefix_carriers) = carried(&checkpoint.prefix_rows);
        let (retained, retained_carriers) = carried(&checkpoint.retained);
        let (outcome, replay) = match restore_contiguous_replay(&checkpoint) {
            Err(error) => (replay_checkpoint_outcome(&error).to_owned(), Vec::new()),
            Ok(prepared) => {
                let native = checkpoint
                    .retained
                    .iter()
                    .map(|row| Message::Assistant {
                        request_id: None,
                        id: row.id.clone(),
                        content: row.content.clone(),
                    })
                    .collect::<Vec<_>>();
                let body = native_messages_body(&native)
                    .unwrap_or_else(|error| panic!("{}: {error}", case.name));
                let sent = body["messages"]
                    .as_array()
                    .expect("messages")
                    .iter()
                    .map(body_replay_blocks)
                    .collect::<Vec<_>>();
                let replay = prepared
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(gents_lean_vocab::claude_replay::Block::of_native_json)
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    sent,
                    replay
                        .iter()
                        .filter(|row| !row.is_empty())
                        .cloned()
                        .collect::<Vec<_>>(),
                    "{}: the Messages body sends the restored replay",
                    case.name
                );
                ("ok".to_owned(), replay)
            }
        };
        gents_lean_vocab::families::contracts_prompt_assembly_claude_checkpoint::ClaudeCheckpointOutput {
            outcome,
            prefix_rows: recode(&prefix_rows),
            prefix_carriers,
            retained: recode(&retained),
            retained_carriers,
            replay: recode(&replay),
        }
    });
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ReasoningWitness {
    block_index: u64,
    parts: Vec<gents_lean_vocab::claude_replay::Part>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Resolution {
    tag: gents_lean_vocab::claude_replay::Tag,
    origin: String,
    issuer_family: String,
    issuer_endpoint: String,
    wire: String,
    physical_header: String,
    complete: bool,
    captured: Option<Vec<FlatItem>>,
    expected_reasoning: Vec<ReasoningWitness>,
}

fn native_replay_wire(wire: &str) -> crate::claude_messages_body::ReplayWire {
    use crate::claude_messages_body::ReplayWire;
    match wire {
        "claude_messages" => ReplayWire::ClaudeMessages,
        "responses" => ReplayWire::Responses,
        other => panic!("unmodeled replay wire {other}"),
    }
}

fn native_replay_resolution(
    entry: &Resolution,
) -> crate::claude_messages_body::ResolvedReplayEvidence {
    use crate::claude_messages_body::{ReplayIssuer, ReplayOrigin, ResolvedReplayEvidence};
    ResolvedReplayEvidence {
        origin: match entry.origin.as_str() {
            "claudeSubscription" => ReplayOrigin::ClaudeSubscription,
            "acceptedProvider" => ReplayOrigin::AcceptedProvider,
            "recoveredProvider" => ReplayOrigin::RecoveredProvider,
            "foreign" => ReplayOrigin::Foreign,
            "missing" => ReplayOrigin::Missing,
            "ambiguous" => ReplayOrigin::Ambiguous,
            other => panic!("unmodeled replay origin {other}"),
        },
        issuer: ReplayIssuer {
            family: entry.issuer_family.clone(),
            endpoint: entry.issuer_endpoint.clone(),
        },
        wire: native_replay_wire(&entry.wire),
        physical_header: entry.physical_header.clone(),
        complete: entry.complete,
        captured: entry.captured.as_deref().map(native_flat_items),
        reasoning: entry
            .expected_reasoning
            .iter()
            .map(|block| {
                (
                    usize::try_from(block.block_index).expect("block index fits"),
                    gents_lean_vocab::claude_replay::reasoning_parts(&block.parts),
                )
            })
            .collect(),
    }
}

#[test]
fn generated_reasoning_suffix_cases_bind_native_selection() {
    use crate::claude_messages_body::{
        project_recovered_prefix, replay_stage, select_replay, ReplayIssuer, TaggedAssistantRow,
    };
    gents_lean_vocab::families::contracts_prompt_assembly_reasoning_suffix::replay(|case| {
        let rows: Vec<gents_lean_vocab::claude_replay::Row> = recode(&case.rows);
        let entries: Vec<Resolution> = recode(&case.resolutions);
        let native_rows = rows
            .iter()
            .map(|row| TaggedAssistantRow {
                source: row.source.as_ref().map(native_replay_tag),
                physical_header: row.physical_header.clone(),
                block_indices: row.block_indices(),
                id: None,
                content: gents_lean_vocab::claude_replay::assistant_content(&row.blocks),
            })
            .collect::<Vec<_>>();
        let issuer = ReplayIssuer {
            family: case.issuer_family.clone(),
            endpoint: case.issuer_endpoint.clone(),
        };
        let wire = native_replay_wire(&case.wire);
        let resolutions = entries
            .iter()
            .map(|entry| {
                (
                    native_replay_tag(&entry.tag),
                    native_replay_resolution(entry),
                )
            })
            .collect::<Vec<_>>();
        let resolve = |tag: &crate::claude_messages_body::ReplayTag| {
            resolutions
                .iter()
                .filter(|(candidate, _)| candidate == tag)
                .map(|(_, evidence)| evidence.clone())
                .collect::<Vec<_>>()
        };
        let projected = project_recovered_prefix(&native_rows, resolve);
        let stage = replay_stage(&projected, &issuer, wire, resolve);
        let replayed = select_replay(
            &projected,
            &issuer,
            wire,
            resolve,
            &native_flat_items(&recode::<_, Vec<FlatItem>>(&case.assembled)),
        );
        let mut tags = known_tags(&rows);
        tags.extend(entries.iter().map(|entry| entry.tag.clone()));
        let wire_rows = |native: &[TaggedAssistantRow]| {
            wire_assistant_rows(native, &rows, &tags)
                .into_iter()
                .map(|(row, id)| {
                    assert_eq!(id, None, "{}: replay rows carry no message id", case.name);
                    row
                })
                .collect::<Vec<_>>()
        };
        gents_lean_vocab::families::contracts_prompt_assembly_reasoning_suffix::ReasoningSuffixOutput {
            stage: recode(&wire_rows(&stage)),
            replay: recode(&wire_rows(&replayed)),
        }
    });
}
