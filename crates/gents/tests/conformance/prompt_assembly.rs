use gents_inference::compaction::sanitize_history_for_provider;
use gents_inference::llm::message::{
    AssistantContent, Message, Reasoning, Text, ToolCall, ToolFunction, ToolResult,
    ToolResultContent, UserContent,
};
use gents_loop::claude_messages_body::MessagesBodyError;
use gents_loop::provider_input::ProviderInputProfile;

use gents_lean_vocab::{
    lean_prompt_assembly_claude_replay_cases, lean_prompt_assembly_sanitize_cases,
    native_claude_reasoning_parts, native_claude_replay_content, LeanClaudeReplayBlock,
    LeanPromptAssemblyItem, LeanPromptAssemblyRow,
};

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

fn message_of_row(row: &LeanPromptAssemblyRow) -> Message {
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

fn assistant_item(item: &LeanPromptAssemblyItem) -> AssistantContent {
    match item.item.as_str() {
        "text" => AssistantContent::Text(Text {
            text: text_body(item.value),
        }),
        "other" => AssistantContent::Reasoning(Reasoning::new(&reasoning_body(item.value))),
        "call" => AssistantContent::ToolCall(tool_call(item.value)),
        other => panic!("generated content item has an unmodeled tag: {other}"),
    }
}

fn user_item(item: &LeanPromptAssemblyItem) -> UserContent {
    match item.item.as_str() {
        "text" => UserContent::Text(Text {
            text: text_body(item.value),
        }),
        other => panic!("generated user content item has an unmodeled tag: {other}"),
    }
}

fn row_of_message(message: &Message) -> LeanPromptAssemblyRow {
    match message {
        Message::Assistant { content, .. } => {
            let items = content.iter().map(assistant_item_of).collect::<Vec<_>>();
            let call_ids = items
                .iter()
                .filter(|item| item.item == "call")
                .map(|item| item.value)
                .collect::<Vec<_>>();
            LeanPromptAssemblyRow {
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
                LeanPromptAssemblyRow {
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
                LeanPromptAssemblyRow {
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

fn assistant_item_of(content: &AssistantContent) -> LeanPromptAssemblyItem {
    match content {
        AssistantContent::Text(text) => LeanPromptAssemblyItem {
            item: "text".to_string(),
            value: parse_suffix(&text.text, "text-"),
        },
        AssistantContent::ToolCall(tool_call) => LeanPromptAssemblyItem {
            item: "call".to_string(),
            value: parse_call_key(&call_key_of(tool_call)),
        },
        AssistantContent::Reasoning(reasoning) => LeanPromptAssemblyItem {
            item: "other".to_string(),
            value: parse_suffix(&reasoning_text(reasoning), "reasoning-"),
        },
        AssistantContent::Image(_) => {
            panic!("no witness row builds an image; projection would be ambiguous")
        }
    }
}

fn user_item_of(content: &UserContent) -> LeanPromptAssemblyItem {
    match content {
        UserContent::Text(text) => LeanPromptAssemblyItem {
            item: "text".to_string(),
            value: parse_suffix(&text.text, "text-"),
        },
        other => panic!("no witness row builds {other:?}; projection would be ambiguous"),
    }
}

fn reasoning_text(reasoning: &Reasoning) -> String {
    use gents_inference::llm::message::ReasoningContent;
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

fn rows_of(messages: &[Message]) -> Vec<LeanPromptAssemblyRow> {
    messages.iter().map(row_of_message).collect()
}

fn messages_of(rows: &[LeanPromptAssemblyRow]) -> Vec<Message> {
    rows.iter().map(message_of_row).collect()
}

#[test]
fn generated_rows_round_trip_through_production_messages() {
    let cases = lean_prompt_assembly_sanitize_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly sanitize cases"
    );
    for case in cases {
        for rows in [&case.input, &case.expected, &case.expected_twice] {
            assert_eq!(
                &rows_of(&messages_of(rows)),
                rows,
                "row -> Message -> row must be the identity ({})",
                case.name
            );
        }
    }
}

#[test]
fn generated_sanitize_cases_drive_the_production_sanitizer() {
    let cases = lean_prompt_assembly_sanitize_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly sanitize cases"
    );

    for case in cases {
        let once = sanitize_history_for_provider(
            ProviderInputProfile::OpenAiChatCompletions,
            messages_of(&case.input),
        );
        assert_eq!(
            rows_of(&once),
            case.expected,
            "sanitize disagrees with the Lean model on case {:?}",
            case.name
        );

        let twice = sanitize_history_for_provider(
            ProviderInputProfile::OpenAiChatCompletions,
            once.clone(),
        );
        assert_eq!(
            rows_of(&twice),
            case.expected_twice,
            "sanitize is not idempotent on case {:?}",
            case.name
        );

        for split in &case.splits {
            assert!(
                split.index <= case.input.len(),
                "generated split index {} exceeds input length on case {:?}",
                split.index,
                case.name
            );
            let suffix = messages_of(&case.input[split.index..]);
            assert_eq!(
                rows_of(&sanitize_history_for_provider(
                    ProviderInputProfile::OpenAiChatCompletions,
                    suffix
                )),
                split.expected,
                "sanitize disagrees with the Lean model on case {:?} at split {}",
                case.name,
                split.index
            );
        }
    }
}

fn profile_of_order(mode: gents_lean_vocab::LeanAssistantOrderMode) -> ProviderInputProfile {
    match mode {
        gents_lean_vocab::LeanAssistantOrderMode::Grouped => {
            ProviderInputProfile::OpenAiChatCompletions
        }
        gents_lean_vocab::LeanAssistantOrderMode::NativePreserved => {
            ProviderInputProfile::ClaudeMessages
        }
    }
}

#[test]
fn generated_mode_sanitize_cases_bind_composed_provider_view() {
    let cases = gents_lean_vocab::lean_prompt_assembly_mode_sanitize_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let profile = profile_of_order(case.order_mode);
        let once = sanitize_history_for_provider(profile, messages_of(&case.input));
        assert_eq!(rows_of(&once), case.expected, "{}", case.name);
        let twice = sanitize_history_for_provider(profile, once);
        assert_eq!(rows_of(&twice), case.expected_twice, "{}", case.name);

        let tagged = messages_of(&case.input)
            .into_iter()
            .enumerate()
            .map(|(row, message)| match &message {
                Message::Assistant { content, .. } => gents_loop::loop_stream::TaggedMessage {
                    block_indices: (0..content.len()).collect(),
                    source: Some(physical_replay_tag(row)),
                    physical_header: Some(format!("header-{row}")),
                    message,
                },
                _ => gents_loop::loop_stream::TaggedMessage::unassociated(message),
            })
            .collect::<Vec<_>>();
        let daemon = gents_loop::loop_stream::provider_view_tagged(tagged)
            .unwrap_or_else(|error| panic!("{}: daemon projection: {error}", case.name));
        let entry = gents_loop::loop_stream::sanitize_tagged_history(daemon.clone())
            .unwrap_or_else(|error| panic!("{}: loop entry projection: {error}", case.name));
        assert_eq!(entry, daemon, "{}: second tagged pass", case.name);
        let native = entry
            .iter()
            .map(|row| row.message.clone())
            .collect::<Vec<_>>();
        assert_eq!(rows_of(&native), case.association, "{}", case.name);
        assert_eq!(
            entry
                .iter()
                .map(|row| row.block_indices.clone())
                .collect::<Vec<_>>(),
            case.association_indices,
            "{}",
            case.name
        );
        assert!(
            entry.iter().all(|row| row.source.is_some()
                == matches!(row.message, Message::Assistant { .. })),
            "{}",
            case.name
        );
        assert_eq!(
            rows_of(&gents_loop::loop_stream::provider_messages(profile, &entry)),
            case.expected,
            "{}",
            case.name
        );
    }
}

fn physical_replay_tag(row: usize) -> gents_loop::claude_messages_body::ReplayTag {
    gents_loop::claude_messages_body::ReplayTag {
        request_doc_id: "request-doc".to_string(),
        source: gents_protocol::output::OutputSource::ProviderTurn {
            scope: "inference.1".parse().expect("modeled provider scope"),
            turn_index: row.try_into().expect("witness row fits"),
            attempt: 0,
        },
    }
}

fn native_replay_tag(
    tag: &gents_lean_vocab::LeanCanonicalCoordinate,
) -> gents_loop::claude_messages_body::ReplayTag {
    use gents_lean_vocab::LeanCanonicalSource;
    use gents_protocol::output::OutputSource;
    let source = match &tag.source {
        LeanCanonicalSource::Provider {
            scope,
            turn,
            attempt,
        } => OutputSource::ProviderTurn {
            scope: format!("inference.{}", scope.checked_add(1).expect("scope fits"))
                .parse()
                .expect("modeled provider scope"),
            turn_index: (*turn).try_into().expect("modeled turn fits"),
            attempt: (*attempt).try_into().expect("modeled attempt fits"),
        },
        LeanCanonicalSource::Tool { call } => OutputSource::ToolCall {
            tool_call_doc_id: format!("tool-{call}"),
        },
        LeanCanonicalSource::Auxiliary {
            auxiliary_kind,
            scope,
            turn,
            attempt,
        } => OutputSource::ProviderTurn {
            scope: gents_protocol::rendered_request::CaptureScope {
                kind: match auxiliary_kind {
                    gents_lean_vocab::LeanAuxiliaryKind::Compaction => {
                        gents_protocol::rendered_request::CaptureScopeKind::Compaction
                    }
                    gents_lean_vocab::LeanAuxiliaryKind::CompactionFallback => {
                        gents_protocol::rendered_request::CaptureScopeKind::CompactionFallback
                    }
                    gents_lean_vocab::LeanAuxiliaryKind::Title => {
                        gents_protocol::rendered_request::CaptureScopeKind::Title
                    }
                    gents_lean_vocab::LeanAuxiliaryKind::AccessReview => {
                        gents_protocol::rendered_request::CaptureScopeKind::AccessReview
                    }
                },
                seq: *scope,
            },
            turn_index: (*turn).try_into().expect("modeled turn fits"),
            attempt: (*attempt).try_into().expect("modeled attempt fits"),
        },
        LeanCanonicalSource::Authored { key } => OutputSource::Authored {
            key: format!("authored-{key}"),
        },
    };
    gents_loop::claude_messages_body::ReplayTag {
        request_doc_id: format!("request-{}", tag.request),
        source,
    }
}

fn native_replay_origin(
    origin: &gents_lean_vocab::LeanClaudeReplayOrigin,
) -> gents_loop::claude_messages_body::ReplayOrigin {
    use gents_lean_vocab::LeanClaudeReplayOrigin as Lean;
    use gents_loop::claude_messages_body::ReplayOrigin as Native;
    match origin {
        Lean::ClaudeSubscription => Native::ClaudeSubscription,
        Lean::AcceptedProvider => Native::AcceptedProvider,
        Lean::RecoveredProvider => Native::RecoveredProvider,
        Lean::Foreign => Native::Foreign,
        Lean::Missing => Native::Missing,
        Lean::Ambiguous => Native::Ambiguous,
    }
}

fn native_replay_wire(
    wire: gents_lean_vocab::LeanReasoningReplayWire,
) -> gents_loop::claude_messages_body::ReplayWire {
    use gents_lean_vocab::LeanReasoningReplayWire as Lean;
    use gents_loop::claude_messages_body::ReplayWire as Native;
    match wire {
        Lean::ClaudeMessages => Native::ClaudeMessages,
        Lean::Responses => Native::Responses,
    }
}

fn native_replay_resolution(
    entry: &gents_lean_vocab::LeanClaudeCheckpointResolution,
) -> gents_loop::claude_messages_body::ResolvedReplayEvidence {
    use gents_loop::claude_messages_body::{ReplayIssuer, ResolvedReplayEvidence};
    ResolvedReplayEvidence {
        origin: native_replay_origin(&entry.origin),
        issuer: ReplayIssuer {
            family: entry.issuer_family.clone(),
            endpoint: entry.issuer_endpoint.clone(),
        },
        wire: native_replay_wire(entry.wire),
        physical_header: entry.physical_header.clone(),
        complete: entry.complete,
        captured: entry.captured.as_deref().map(native_flat_items),
        reasoning: entry
            .expected_reasoning
            .iter()
            .map(|block| {
                (
                    block.block_index,
                    native_claude_reasoning_parts(&block.parts),
                )
            })
            .collect(),
    }
}

fn native_flat_items(
    items: &[gents_lean_vocab::LeanReplayFlatItem],
) -> Vec<gents_loop::provider_input::replay_frontier::FlatItem> {
    use gents_lean_vocab::LeanReplayFlatKind;
    use gents_loop::provider_input::replay_frontier::FlatItem;
    items
        .iter()
        .map(|item| match item.kind {
            LeanReplayFlatKind::Ordinary => FlatItem::Ordinary(item.bytes.clone()),
            LeanReplayFlatKind::Reasoning => FlatItem::Reasoning(item.bytes.clone()),
        })
        .collect()
}

fn native_tagged_replay_row(
    row: &gents_lean_vocab::LeanClaudeTaggedReplayRow,
) -> gents_loop::claude_messages_body::TaggedAssistantRow {
    gents_loop::claude_messages_body::TaggedAssistantRow {
        source: row.source.as_ref().map(native_replay_tag),
        physical_header: row.physical_header.clone(),
        block_indices: row.block_indices.clone(),
        id: None,
        content: native_claude_replay_content(&row.blocks),
    }
}

fn native_tagged_message(
    row: &gents_lean_vocab::LeanClaudeTaggedReplayRow,
) -> gents_loop::loop_stream::TaggedMessage {
    gents_loop::loop_stream::TaggedMessage {
        message: Message::Assistant {
            request_id: None,
            id: None,
            content: native_claude_replay_content(&row.blocks),
        },
        source: row.source.as_ref().map(native_replay_tag),
        physical_header: row.physical_header.clone(),
        block_indices: row.block_indices.clone(),
    }
}

fn native_modeled_provider_view(
    messages: &[Message],
    rows: &[gents_lean_vocab::LeanClaudeTaggedReplayRow],
) -> Vec<gents_loop::loop_stream::TaggedMessage> {
    let mut modeled = rows.iter();
    let mut announced: std::collections::VecDeque<ToolCall> = std::collections::VecDeque::new();
    let mut view = Vec::new();
    for message in messages {
        match message {
            Message::Assistant { .. } => {
                let tagged =
                    native_tagged_message(modeled.next().expect("one modeled row per assistant"));
                if let Message::Assistant { content, .. } = &tagged.message {
                    announced = content
                        .iter()
                        .filter_map(|item| match item {
                            AssistantContent::ToolCall(call) => Some(call.clone()),
                            _ => None,
                        })
                        .collect();
                }
                view.push(tagged);
            }
            Message::User { content } => {
                let content = content
                    .iter()
                    .map(|item| match item {
                        UserContent::ToolResult(result) => {
                            let call = announced
                                .pop_front()
                                .expect("a modeled tool call for every fixture tool result");
                            UserContent::ToolResult(ToolResult {
                                id: call.id,
                                call_id: call.call_id,
                                content: result.content.clone(),
                            })
                        }
                        other => other.clone(),
                    })
                    .collect();
                view.push(gents_loop::loop_stream::TaggedMessage::unassociated(
                    Message::User { content },
                ));
            }
            other => view.push(gents_loop::loop_stream::TaggedMessage::unassociated(
                other.clone(),
            )),
        }
    }
    assert!(
        modeled.next().is_none(),
        "every modeled row needs an assistant position"
    );
    view
}

#[test]
fn generated_replay_shape_cases_bind_source_index_projection() {
    use gents_lean_vocab::lean_prompt_assembly_replay_shape_cases;
    use gents_loop::loop_stream::select_tagged_assistant_blocks;

    let cases = lean_prompt_assembly_replay_shape_cases();
    assert!(!cases.is_empty(), "Lean emitted no replay-shape cases");
    for case in cases {
        let source = native_tagged_message(&case.source);
        match select_tagged_assistant_blocks(&source, &case.retained_indices) {
            Err(error) => {
                assert_eq!(
                    replay_checkpoint_outcome(&error),
                    case.outcome,
                    "{}",
                    case.name
                );
                assert!(case.shaped.is_none(), "{}", case.name);
            }
            Ok(shaped) => {
                assert_eq!(case.outcome, "ok", "{}", case.name);
                assert_eq!(
                    shaped,
                    native_tagged_message(case.shaped.as_ref().expect("modeled shaped row")),
                    "{}",
                    case.name
                );
            }
        }
    }
}

#[test]
fn generated_replay_prefix_cases_bind_acceptance_checks() {
    use gents_lean_vocab::lean_prompt_assembly_replay_prefix_cases;
    use gents_loop::provider_input::replay_frontier::{anchored, drop_leading_reasoning, ords};

    let cases = lean_prompt_assembly_replay_prefix_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let captured = native_flat_items(&case.captured);
        let current = native_flat_items(&case.current);
        let captured_anchored = anchored(&captured);
        let current_anchored = anchored(&current);
        assert_eq!(
            ords(&current) == ords(&captured),
            case.ordinary_equal,
            "{}",
            case.name
        );
        assert_eq!(
            captured_anchored.ends_with(&current_anchored),
            case.reasoning_suffix,
            "{}",
            case.name
        );
        assert_eq!(
            current
                == drop_leading_reasoning(
                    captured_anchored
                        .len()
                        .saturating_sub(current_anchored.len()),
                    &captured
                ),
            case.leading_removal,
            "{}",
            case.name
        );
    }
}

#[test]
fn generated_reasoning_suffix_cases_bind_native_selection() {
    use gents_lean_vocab::lean_prompt_assembly_reasoning_suffix_cases;
    use gents_loop::claude_messages_body::{replay_stage, select_replay, ReplayIssuer};

    let cases = lean_prompt_assembly_reasoning_suffix_cases();
    assert!(
        cases.iter().any(|case| case.name.contains("/before-turn-")),
        "Lean emitted no replay scenario steps"
    );
    for case in cases {
        let rows = case
            .rows
            .iter()
            .map(native_tagged_replay_row)
            .collect::<Vec<_>>();
        let issuer = ReplayIssuer {
            family: case.issuer_family.clone(),
            endpoint: case.issuer_endpoint.clone(),
        };
        let wire = native_replay_wire(case.wire);
        let resolutions = case
            .resolutions
            .iter()
            .map(|entry| {
                (
                    native_replay_tag(&entry.tag),
                    native_replay_resolution(entry),
                )
            })
            .collect::<Vec<_>>();
        let resolve = |tag: &gents_loop::claude_messages_body::ReplayTag| {
            resolutions
                .iter()
                .filter(|(candidate, _)| candidate == tag)
                .map(|(_, evidence)| evidence.clone())
                .collect::<Vec<_>>()
        };
        let rows = gents_loop::claude_messages_body::project_recovered_prefix(&rows, resolve);
        let stage = replay_stage(&rows, &issuer, wire, resolve);
        let expected_stage = case
            .stage
            .iter()
            .map(native_tagged_replay_row)
            .collect::<Vec<_>>();
        assert_eq!(stage, expected_stage, "{}: stage rows", case.name);
        let actual = select_replay(
            &rows,
            &issuer,
            wire,
            resolve,
            &native_flat_items(&case.assembled),
        );
        let expected = case
            .replay
            .iter()
            .map(native_tagged_replay_row)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{}", case.name);
    }
}

fn replay_codec_outcome(
    error: &gents_loop::claude_messages_body::AssistantCodecError,
) -> &'static str {
    use gents_loop::claude_messages_body::AssistantCodecError as Error;
    match error {
        Error::UnsupportedImage => "unsupportedReplayBlock",
        Error::MissingSignature => "missingSignature",
        Error::EmptyRedacted => "malformedRedacted",
        Error::UnsupportedReasoning => "unsupportedReasoning",
        Error::UnencodableToolInput => "unencodableToolInput",
    }
}

fn claude_request(
    preamble: Option<&str>,
    history: &[Message],
    tools: Vec<rig::completion::ToolDefinition>,
) -> rig::completion::CompletionRequest {
    rig::completion::CompletionRequest {
        model: None,
        preamble: preamble.map(str::to_string),
        chat_history: rig::OneOrMany::many(gents_inference::llm::rig_compat::to_rig_messages(
            history,
        ))
        .expect("at least one history row"),
        documents: Vec::new(),
        tools,
        temperature: None,
        max_tokens: Some(128),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    }
}

/// The body of an assistant-only history, as the replay cases drive it.
fn native_messages_body(history: &[Message]) -> Result<serde_json::Value, MessagesBodyError> {
    if history.is_empty() {
        return Ok(serde_json::json!({"messages": []}));
    }
    gents_inference::clients::claude_messages::build_messages_body(
        "claude-sonnet-5",
        &claude_request(None, history, Vec::new()),
    )
}

fn replay_checkpoint_outcome(
    error: &gents_loop::claude_messages_body::ReplayCheckpointError,
) -> &'static str {
    use gents_loop::claude_messages_body::ReplayCheckpointError as Error;
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
fn generated_protected_replay_compaction_cases_bind_native_split_and_checkpoint() {
    use gents_lean_vocab::lean_protected_replay_compaction_cases;
    use gents_loop::claude_messages_body::prepare_replay_checkpoint;
    use gents_loop::compaction::history::protected_pair_safe_split_index;

    let cases = lean_protected_replay_compaction_cases();
    assert_eq!(cases.len(), 6);
    for case in cases {
        let roles = super::streaming_compaction::compaction_messages_for_count(case.message_count);
        let assistant_count = roles
            .iter()
            .filter(|message| matches!(message, Message::Assistant { .. }))
            .count();
        assert_eq!(case.rows.len(), assistant_count, "{}", case.name);

        let provider_view = native_modeled_provider_view(&roles, &case.rows);
        let messages = provider_view
            .iter()
            .map(|row| row.message.clone())
            .collect::<Vec<_>>();
        let required = case
            .required
            .iter()
            .map(native_replay_tag)
            .collect::<Vec<_>>();

        let ceiling = match gents_loop::loop_stream::replay_compaction_prefix_bound(
            &provider_view,
            &required,
        ) {
            Err(error) => {
                assert_eq!(
                    replay_checkpoint_outcome(&error),
                    case.ceiling_error,
                    "{}: refused ceiling",
                    case.name
                );
                assert_eq!(case.outcome, "ceiling_refused", "{}", case.name);
                continue;
            }
            Ok(ceiling) => ceiling,
        };
        assert!(
            case.ceiling_error.is_empty(),
            "{}: the model refused this ceiling",
            case.name
        );
        assert_eq!(ceiling, case.ceiling, "{}: derived ceiling", case.name);
        let Some(ceiling) = ceiling else {
            assert_eq!(case.outcome, "unbounded", "{}", case.name);
            continue;
        };

        let split = protected_pair_safe_split_index(&messages, case.raw_index, ceiling);
        assert_eq!(
            split, case.selected_split,
            "{}: pair-safe protected split",
            case.name
        );
        let outcome = match split {
            None => "cannot_fit",
            Some(split) => {
                let assistant_split = messages[..split]
                    .iter()
                    .filter(|message| matches!(message, Message::Assistant { .. }))
                    .count();
                let rows = case.rows.iter().map(native_tagged_replay_row).collect();
                match prepare_replay_checkpoint(
                    case.required.iter().map(native_replay_tag).collect(),
                    rows,
                    assistant_split,
                ) {
                    Ok(_) => "ok",
                    Err(error) => replay_checkpoint_outcome(&error),
                }
            }
        };
        assert_eq!(outcome, case.outcome, "{}: composed checkpoint", case.name);
    }
}

#[test]
fn generated_claude_checkpoint_cases_bind_selected_assistant_projection() {
    use gents_lean_vocab::lean_prompt_assembly_claude_checkpoint_cases;
    use gents_loop::claude_messages_body::{
        prepare_replay_checkpoint, restore_contiguous_replay, TaggedAssistantRow,
    };
    let cases = lean_prompt_assembly_claude_checkpoint_cases();
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(case.carrier_ids.len(), case.rows.len(), "{}", case.name);
        let rows = case
            .rows
            .iter()
            .zip(&case.carrier_ids)
            .map(|(row, id)| TaggedAssistantRow {
                source: row.source.as_ref().map(native_replay_tag),
                physical_header: row.physical_header.clone(),
                block_indices: row.block_indices.clone(),
                id: Some(id.clone()),
                content: native_claude_replay_content(&row.blocks),
            })
            .collect();
        let checkpoint = match prepare_replay_checkpoint(
            case.required.iter().map(native_replay_tag).collect(),
            rows,
            case.split,
        ) {
            Err(error) => {
                assert_eq!(
                    replay_checkpoint_outcome(&error),
                    case.outcome,
                    "{}",
                    case.name
                );
                assert!(
                    case.prefix_rows.is_empty()
                        && case.retained.is_empty()
                        && case.replay.is_empty()
                );
                continue;
            }
            Ok(checkpoint) => checkpoint,
        };
        for (actual, expected, offset) in [
            (&checkpoint.prefix_rows, &case.prefix_rows, 0),
            (&checkpoint.retained, &case.retained, case.prefix_rows.len()),
        ] {
            assert_eq!(actual.len(), expected.len(), "{}", case.name);
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                assert_eq!(
                    actual.source,
                    expected.source.as_ref().map(native_replay_tag),
                    "{}",
                    case.name
                );
                assert_eq!(
                    actual.content,
                    native_claude_replay_content(&expected.blocks),
                    "{}",
                    case.name
                );
                assert_eq!(
                    actual.id.as_ref(),
                    Some(&case.carrier_ids[offset + index]),
                    "{}",
                    case.name
                );
            }
        }
        match restore_contiguous_replay(&checkpoint) {
            Err(error) => assert_eq!(
                replay_checkpoint_outcome(&error),
                case.outcome,
                "{}",
                case.name
            ),
            Ok(prepared) => {
                assert_eq!(case.outcome, "ok", "{}", case.name);
                assert_eq!(prepared.len(), case.replay.len(), "{}", case.name);
                for (row, expected) in prepared.iter().zip(&case.replay) {
                    assert_eq!(
                        row,
                        &expected
                            .iter()
                            .map(LeanClaudeReplayBlock::native_json)
                            .collect::<Vec<_>>(),
                        "{}",
                        case.name
                    );
                }
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
                let actual = body["messages"]
                    .as_array()
                    .expect("messages")
                    .iter()
                    .map(|message| {
                        let mut blocks = message["content"]
                            .as_array()
                            .expect("assistant content")
                            .clone();
                        for block in &mut blocks {
                            block
                                .as_object_mut()
                                .expect("content object")
                                .remove("cache_control");
                        }
                        blocks
                    })
                    .collect::<Vec<_>>();
                let expected = prepared
                    .into_iter()
                    .filter(|row| !row.is_empty())
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "{}", case.name);
            }
        }
    }
}

