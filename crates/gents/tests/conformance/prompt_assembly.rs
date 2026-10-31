use gents::inference::compaction::sanitize_history_for_provider;
use gents::inference::llm::message::{
    AssistantContent, Message, Reasoning, Text, ToolCall, ToolFunction, ToolResult,
    ToolResultContent, UserContent,
};
use gents_loop::provider_input::ProviderInputProfile;

use rapidhash::{HashSetExt, RapidHashSet};

use gents::inference::clients::claude_messages::{
    build_messages_body_native, parse_messages_sse, parse_messages_sse_typed, CLAUDE_CODE_IDENTITY,
};
use gents::inference::clients::claude_subscription::ClaudeStreamResponse;
use rig::completion::CompletionError;
use rig::streaming::RawStreamingChoice;

use crate::lean_vocab_test::{
    lean_prompt_assembly_claude_body_cases, lean_prompt_assembly_claude_map_cases,
    lean_prompt_assembly_claude_replay_cases, lean_prompt_assembly_claude_stream_cases,
    lean_prompt_assembly_claude_thinking_stream_cases, lean_prompt_assembly_sanitize_cases,
    native_claude_reasoning_parts, native_claude_replay_content, LeanClaudeReasoningPart,
    LeanClaudeReplayBlock, LeanClaudeStreamBlock, LeanClaudeStreamEvent, LeanPromptAssemblyItem,
    LeanPromptAssemblyRow,
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
    use gents::inference::llm::message::ReasoningContent;
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

#[test]
fn generated_claude_map_cases_drive_the_messages_parser() {
    let cases = lean_prompt_assembly_claude_map_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly Claude map cases"
    );
    for case in cases {
        let surface: RapidHashSet<String> = case.surface.iter().cloned().collect();
        let sse = claude_map_blocks_as_sse(&case.blocks);
        let parsed = parse_messages_sse(&sse, &surface);
        match case.outcome.as_str() {
            "ok" => {
                let events =
                    parsed.unwrap_or_else(|err| panic!("case {} should map: {err}", case.name));
                let got_ids: Vec<u64> = events
                    .iter()
                    .filter_map(|event| match event {
                        RawStreamingChoice::ToolCall(call) => {
                            Some(call.id.parse::<u64>().unwrap_or_else(|_| {
                                panic!("case {} mapped id {} is not a Nat", case.name, call.id)
                            }))
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(got_ids, case.ids, "mapped ids ({})", case.name);
            }
            outcome => assert_fail_closed(&case.name, outcome, parsed),
        }
    }
}

fn assert_fail_closed(
    case_name: &str,
    outcome: &str,
    parsed: Result<Vec<RawStreamingChoice<ClaudeStreamResponse>>, CompletionError>,
) {
    let err = parsed
        .expect_err("witness outcome is an error; parser must fail closed")
        .to_string();
    let ok = match outcome {
        "emptySurface" => err.contains("fail-closed: tool_use observed"),
        o if o.starts_with("unmappedName:") => {
            let name = o.strip_prefix("unmappedName:").expect("prefix");
            err.contains("fail-closed: tool_use observed") && err.contains(name)
        }
        o if o.starts_with("duplicateId:") => err.contains(&format!(
            "fail-closed: duplicate tool_use id {}",
            o.strip_prefix("duplicateId:").expect("prefix")
        )),
        o if o.starts_with("overlappingBlock:") => err.contains(&format!(
            "fail-closed: overlapping tool_use block {}",
            o.strip_prefix("overlappingBlock:").expect("prefix")
        )),
        other => panic!("case {case_name} unknown outcome {other}"),
    };
    assert!(
        ok,
        "case {case_name}: outcome {outcome} but error was: {err}"
    );
}

fn sse_event(payload: serde_json::Value) -> String {
    let kind = payload["type"].as_str().expect("type").to_string();
    format!("event: {kind}\ndata: {payload}\n\n")
}

fn claude_map_blocks_as_sse(blocks: &[String]) -> String {
    let mut sse = String::new();
    for (index, block) in blocks.iter().enumerate() {
        if block == "text" {
            sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_start", "index": index,
                "content_block": {"type": "text", "text": ""}
            })));
            sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_delta", "index": index,
                "delta": {"type": "text_delta", "text": "hi"}
            })));
            sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_stop", "index": index
            })));
        } else if let Some(rest) = block.strip_prefix("toolUse:") {
            let (id, name) = rest
                .split_once(':')
                .unwrap_or_else(|| panic!("toolUse tag must be toolUse:id:name, got {block}"));
            sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_start", "index": index,
                "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
            })));
            sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_stop", "index": index
            })));
        } else if block.starts_with("toolResult:") {
            continue;
        } else {
            panic!("unknown Claude map block tag {block}");
        }
    }
    sse.push_str(&sse_event(serde_json::json!({"type": "message_stop"})));
    sse
}

