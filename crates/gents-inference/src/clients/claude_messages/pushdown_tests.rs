use crate::clients::claude_messages::parse_messages_sse;
use crate::clients::claude_messages::parse_messages_sse_typed;
use crate::clients::claude_messages::MessagesParseError;
use crate::llm::message::AssistantContent;
use crate::llm::message::Message;
use crate::llm::message::Reasoning;
use gents_claude::attribution;
use gents_claude::body::ModelFacts;
use gents_claude::catalog;
use gents_claude::profile;
use gents_loop::claude_messages_body::plan_with_ids;
use gents_loop::claude_messages_body::ClaudeAccountContext;
use gents_loop::claude_messages_body::ClaudeRequestContext;
use gents_loop::claude_messages_body::ClaudeRequestIds;
use rapidhash::RapidHashSet;
use rig::streaming::RawStreamingChoice;

fn fail_closed_outcome(error: MessagesParseError) -> String {
    match error {
        MessagesParseError::EmptyToolSurface { .. } => "emptySurface".to_owned(),
        MessagesParseError::ToolUse { names } => format!("unmappedName:{names}"),
        MessagesParseError::DuplicateToolUseId { id } => format!("duplicateId:{id}"),
        MessagesParseError::OverlappingToolUse { id } => format!("overlappingBlock:{id}"),
        other => panic!("not a modeled fail-closed tool_use outcome: {other}"),
    }
}

fn sse_event(payload: serde_json::Value) -> String {
    let kind = payload["type"].as_str().expect("type").to_string();
    format!("event: {kind}\ndata: {payload}\n\n")
}

fn claude_map_blocks_as_sse(
    blocks: &[gents_lean_vocab::families::contracts_claude_map::ClaudeBlockWire],
) -> String {
    let mut sse = String::new();
    for (index, block) in blocks.iter().enumerate() {
        match block.kind.as_str() {
            "text" => {
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
            }
            "toolUse" => {
                let id = block.id.expect("modeled tool id").to_string();
                let name = block.name.as_deref().expect("modeled tool name");
                sse.push_str(&sse_event(serde_json::json!({
                    "type": "content_block_start", "index": index,
                    "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
                })));
                sse.push_str(&sse_event(serde_json::json!({
                    "type": "content_block_stop", "index": index
                })));
            }
            "toolResult" => {}
            other => panic!("unknown Claude map block {other}"),
        }
    }
    sse.push_str(&sse_event(serde_json::json!({"type": "message_stop"})));
    sse
}

fn claude_stream_events_as_sse(
    events: &[gents_lean_vocab::families::contracts_claude_stream::ClaudeStreamEventWire],
) -> String {
    let mut sse = String::new();
    for event in events {
        match event.kind.as_str() {
            "text" => sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": event.value.as_deref().expect("text")}
            }))),
            "toolStart" => {
                let mut block = serde_json::json!({
                    "type": "tool_use",
                    "id": event.id.expect("modeled tool id").to_string(),
                    "name": event.name.as_deref().expect("modeled tool name")
                });
                if let Some(input) = &event.input {
                    block["input"] = serde_json::from_str(input).expect("modeled tool input JSON");
                }
                sse.push_str(&sse_event(serde_json::json!({
                    "type": "content_block_start", "index": 0, "content_block": block
                })));
            }
            "toolDelta" => sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta",
                    "partial_json": event.fragment.as_deref().expect("tool fragment")}
            }))),
            "toolStop" => sse.push_str(&sse_event(serde_json::json!({
                "type": "content_block_stop", "index": 0
            }))),
            other => panic!("unknown Claude stream event {other}"),
        }
    }
    sse
}