#[test]
fn generated_claude_replay_cases_drive_native_messages_body() {
    let cases = lean_prompt_assembly_claude_replay_cases();
    assert!(!cases.is_empty(), "Lean emitted no Claude replay cases");
    for case in cases {
        let content = native_claude_replay_content(&case.blocks);
        let history = [Message::Assistant {
            request_id: None,
            id: None,
            content,
        }];
        let result = native_messages_body(&history);
        if case.outcome != "ok" {
            let Err(MessagesBodyError::Codec(error)) = result else {
                panic!("modeled unsupported replay must fail closed: {}", case.name);
            };
            assert_eq!(replay_codec_outcome(&error), case.outcome, "{}", case.name);
            continue;
        }
        let body = result.unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let mut actual = if case.replay.is_empty() {
            assert!(
                body["messages"].as_array().is_none_or(Vec::is_empty),
                "{}: empty replay must omit assistant message: {body}",
                case.name
            );
            Vec::new()
        } else {
            body["messages"][0]["content"]
                .as_array()
                .unwrap_or_else(|| panic!("{}: missing assistant content: {body}", case.name))
                .clone()
        };
        for block in &mut actual {
            block
                .as_object_mut()
                .expect("content object")
                .remove("cache_control");
        }
        let expected = case
            .replay
            .iter()
            .map(LeanClaudeReplayBlock::native_json)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "replay ({})", case.name);
    }
}

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|path| path.join("crates/gents/proofs/lakefile.lean").exists())
        .expect("repository root should contain crates/gents/proofs/lakefile.lean")
        .to_path_buf()
}