#[test]
fn generated_claude_stream_cases_drive_the_messages_parser() {
    let cases = lean_prompt_assembly_claude_stream_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly Claude stream cases"
    );
    for case in cases {
        let surface: RapidHashSet<String> = case.surface.iter().cloned().collect();
        let sse = claude_stream_events_as_sse(&case.events);
        let parsed = parse_messages_sse(&sse, &surface);
        match case.outcome.as_str() {
            "ok" => {
                let events =
                    parsed.unwrap_or_else(|err| panic!("case {} should parse: {err}", case.name));
                let got: Vec<String> = events
                    .iter()
                    .filter_map(|event| match event {
                        RawStreamingChoice::ToolCall(call) => {
                            Some(format!("{}={}", call.id, call.arguments))
                        }
                        _ => None,
                    })
                    .collect();
                let want: Vec<String> = case
                    .calls
                    .iter()
                    .map(|entry| {
                        let (id, args) = entry.split_once('=').expect("id=args");
                        let value: serde_json::Value =
                            serde_json::from_str(args).unwrap_or_else(|err| {
                                panic!("case {} witness args {args}: {err}", case.name)
                            });
                        format!("{id}={value}")
                    })
                    .collect();
                assert_eq!(got, want, "calls ({})", case.name);
            }
            outcome => assert_fail_closed(&case.name, outcome, parsed),
        }
    }
}

fn claude_stream_events_as_sse(events: &[String]) -> String {
    let mut sse = String::new();
    for tag in events {
        if let Some(text) = tag.strip_prefix("text:") {
            sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": text}
            })));
        } else if let Some(rest) = tag.strip_prefix("start:") {
            let mut parts = rest.splitn(3, ':');
            let id = parts.next().expect("id");
            let name = parts.next().expect("name");
            let input = parts.next().unwrap_or("");
            let mut block = serde_json::json!({"type": "tool_use", "id": id, "name": name});
            if !input.is_empty() {
                block["input"] = serde_json::from_str(input)
                    .unwrap_or_else(|err| panic!("witness start input {input}: {err}"));
            }
            sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_start", "index": 0, "content_block": block
            })));
        } else if let Some(partial) = tag.strip_prefix("delta:") {
            sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": partial}
            })));
        } else if tag == "stop" {
            sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_stop", "index": 0
            })));
        } else {
            panic!("unknown Claude stream event tag {tag}");
        }
    }
    sse
}

