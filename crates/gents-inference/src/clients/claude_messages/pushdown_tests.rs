use rapidhash::{HashSetExt, RapidHashSet};
use rig::completion::CompletionError;
use rig::streaming::RawStreamingChoice;
use crate::clients::claude_subscription::ClaudeStreamResponse;
use crate::clients::claude_messages::parse_messages_sse;
use crate::clients::claude_messages::parse_messages_sse_typed;
use gents_claude::body::ModelFacts;
use gents_claude::body::Effort;
use gents_claude::attribution;
use gents_claude::catalog;
use gents_claude::profile;
use crate::llm::message::Message;
use crate::llm::message::AssistantContent;
use crate::llm::message::Reasoning;
use gents_loop::provider_input::ProviderInputProfile;
use gents_loop::claude_messages_body::ClaudeAccountContext;
use gents_loop::claude_messages_body::ClaudeRequestContext;
use gents_loop::claude_messages_body::ClaudeRequestIds;
use gents_loop::claude_messages_body::plan_with_ids;
use gents_lean_vocab::LeanClaudeReasoningPart;
use gents_lean_vocab::LeanClaudeStreamBlock;
use gents_lean_vocab::LeanClaudeStreamEvent;
use gents_lean_vocab::lean_prompt_assembly_claude_body_cases;
use gents_lean_vocab::lean_prompt_assembly_claude_map_cases;
use gents_lean_vocab::lean_prompt_assembly_claude_stream_cases;
use gents_lean_vocab::lean_prompt_assembly_claude_thinking_stream_cases;

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

fn lean_facts(facts: &gents_lean_vocab::LeanPromptAssemblyClaudeFieldFacts) -> ModelFacts {
    ModelFacts {
        adaptive: facts.adaptive,
        always_thinks: facts.always_thinks,
        thinking_supported: facts.thinking_supported,
        rejects_disabled: facts.rejects_disabled,
        accepts_temperature: facts.accepts_temperature,
        effort_capable: facts.effort_capable,
        max_effort: facts.max_effort,
        xhigh_effort: facts.xhigh_effort,
        caps_effort_when_disabled: facts.caps_effort_when_disabled,
        per_turn_effort: facts.per_turn_effort,
        output_default: facts.output_default,
        output_upper: facts.output_upper,
        default_effort: facts
            .default_effort
            .as_deref()
            .map(|name| Effort::parse(name).expect("a Lean effort name")),
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
        chat_history: rig::OneOrMany::many(crate::llm::rig_compat::to_rig_messages(
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

#[test]
fn generated_claude_thinking_stream_cases_drive_native_sse_parser() {
    use crate::llm::message::ReasoningContent;

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
            use crate::clients::claude_messages::{
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
            crate::clients::claude_messages::MessagesSseState::new(surface.clone());
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
                    let part = crate::llm::rig_compat::from_rig_reasoning_part(content);
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
        let mut state = crate::clients::claude_messages::MessagesSseState::new(surface);
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
    use crate::llm::message::ReasoningContent;
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
                    let part = crate::llm::rig_compat::from_rig_reasoning_part(&content);
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

#[test]
fn generated_claude_body_cases_drive_the_body_builder() {
    let cases = lean_prompt_assembly_claude_body_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly Claude body cases"
    );
    let account = ClaudeAccountContext::projection();
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
                    Message::user(case.first_user_text.clone())
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
        let mut request = claude_request(case.preamble.as_deref(), &history, tools);
        request.additional_params = Some(serde_json::json!({
            (crate::clients::claude_messages::ADVERTISED_REASONING_EFFORTS_PARAM): case.supported_efforts,
            "output_config": {"effort": case.effort}
        }));
        let context = ClaudeRequestContext::client_turn("conformance-session");
        let ids = ClaudeRequestIds {
            session: context.ids().session,
            prompt: case.prompt_id.clone(),
        };
        let plan = plan_with_ids(&case.model, &request, &account, &context, &ids)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));

        let fingerprint = attribution::fingerprint(&case.first_user_text, profile::VERSION);
        let expected = |text: &String| text.replace(&case.fingerprint_placeholder, &fingerprint);
        assert_eq!(
            expected(&case.system[0]),
            expected(&case.attribution),
            "{}",
            case.name
        );
        let system: Vec<String> = plan
            .body
            .system
            .iter()
            .map(|block| block.text.clone())
            .collect();
        assert_eq!(
            system,
            case.system.iter().map(expected).collect::<Vec<_>>(),
            "system blocks ({})",
            case.name
        );
        assert_eq!(
            plan.body
                .system
                .iter()
                .map(|block| block.cache_control.is_some())
                .collect::<Vec<_>>(),
            case.system_marked,
            "system markers ({})",
            case.name
        );
        assert_eq!(
            plan.body.tools.as_ref().map(Vec::len),
            Some(case.tools.len()),
            "tools key ({})",
            case.name
        );
        assert!(case.tools_present, "{}", case.name);

        let catalog_facts = ModelFacts::of(catalog::find(&case.model).expect("a catalog model"));
        assert_eq!(
            catalog_facts,
            lean_facts(&case.facts),
            "facts ({})",
            case.name
        );
        let body = serde_json::to_value(&plan.body).expect("body JSON");
        assert_eq!(body["max_tokens"], case.max_tokens, "{}", case.name);
        assert_eq!(
            body["temperature"].as_u64(),
            case.temperature,
            "{}",
            case.name
        );
        assert_eq!(
            body.get("context_management").is_some(),
            case.context_management,
            "{}",
            case.name
        );
        assert_eq!(
            body["output_config"]["effort"].as_str(),
            case.body_effort.as_deref(),
            "{}",
            case.name
        );
        assert_eq!(
            body.get("diagnostics").is_some(),
            case.diagnostics.present,
            "{}",
            case.name
        );
        let thinking = &body["thinking"];
        match case.thinking.kind.as_str() {
            "none" => assert!(thinking.is_null(), "{}", case.name),
            "disabled" => assert_eq!(thinking["type"], "disabled", "{}", case.name),
            "adaptive" => {
                assert_eq!(thinking["type"], "adaptive", "{}", case.name);
                assert_eq!(
                    thinking["display"].as_str() == Some("updates"),
                    case.thinking.updates,
                    "{}",
                    case.name
                );
            }
            "enabled" => assert_eq!(
                thinking["budget_tokens"].as_u64(),
                case.thinking.budget,
                "{}",
                case.name
            ),
            other => panic!("unknown thinking kind {other}"),
        }
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