fn production_sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    let mut stack = [
        "gents",
        "gents-loop",
        "gents-inference",
        "gents-session",
        "gents-tools",
        "gents-toolcalls",
        "gents-automation",
        "gents-observe",
    ]
    .into_iter()
    .map(|name| root.join("crates").join(name).join("src"))
    .collect::<Vec<_>>();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .expect("path under repo root")
                .to_string_lossy()
                .replace('\\', "/");
            let file_name = relative.rsplit('/').next().unwrap_or("");
            if relative.ends_with("/tests.rs")
                || relative.contains("/tests/")
                || file_name.ends_with("_tests.rs")
                || file_name.ends_with("_test.rs")
                || relative.contains("test_support")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            out.push((relative, source));
        }
    }
    out.sort();
    out
}

fn files_containing(needles: &[&str]) -> BTreeSet<String> {
    production_sources()
        .into_iter()
        .filter(|(_, source)| needles.iter().any(|needle| source.contains(needle)))
        .map(|(path, _)| path)
        .collect()
}

#[test]
fn provider_invocations_are_confined_to_the_owned_loop_seam() {
    let expected = BTreeSet::from([
        "crates/gents-loop/src/loop_stream.rs".to_string(),
        "crates/gents-inference/src/llm/rig_compat.rs".to_string(),
    ]);
    let found = files_containing(&[".completion_request(", ".completion(", ".stream("]);
    assert_eq!(
        found, expected,
        "provider invocation escaped the owned-loop seam; every completion request \
         must be born in run_loop_stream, which sanitizes loaded history at entry"
    );
}