fn claude_thinking_sse_events(events: &[LeanClaudeStreamEvent]) -> Vec<String> {
    let mut sse_events = Vec::with_capacity(events.len());
    let mut tool_index = 0;
    for event in events {
        let index = event.index.unwrap_or(tool_index);
        let payload = match event.kind.as_str() {
            "text" => serde_json::json!({
                "type": "content_block_delta", "index": index,
                "delta": {"type": "text_delta", "text": event.value.as_deref().expect("text")}
            }),
            "toolStart" => {
                tool_index = event.id.expect("tool id");
                let mut block = serde_json::json!({
                    "type": "tool_use", "id": tool_index.to_string(),
                    "name": event.name.as_deref().expect("tool name")
                });
                if let Some(input) = &event.input {
                    block["input"] = serde_json::from_str(input).expect("modeled tool input JSON");
                }
                serde_json::json!({"type":"content_block_start", "index":tool_index,
                    "content_block":block})
            }
            "toolDelta" => serde_json::json!({
                "type":"content_block_delta", "index":tool_index,
                "delta":{"type":"input_json_delta",
                    "partial_json":event.fragment.as_deref().expect("tool fragment")}
            }),
            "toolStop" => serde_json::json!({"type":"content_block_stop", "index":tool_index}),
            "thinkingStart" => serde_json::json!({
                "type":"content_block_start", "index":index,
                "content_block":{"type":"thinking",
                    "thinking":event.value.as_deref().expect("initial thinking text")}
            }),
            "thinkingDelta" => serde_json::json!({
                "type":"content_block_delta", "index":index,
                "delta":{"type":"thinking_delta",
                    "thinking":event.fragment.as_deref().expect("thinking fragment")}
            }),
            "signatureDelta" => serde_json::json!({
                "type":"content_block_delta", "index":index,
                "delta":{"type":"signature_delta",
                    "signature":event.fragment.as_deref().expect("signature fragment")}
            }),
            "redactedStart" => serde_json::json!({
                "type":"content_block_start", "index":index,
                "content_block":{"type":"redacted_thinking",
                    "data":event.data.as_deref().expect("redacted data")}
            }),
            "contentStop" => serde_json::json!({"type":"content_block_stop", "index":index}),
            other => panic!("unknown modeled Claude event {other}"),
        };
        sse_events.push(sse_event(payload));
    }
    sse_events
}

fn claude_thinking_events_as_sse(events: &[LeanClaudeStreamEvent]) -> String {
    claude_thinking_sse_events(events).concat()
}