fn claude_thinking_sse_events(
    events: &[gents_lean_vocab::families::contracts_claude_thinking_stream::ClaudeStreamEventWire],
) -> Vec<String> {
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

fn claude_thinking_events_as_sse(
    events: &[gents_lean_vocab::families::contracts_claude_thinking_stream::ClaudeStreamEventWire],
) -> String {
    claude_thinking_sse_events(events).concat()
}

fn claude_request(
    preamble: Option<&str>,
    history: &[Message],
    tools: Vec<rig::completion::ToolDefinition>,
) -> rig::completion::CompletionRequest {
    rig::completion::CompletionRequest {
        model: None,
        preamble: preamble.map(str::to_string),
        chat_history: rig::OneOrMany::many(crate::llm::rig_compat::to_rig_messages(history))
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
fn claude_map_follows_the_lean_model() {
    gents_lean_vocab::families::contracts_claude_map::replay(|input| {
        let surface: RapidHashSet<String> = input.surface.iter().cloned().collect();
        match parse_messages_sse_typed(&claude_map_blocks_as_sse(&input.blocks), &surface) {
            Ok(events) => gents_lean_vocab::families::contracts_claude_map::ClaudeMapOutput {
                outcome: "ok".to_owned(),
                ids: events
                    .iter()
                    .filter_map(|event| match event {
                        RawStreamingChoice::ToolCall(call) => {
                            Some(call.id.parse::<u64>().expect("modeled Nat tool id"))
                        }
                        _ => None,
                    })
                    .collect(),
            },
            Err(error) => gents_lean_vocab::families::contracts_claude_map::ClaudeMapOutput {
                outcome: fail_closed_outcome(error),
                ids: Vec::new(),
            },
        }
    });
}

#[test]
fn claude_stream_follows_the_lean_model() {
    use gents_lean_vocab::families::contracts_claude_stream::{
        ClaudeStreamCall, ClaudeStreamOutput,
    };
    gents_lean_vocab::families::contracts_claude_stream::replay(|input| {
        let surface: RapidHashSet<String> = input.surface.iter().cloned().collect();
        match parse_messages_sse_typed(&claude_stream_events_as_sse(&input.events), &surface) {
            Ok(events) => ClaudeStreamOutput {
                outcome: "ok".to_owned(),
                calls: events
                    .iter()
                    .filter_map(|event| match event {
                        RawStreamingChoice::ToolCall(call) => {
                            let raw = call
                                .additional_params
                                .as_ref()
                                .and_then(|params| {
                                    params[gents_loop::rig_compat::RAW_TOOL_ARGUMENTS_PARAM]
                                        .as_str()
                                })
                                .expect("streamed tool arguments")
                                .to_owned();
                            assert_eq!(
                                call.arguments,
                                serde_json::from_str::<serde_json::Value>(&raw)
                                    .expect("streamed arguments are JSON")
                            );
                            Some(ClaudeStreamCall {
                                id: call.id.parse().expect("modeled Nat tool id"),
                                arguments: raw,
                            })
                        }
                        _ => None,
                    })
                    .collect(),
            },
            Err(error) => ClaudeStreamOutput {
                outcome: fail_closed_outcome(error),
                calls: Vec::new(),
            },
        }
    });
}

fn thinking_outcome(error: MessagesParseError) -> String {
    use crate::clients::claude_messages::ThinkingParseCause;
    match error {
        MessagesParseError::MalformedThinking { cause } => match cause {
            ThinkingParseCause::WrongBlock => "wrongBlock".to_owned(),
            ThinkingParseCause::WrongIndex(index) => format!("wrongIndex:{index}"),
            ThinkingParseCause::IncompleteBlock => "incompleteBlock".to_owned(),
            ThinkingParseCause::SignatureOrder => "signatureOrder".to_owned(),
            ThinkingParseCause::MissingSignature => "missingSignature".to_owned(),
            other => panic!("not a modeled thinking outcome: {other}"),
        },
        other => fail_closed_outcome(other),
    }
}

fn sealed_block(
    event: &RawStreamingChoice<crate::clients::claude_subscription::ClaudeStreamResponse>,
) -> Option<gents_lean_vocab::families::contracts_claude_thinking_stream::ClaudeStreamBlockWire> {
    use crate::llm::message::ReasoningContent;
    use gents_lean_vocab::families::contracts_claude_thinking_stream::{
        ClaudeReasoningPartWire, ClaudeStreamBlockWire,
    };
    match event {
        RawStreamingChoice::Message(value) => Some(ClaudeStreamBlockWire {
            kind: "text".to_owned(),
            value: Some(value.clone()),
            parts: None,
            id: None,
            name: None,
            arguments: None,
        }),
        RawStreamingChoice::Reasoning { content, .. } => {
            let part = match crate::llm::rig_compat::from_rig_reasoning_part(content) {
                ReasoningContent::Text { text, signature } => ClaudeReasoningPartWire {
                    kind: "text".to_owned(),
                    payload: text,
                    signature,
                },
                ReasoningContent::Redacted { data } => ClaudeReasoningPartWire {
                    kind: "redacted".to_owned(),
                    payload: data,
                    signature: None,
                },
                other => panic!("unsupported native Claude reasoning output: {other:?}"),
            };
            Some(ClaudeStreamBlockWire {
                kind: "reasoning".to_owned(),
                value: None,
                parts: Some(vec![part]),
                id: None,
                name: None,
                arguments: None,
            })
        }
        RawStreamingChoice::ToolCall(call) => Some(ClaudeStreamBlockWire {
            kind: "toolUse".to_owned(),
            value: None,
            parts: None,
            id: Some(call.id.parse().expect("modeled Nat tool id")),
            name: Some(call.name.clone()),
            arguments: Some(
                call.additional_params
                    .as_ref()
                    .and_then(|params| {
                        params[gents_loop::rig_compat::RAW_TOOL_ARGUMENTS_PARAM].as_str()
                    })
                    .expect("streamed tool arguments")
                    .to_owned(),
            ),
        }),
        _ => None,
    }
}

#[test]
fn claude_thinking_stream_follows_the_lean_model() {
    use gents_lean_vocab::families::contracts_claude_thinking_stream::{
        ClaudeContentStepWire, ClaudeThinkingStreamOutput,
    };
    use gents_loop::provider_audit::{ClaudeAuditEvent, ClaudeBlockKind};
    gents_lean_vocab::families::contracts_claude_thinking_stream::replay(|input| {
        let surface = input.surface.iter().cloned().collect::<RapidHashSet<_>>();
        let mut state = crate::clients::claude_messages::MessagesSseState::new(surface);
        let mut steps = Vec::new();
        let mut sealed = Vec::new();
        let mut open = None::<ClaudeBlockKind>;
        let mut thinking = String::new();
        let mut signature = None::<String>;
        let mut redacted = None::<String>;
        for sse in claude_thinking_sse_events(&input.events) {
            for line in sse.lines() {
                let emitted = match state.push_line_typed(line) {
                    Ok(emitted) => emitted,
                    Err(error) => {
                        return ClaudeThinkingStreamOutput {
                            outcome: thinking_outcome(error),
                            steps: Vec::new(),
                            content: Vec::new(),
                        }
                    }
                };
                for audit in state.take_audit_events() {
                    match audit {
                        ClaudeAuditEvent::BlockStart { kind, .. } => {
                            open = Some(kind);
                            thinking.clear();
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
                        ClaudeAuditEvent::BlockStop { .. } => open = None,
                        ClaudeAuditEvent::ThinkingText { .. } => {}
                    }
                }
                for event in &emitted {
                    if let RawStreamingChoice::ReasoningDelta { reasoning, .. } = event {
                        thinking.push_str(reasoning);
                    }
                    sealed.extend(sealed_block(event));
                }
            }
            let thinking_open = matches!(open, Some(ClaudeBlockKind::Thinking));
            steps.push(ClaudeContentStepWire {
                provisional_thinking: thinking_open.then(|| thinking.clone()),
                provisional_signature: signature.clone().filter(|_| thinking_open),
                provisional_redacted: redacted
                    .clone()
                    .filter(|_| matches!(open, Some(ClaudeBlockKind::RedactedThinking))),
                sealed: sealed.clone(),
            });
        }
        match state.finish_typed() {
            Ok(emitted) => {
                sealed.extend(emitted.iter().filter_map(sealed_block));
                ClaudeThinkingStreamOutput {
                    outcome: "ok".to_owned(),
                    steps,
                    content: sealed,
                }
            }
            Err(error) => ClaudeThinkingStreamOutput {
                outcome: thinking_outcome(error),
                steps,
                content: Vec::new(),
            },
        }
    });
}

#[test]
fn claude_thinking_seal_follows_the_lean_model() {
    use crate::llm::message::ReasoningContent;
    use gents_lean_vocab::families::contracts_claude_thinking_seal::ClaudeReasoningPartWire;
    use gents_loop::provider_input::ProviderInputProfile;
    use gents_loop::stream_processor::AssistantTurnAccumulator;

    gents_lean_vocab::families::contracts_claude_thinking_seal::replay(|input| {
        let surface = input.surface.iter().cloned().collect::<RapidHashSet<_>>();
        let events: Vec<
            gents_lean_vocab::families::contracts_claude_thinking_stream::ClaudeStreamEventWire,
        > = gents_lean_vocab::family::recode(&input.events);
        let parsed = parse_messages_sse(&claude_thinking_events_as_sse(&events), &surface)
            .expect("modeled stream parses");
        let mut accumulator = AssistantTurnAccumulator::default();
        for event in parsed {
            match event {
                RawStreamingChoice::ReasoningDelta { id, reasoning } => {
                    accumulator.push_reasoning_delta(id, &reasoning)
                }
                RawStreamingChoice::Reasoning { id, content } => {
                    let part = crate::llm::rig_compat::from_rig_reasoning_part(&content);
                    accumulator
                        .push_provider_reasoning(
                            ProviderInputProfile::ClaudeMessages,
                            Reasoning {
                                id,
                                content: vec![part],
                            },
                        )
                        .expect("accumulator takes Claude reasoning");
                }
                RawStreamingChoice::FinalResponse(_) => {}
                other => panic!("reasoning-only stream emitted {other:?}"),
            }
        }
        let Some(Message::Assistant { content, .. }) = accumulator.take_message() else {
            panic!("no canonical assistant message");
        };
        content
            .into_iter()
            .map(|block| {
                let AssistantContent::Reasoning(reasoning) = block else {
                    panic!("reasoning-only stream sealed {block:?}");
                };
                assert_eq!(reasoning.id, None);
                reasoning
                    .content
                    .into_iter()
                    .map(|part| match part {
                        ReasoningContent::Text { text, signature } => ClaudeReasoningPartWire {
                            kind: "text".to_owned(),
                            payload: text,
                            signature,
                        },
                        ReasoningContent::Redacted { data } => ClaudeReasoningPartWire {
                            kind: "redacted".to_owned(),
                            payload: data,
                            signature: None,
                        },
                        other => panic!("unexpected Claude reasoning: {other:?}"),
                    })
                    .collect()
            })
            .collect()
    });
}

#[test]
fn body_plan_follows_the_lean_model() {
    use gents_lean_vocab::families::claude_prompt_assembly_body_plan::{
        BodyPlan, BodyThinking, FieldDiagnostics,
    };
    let account = ClaudeAccountContext::projection();
    gents_lean_vocab::families::claude_prompt_assembly_body_plan::replay::<String>(|input| {
        let history: Vec<Message> = input
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
                    Message::user(input.first_user_text.clone())
                }
            })
            .collect();
        let tools: Vec<rig::completion::ToolDefinition> = input
            .tools
            .iter()
            .map(|name| rig::completion::ToolDefinition {
                name: name.clone(),
                description: name.clone(),
                parameters: serde_json::json!({"type": "object"}),
            })
            .collect();
        let mut request = claude_request(input.preamble.as_deref(), &history, tools);
        request.additional_params = Some(serde_json::json!({
            (crate::clients::claude_messages::ADVERTISED_REASONING_EFFORTS_PARAM): input.supported_efforts,
            "output_config": {"effort": input.effort}
        }));
        let context = ClaudeRequestContext::client_turn("conformance-session");
        let ids = ClaudeRequestIds {
            session: context.ids().session,
            prompt: input.prompt_id.clone(),
        };
        let plan = plan_with_ids(&input.model, &request, &account, &context, &ids)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            plan.body.tools.as_ref().map(Vec::len),
            Some(input.tools.len())
        );

        let fingerprint = attribution::fingerprint(&input.first_user_text, profile::VERSION);
        let body = serde_json::to_value(&plan.body).expect("body JSON");
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
        assert!(!leaked, "System row leaked into messages: {body}");
        let facts = ModelFacts::of(catalog::find(&input.model).expect("a catalog model"));
        let thinking = &body["thinking"];
        let display_updates = thinking["display"].as_str() == Some("updates");
        BodyPlan {
            system: plan
                .body
                .system
                .iter()
                .map(|block| {
                    block
                        .text
                        .replace(&fingerprint, &input.fingerprint_placeholder)
                })
                .collect(),
            system_marked: plan
                .body
                .system
                .iter()
                .map(|block| block.cache_control.is_some())
                .collect(),
            tools_present: plan.body.tools.is_some(),
            facts: gents_lean_vocab::families::claude_prompt_assembly_body_plan::ModelFacts {
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
                    .map(|effort| effort.as_str().to_owned()),
            },
            max_tokens: body["max_tokens"].as_u64().expect("max_tokens"),
            thinking: match thinking["type"].as_str() {
                None => BodyThinking {
                    kind: "none".to_owned(),
                    budget: None,
                    updates: false,
                },
                Some("disabled") => BodyThinking {
                    kind: "disabled".to_owned(),
                    budget: None,
                    updates: display_updates,
                },
                Some(kind) => BodyThinking {
                    kind: kind.to_owned(),
                    budget: thinking["budget_tokens"].as_u64(),
                    updates: display_updates,
                },
            },
            temperature: body["temperature"].as_u64(),
            context_management: body.get("context_management").is_some(),
            body_effort: body["output_config"]["effort"].as_str().map(str::to_owned),
            diagnostics: FieldDiagnostics {
                present: body.get("diagnostics").is_some(),
                previous_message_id: body["diagnostics"]["previous_message_id"]
                    .as_str()
                    .map(str::to_owned),
            },
            per_turn_effort: body["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .find(|message| message["role"] == "system")
                .and_then(|message| message["output_config"]["effort"].as_str())
                .map(str::to_owned),
        }
    });
}