#[test]
fn owned_loop_entry_points_are_registered() {
    let expected = BTreeSet::from([
        "crates/gents-loop/src/loop_stream/one_shot.rs".to_string(),
        "crates/gents/src/runtime/agent/daemon/inference.rs".to_string(),
        "crates/gents/src/runtime/agent/daemon/title.rs".to_string(),
        "crates/gents/src/runtime/agent/daemon/title/review.rs".to_string(),
        "crates/gents-loop/src/compaction_rolling.rs".to_string(),
        "crates/gents/src/runtime/oneshot.rs".to_string(),
    ]);
    let found = files_containing(&[
        "run_loop_stream(",
        "run_loop_stream::<",
        "run_loop_to_text(",
        "run_loop_to_text::<",
        "run_loop_to_typed(",
        "run_loop_to_typed::<",
    ]);
    assert_eq!(
        found, expected,
        "the set of owned-loop entry points changed; a new completion path must be \
         reviewed against the sanitize-at-entry chokepoint"
    );
}

#[test]
fn empty_skill_catalog_contributes_no_reminders() {
    use gents::turn::prompt::LayeredPromptBuilder;

    let builder =
        LayeredPromptBuilder::for_agent("literal preamble", "fence", &["bash"], false, &[]);

    assert!(
        builder.selected_skill_reminders(&[]).is_empty(),
        "an empty whitelist must not produce skill reminders"
    );

    assert!(
        builder
            .selected_skill_reminders(&["not-installed".to_string()])
            .is_empty(),
        "an unresolvable selection must not produce skill reminders"
    );
    assert!(
        builder
            .selected_skill_reminders(&["  ".to_string()])
            .is_empty(),
        "a blank selection entry must not produce skill reminders"
    );
}