#[test]
fn generated_claude_thinking_stream_cases_drive_native_sse_parser() {
    use gents::inference::llm::message::ReasoningContent;

    let cases = lean_prompt_assembly_claude_thinking_stream_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no Claude thinking stream cases"
    );
    for case in cases {
        let surface = case.surface.iter().cloned().collect::<RapidHashSet<_>>();
        let parsed =
            parse_messages_sse_typed(&claude_thinking_events_as_sse(&case.events), &surface);
        if case.outcome != "ok" {
            use gents::inference::clients::claude_messages::{
                MessagesParseError, ThinkingParseCause,
            };
            let error = parsed.expect_err("modeled malformed block must fail closed");
            let MessagesParseError::MalformedThinking { cause } = error else {
                panic!("{}: expected malformed thinking: {error}", case.name);
            };
            let expected = match case.outcome.as_str() {
                "missingSignature" => ThinkingParseCause::MissingSignature,
                "incompleteBlock" => ThinkingParseCause::IncompleteBlock,
                "signatureOrder" => ThinkingParseCause::SignatureOrder,
                "wrongBlock" => ThinkingParseCause::WrongBlock,
                outcome if outcome.starts_with("wrongIndex:") => ThinkingParseCause::WrongIndex(
                    outcome["wrongIndex:".len()..]
                        .parse()
                        .expect("modeled wrong index"),
                ),
                other => panic!("unknown modeled Claude thinking error {other}"),
            };
            assert_eq!(cause, expected, "{}", case.name);
            continue;
        }
        let parsed = parsed.unwrap_or_else(|error| panic!("{}: {error}", case.name));
        assert_eq!(case.steps.len(), case.events.len(), "{}", case.name);
        let mut state =
            gents::inference::clients::claude_messages::MessagesSseState::new(surface.clone());
        let mut live_preview = String::new();
        for (event, (sse, step)) in case.events.iter().zip(
            claude_thinking_sse_events(&case.events)
                .into_iter()
                .zip(case.steps.iter()),
        ) {
            if event.kind == "thinkingStart" {
                live_preview.clear();
            }
            for line in sse.lines() {
                for emitted in state
                    .push_line(line)
                    .unwrap_or_else(|error| panic!("{}: event {}: {error}", case.name, event.kind))
                {
                    if let RawStreamingChoice::ReasoningDelta { reasoning, .. } = emitted {
                        live_preview.push_str(&reasoning);
                    }
                }
            }
            match &step.provisional_thinking {
                Some(expected) => assert_eq!(
                    &live_preview, expected,
                    "{}: event {} preview",
                    case.name, event.kind
                ),
                None if event.kind == "contentStop" => live_preview.clear(),
                None => {}
            }
        }
        let observed: Vec<LeanClaudeStreamBlock> = parsed
            .iter()
            .filter_map(|event| match event {
                RawStreamingChoice::Message(value) => Some(LeanClaudeStreamBlock {
                    kind: "text".into(),
                    value: Some(value.clone()),
                    parts: None,
                    id: None,
                    name: None,
                    arguments: None,
                }),
                RawStreamingChoice::Reasoning { content, .. } => {
                    let part = gents::inference::llm::rig_compat::from_rig_reasoning_part(content);
                    let part = match part {
                        ReasoningContent::Text { text, signature } => LeanClaudeReasoningPart {
                            kind: "text".into(),
                            payload: text,
                            signature,
                        },
                        ReasoningContent::Redacted { data } => LeanClaudeReasoningPart {
                            kind: "redacted".into(),
                            payload: data,
                            signature: None,
                        },
                        other => panic!("unsupported native Claude reasoning output: {other:?}"),
                    };
                    Some(LeanClaudeStreamBlock {
                        kind: "reasoning".into(),
                        value: None,
                        parts: Some(vec![part]),
                        id: None,
                        name: None,
                        arguments: None,
                    })
                }
                RawStreamingChoice::ToolCall(call) => Some(LeanClaudeStreamBlock {
                    kind: "toolUse".into(),
                    value: None,
                    parts: None,
                    id: Some(call.id.parse().expect("modeled Nat tool id")),
                    name: Some(call.name.clone()),
                    arguments: Some(call.arguments.to_string()),
                }),
                _ => None,
            })
            .collect();
        assert_eq!(observed, case.content, "sealed content ({})", case.name);
    }
}

#[test]
fn generated_claude_decoded_abort_prefixes_emit_native_audit_events() {
    use gents_loop::provider_audit::ClaudeAuditEvent;

    let mut saw_signature = false;
    let mut saw_redacted = false;
    for case in lean_prompt_assembly_claude_thinking_stream_cases()
        .iter()
        .filter(|case| {
            case.outcome == "incompleteBlock"
                && case.steps.last().is_some_and(|step| {
                    step.provisional_signature.is_some() || step.provisional_redacted.is_some()
                })
        })
    {
        assert_eq!(case.steps.len(), case.events.len(), "{}", case.name);
        let surface = case.surface.iter().cloned().collect::<RapidHashSet<_>>();
        let mut state = gents::inference::clients::claude_messages::MessagesSseState::new(surface);
        let mut signature = None::<String>;
        let mut redacted = None::<String>;
        for ((event, sse), step) in case
            .events
            .iter()
            .zip(claude_thinking_sse_events(&case.events))
            .zip(&case.steps)
        {
            for line in sse.lines() {
                state
                    .push_line_typed(line)
                    .unwrap_or_else(|error| panic!("{}: event {}: {error}", case.name, event.kind));
            }
            for audit in state.take_audit_events() {
                match audit {
                    ClaudeAuditEvent::BlockStart { .. } => {
                        signature = None;
                        redacted = None;
                    }
                    ClaudeAuditEvent::Signature { fragment, .. } => {
                        signature
                            .get_or_insert_with(String::new)
                            .push_str(&fragment);
                    }
                    ClaudeAuditEvent::RedactedData { data, .. } => {
                        redacted.get_or_insert_with(String::new).push_str(&data);
                    }
                    ClaudeAuditEvent::BlockStop { .. } => {
                        panic!("{}: decoded abort prefix unexpectedly sealed", case.name);
                    }
                    ClaudeAuditEvent::ThinkingText { .. } => {}
                }
            }
            assert_eq!(
                signature, step.provisional_signature,
                "{}: event {} signature audit prefix",
                case.name, event.kind
            );
            assert_eq!(
                redacted, step.provisional_redacted,
                "{}: event {} redacted audit prefix",
                case.name, event.kind
            );
            assert!(
                step.sealed.is_empty(),
                "{}: decoded abort prefix sealed",
                case.name
            );
        }
        saw_signature |= signature.is_some();
        saw_redacted |= redacted.is_some();
    }
    assert!(
        saw_signature && saw_redacted,
        "Lean emitted no decoded signature or redacted abort prefix"
    );
}

#[test]
fn generated_claude_initial_thinking_text_seals_in_native_accumulator() {
    use gents::inference::llm::message::ReasoningContent;
    use gents_loop::provider_input::ProviderInputProfile;
    use gents_loop::stream_processor::AssistantTurnAccumulator;

    let cases: Vec<_> = lean_prompt_assembly_claude_thinking_stream_cases()
        .iter()
        .filter(|case| {
            case.outcome == "ok"
                && !case.content.is_empty()
                && case.content.iter().all(|block| block.kind == "reasoning")
                && case.events.iter().any(|event| {
                    event.kind == "thinkingStart"
                        && event.value.as_deref().is_some_and(|text| !text.is_empty())
                })
        })
        .collect();
    assert!(
        !cases.is_empty(),
        "Lean emitted no initial-text thinking case"
    );
    for case in cases {
        let parsed = parse_messages_sse(
            &claude_thinking_events_as_sse(&case.events),
            &RapidHashSet::new(),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let mut accumulator = AssistantTurnAccumulator::default();
        for event in parsed {
            match event {
                RawStreamingChoice::ReasoningDelta { id, reasoning } => {
                    accumulator.push_reasoning_delta(id, &reasoning)
                }
                RawStreamingChoice::Reasoning { id, content } => {
                    let part = gents::inference::llm::rig_compat::from_rig_reasoning_part(&content);
                    match &part {
                        ReasoningContent::Text { .. } | ReasoningContent::Redacted { .. } => {}
                        other => panic!("unexpected Claude reasoning: {other:?}"),
                    }
                    accumulator
                        .push_provider_reasoning(
                            ProviderInputProfile::ClaudeMessages,
                            Reasoning {
                                id,
                                content: vec![part],
                            },
                        )
                        .unwrap_or_else(|error| panic!("{}: {error}", case.name));
                }
                RawStreamingChoice::FinalResponse(_) => {}
                other => panic!("initial-text case emitted unrelated event: {other:?}"),
            }
        }
        let expected: Vec<_> = case
            .content
            .iter()
            .map(|block| {
                let parts = block.parts.as_ref().expect("modeled reasoning parts");
                AssistantContent::Reasoning(Reasoning {
                    id: None,
                    content: parts
                        .iter()
                        .map(|part| match part.kind.as_str() {
                            "text" => ReasoningContent::Text {
                                text: part.payload.clone(),
                                signature: part.signature.clone(),
                            },
                            "redacted" => ReasoningContent::Redacted {
                                data: part.payload.clone(),
                            },
                            other => panic!("unexpected modeled reasoning part: {other}"),
                        })
                        .collect(),
                })
            })
            .collect();
        let Some(Message::Assistant { content, .. }) = accumulator.take_message() else {
            panic!("{}: no canonical assistant message", case.name);
        };
        assert_eq!(content, expected, "{}: canonical seal", case.name);
    }
}

fn profile_of_order(mode: crate::lean_vocab_test::LeanAssistantOrderMode) -> ProviderInputProfile {
    match mode {
        crate::lean_vocab_test::LeanAssistantOrderMode::Grouped => {
            ProviderInputProfile::OpenAiChatCompletions
        }
        crate::lean_vocab_test::LeanAssistantOrderMode::NativePreserved => {
            ProviderInputProfile::ClaudeMessages
        }
    }
}

#[test]
fn generated_assistant_order_cases_bind_selected_provider_order() {
    use gents::inference::compaction::history::normalize_assistant_content_order;
    let cases = crate::lean_vocab_test::lean_prompt_assembly_assistant_order_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let profile = profile_of_order(case.order_mode);
        let input = vec![Message::Assistant {
            id: None,
            content: case.input.iter().map(assistant_item).collect(),
        }];
        let once = normalize_assistant_content_order(profile, input);
        assert_eq!(once.len(), 1, "{}", case.name);
        assert_eq!(
            row_of_message(&once[0]).content,
            case.expected,
            "{}",
            case.name
        );
        let twice = normalize_assistant_content_order(profile, once);
        assert_eq!(twice.len(), 1, "{}", case.name);
        assert_eq!(
            row_of_message(&twice[0]).content,
            case.expected_twice,
            "{}",
            case.name
        );
    }
}

#[test]
fn generated_mode_sanitize_cases_bind_composed_provider_view() {
    let cases = crate::lean_vocab_test::lean_prompt_assembly_mode_sanitize_cases();
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
    tag: &crate::lean_vocab_test::LeanCanonicalCoordinate,
) -> gents_loop::claude_messages_body::ReplayTag {
    use crate::lean_vocab_test::LeanCanonicalSource;
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
                    crate::lean_vocab_test::LeanAuxiliaryKind::Compaction => {
                        gents_protocol::rendered_request::CaptureScopeKind::Compaction
                    }
                    crate::lean_vocab_test::LeanAuxiliaryKind::CompactionFallback => {
                        gents_protocol::rendered_request::CaptureScopeKind::CompactionFallback
                    }
                    crate::lean_vocab_test::LeanAuxiliaryKind::Title => {
                        gents_protocol::rendered_request::CaptureScopeKind::Title
                    }
                    crate::lean_vocab_test::LeanAuxiliaryKind::AccessReview => {
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
    origin: &crate::lean_vocab_test::LeanClaudeReplayOrigin,
) -> gents_loop::claude_messages_body::ReplayOrigin {
    use crate::lean_vocab_test::LeanClaudeReplayOrigin as Lean;
    use gents_loop::claude_messages_body::ReplayOrigin as Native;
    match origin {
        Lean::ClaudeSubscription => Native::ClaudeSubscription,
        Lean::AcceptedProvider => Native::AcceptedProvider,
        Lean::Foreign => Native::Foreign,
        Lean::Missing => Native::Missing,
        Lean::Ambiguous => Native::Ambiguous,
    }
}

fn native_replay_wire(
    wire: crate::lean_vocab_test::LeanReasoningReplayWire,
) -> gents_loop::claude_messages_body::ReplayWire {
    use crate::lean_vocab_test::LeanReasoningReplayWire as Lean;
    use gents_loop::claude_messages_body::ReplayWire as Native;
    match wire {
        Lean::ClaudeMessages => Native::ClaudeMessages,
        Lean::Responses => Native::Responses,
    }
}

fn native_replay_resolution(
    entry: &crate::lean_vocab_test::LeanClaudeCheckpointResolution,
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
    items: &[crate::lean_vocab_test::LeanReplayFlatItem],
) -> Vec<gents_loop::provider_input::replay_frontier::FlatItem> {
    use crate::lean_vocab_test::LeanReplayFlatKind;
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
    row: &crate::lean_vocab_test::LeanClaudeTaggedReplayRow,
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
    row: &crate::lean_vocab_test::LeanClaudeTaggedReplayRow,
) -> gents_loop::loop_stream::TaggedMessage {
    gents_loop::loop_stream::TaggedMessage {
        message: Message::Assistant {
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
    rows: &[crate::lean_vocab_test::LeanClaudeTaggedReplayRow],
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
    use crate::lean_vocab_test::lean_prompt_assembly_replay_shape_cases;
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
    use crate::lean_vocab_test::lean_prompt_assembly_replay_prefix_cases;
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
    use crate::lean_vocab_test::lean_prompt_assembly_reasoning_suffix_cases;
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
    }
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
    use crate::lean_vocab_test::lean_protected_replay_compaction_cases;
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
    use crate::lean_vocab_test::lean_prompt_assembly_claude_checkpoint_cases;
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
                        id: row.id.clone(),
                        content: row.content.clone(),
                    })
                    .collect::<Vec<_>>();
                let body =
                    build_messages_body_native("claude-sonnet-5", None, Some(128), &native, &[])
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
        let history = [Message::Assistant { id: None, content }];
        let result = build_messages_body_native("claude-sonnet-5", None, Some(128), &history, &[]);
        if case.outcome != "ok" {
            let error = result.expect_err("modeled unsupported replay must fail closed");
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

#[test]
fn generated_claude_body_cases_drive_the_body_builder() {
    let cases = lean_prompt_assembly_claude_body_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly Claude body cases"
    );
    for case in cases {
        let history: Vec<Message> = case
            .rows
            .iter()
            .map(|row| {
                if let Some(text) = row.strip_prefix("system:") {
                    Message::System {
                        content: text.to_string(),
                    }
                } else if row == "other:assistant" {
                    Message::assistant("ok")
                } else {
                    Message::user("hi")
                }
            })
            .collect();
        let tools: Vec<rig::completion::ToolDefinition> = case
            .tools
            .iter()
            .map(|name| rig::completion::ToolDefinition {
                name: name.clone(),
                description: name.clone(),
                parameters: serde_json::json!({"type": "object"}),
            })
            .collect();
        let mut body = build_messages_body_native(
            "claude-sonnet-5",
            case.preamble.as_deref(),
            None,
            &history,
            &tools,
        )
        .expect("Claude body");
        gents::inference::clients::claude_messages::apply_reasoning_parameters(
            "claude-sonnet-5",
            &serde_json::json!({
                (gents::inference::clients::claude_messages::ADVERTISED_REASONING_EFFORTS_PARAM):
                    case.supported_efforts,
                "output_config": {"effort": case.effort}
            }),
            &mut body,
        );
        assert_eq!(
            body["output_config"]["effort"].as_str(),
            case.selected_effort.as_deref(),
            "{}",
            case.name
        );
        assert_eq!(
            body.get("thinking").is_some(),
            case.selected_effort.is_some(),
            "{}",
            case.name
        );
        let system: Vec<String> = body["system"]
            .as_array()
            .unwrap_or_else(|| panic!("case {} system array: {body}", case.name))
            .iter()
            .map(|block| block["text"].as_str().expect("text").to_string())
            .collect();
        assert_eq!(system, case.system, "system blocks ({})", case.name);
        assert_eq!(system[0], CLAUDE_CODE_IDENTITY, "case {}", case.name);
        assert_eq!(
            body.get("tools").is_some(),
            case.tools_present,
            "tools key ({})",
            case.name
        );
        let leaked = body["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .any(|message| {
                message["content"].as_array().is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block["text"]
                            .as_str()
                            .is_some_and(|t| t.starts_with("system: "))
                    })
                })
            });
        assert!(
            !leaked,
            "System row leaked into messages ({}): {body}",
            case.name
        );
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
    let mut stack = vec![
        root.join("crates/gents/src"),
        root.join("crates/gents-loop/src"),
    ];
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
                || relative.contains("lean_vocab_test")
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
        "crates/gents/src/inference/llm/rig_compat.rs".to_string(),
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
        "crates/gents/src/agent/daemon/inference.rs".to_string(),
        "crates/gents/src/agent/daemon/title.rs".to_string(),
        "crates/gents/src/agent/daemon/title/review.rs".to_string(),
        "crates/gents-loop/src/compaction_rolling.rs".to_string(),
        "crates/gents/src/oneshot.rs".to_string(),
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
