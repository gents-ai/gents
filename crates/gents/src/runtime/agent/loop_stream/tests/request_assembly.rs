fn generated_steering_prepared_prompt(
    case: &gents_lean_vocab::LeanQueuedSteeringTraceCase,
) -> Message {
    use gents_lean_vocab::{
        LeanCanonicalClosure, LeanCanonicalSource, LeanCanonicalWriter, LeanMessageBlock,
        LeanMessagePublication, LeanMessageRole, LeanOutcome, LeanPayloadKind, LeanPresentation,
    };

    let candidate = case
        .prepared_candidate
        .as_ref()
        .expect("generated publication has no prepared candidate");
    let segment = &candidate.closing;
    let message = &candidate.message;
    assert_eq!(segment.coordinate.request, case.request_doc_id);
    assert!(matches!(
        &segment.coordinate.source,
        LeanCanonicalSource::Authored { .. }
    ));
    let LeanCanonicalWriter::Request { generation } = &segment.writer else {
        panic!("{} has a non-request authored writer", case.name)
    };
    assert!(matches!(&message.header.publication,
        LeanMessagePublication::RequestExecution { generation: owner } if owner == generation));
    assert_eq!(message.header.request, Some(case.request_doc_id));
    assert!(matches!(&message.header.role, LeanMessageRole::User));
    assert!(matches!(&message.header.outcome, LeanOutcome::Complete));
    let [LeanMessageBlock::Text { payload }] = message.blocks.as_slice() else {
        panic!("{} is not a single prepared user text block", case.name)
    };
    assert!(matches!(&payload.presentation, LeanPresentation::Full));
    assert_eq!(payload.reference.close_id, segment.id);
    assert_eq!(payload.reference.stream, 0);
    assert_eq!(
        message.header.refs.as_slice(),
        std::slice::from_ref(&payload.reference)
    );
    let flush = segment.flush.as_ref().expect("prepared text lacks a flush");
    assert_eq!(flush.ordinal, 0);
    let [run] = flush.runs.as_slice() else {
        panic!("{} is not a single prepared text stream", case.name)
    };
    assert_eq!(run.stream, payload.reference.stream);
    assert_eq!(
        usize::try_from(run.bytes).expect("prepared run exceeds usize"),
        flush.payload.len()
    );
    let declaration = run
        .declaration
        .as_ref()
        .expect("prepared text lacks declaration");
    assert_eq!((declaration.block, declaration.part), (0, 0));
    assert!(matches!(&declaration.kind, LeanPayloadKind::Text));
    assert!(matches!(&segment.close,
        Some(LeanCanonicalClosure::Closed { outcome: LeanOutcome::Complete, segments: 1, stream_bytes })
        if stream_bytes.as_slice() == [run.bytes]));
    Message::user(String::from_utf8(flush.payload.clone()).expect("prepared text is UTF-8"))
}

#[tokio::test]
async fn generated_authored_input_is_durable_before_provider_stream_entry() {
    use gents_lean_vocab::LeanQueuedSteeringAction as Action;

    let cases = gents_lean_vocab::lean_queued_steering_trace_cases();
    let case = cases
        .iter()
        .find(|case| {
            matches!(
                case.actions.as_slice(),
                [
                    Action::Enqueue,
                    Action::ClaimAndBegin,
                    Action::Publish,
                    Action::Capture,
                    Action::Send
                ]
            )
        })
        .expect("generated publication handoff script");
    assert!(case.prepared_candidate.is_some(), "{}", case.name);
    assert_eq!(
        case.capture.request_doc_id, case.request_doc_id,
        "{}",
        case.name
    );

    let (_db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let doc_id = lifecycle.request().doc_id.clone();
    let node_did = lifecycle.request().node_did.clone();
    let requester_did = lifecycle.request().requester_did.clone();
    let prompt = generated_steering_prepared_prompt(case);
    let gate = Arc::new(StreamEntryGate::default());
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("accepted".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ])
    .with_stream_entry_gate(gate.clone());
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(prompt.clone()),
        Vec::new(),
        Arc::new(Vec::new()),
        owned_config(0),
    );
    let scoped_doc_id = gents_store::graphql::escape_graphql_string(&doc_id);
    let query = format!(
        r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{scoped_doc_id}" }} }}) {{ _docID message_key }} }}"#
    );
    let reader = lifecycle.access().unwrap();
    let before = agent_response(&reader, &query).await;
    assert!(!before.has_errors(), "{}: {:?}", case.name, before.errors);
    assert!(before.data.unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .is_empty());

    let (collected, ()) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(
            async {
                let collected = collect_owned_scripted_stream(
                    stream,
                    &hook,
                    &writer,
                    &mut lifecycle,
                    gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
                )
                .await;
                assert_eq!(collected.error, None, "{}", case.name);
                assert!(
                    !model.seen_requests().await.is_empty(),
                    "{} ended before entering the provider stream",
                    case.name
                );
                collected
            },
            async {
                gate.entered.notified().await;
                let observed = agent_response(&reader, &query).await;
                assert!(
                    !observed.has_errors(),
                    "{}: {:?}",
                    case.name,
                    observed.errors
                );
                let observed = observed.data.unwrap();
                let headers = observed["AgentMessage"].as_array().unwrap();
                assert_eq!(
                    headers.len() as u64,
                    case.canonical_authored_count,
                    "{}",
                    case.name
                );
                let [header] = headers.as_slice() else {
                    panic!("{} has no unique authored input", case.name)
                };
                assert_eq!(
                    header["message_key"],
                    gents_protocol::output::rows::authored_message_key(&doc_id, "prompt"),
                    "{}",
                    case.name
                );
                let header_id = header["_docID"].as_str().unwrap();
                let (_, native) = gents_session::session::load_canonical_message(
                    &reader,
                    header_id,
                    &node_did,
                    requester_did.as_deref(),
                )
                .await
                .unwrap();
                assert_eq!(native, prompt, "{}", case.name);
                gate.release.notify_one();
            }
        )
    })
    .await
    .expect("provider stream did not reach the authored-input handoff");
    assert_eq!(collected.error, None, "{}", case.name);
    let sent = model.seen_requests().await;
    assert_eq!(
        sent.len(),
        usize::from(case.provider_send_permitted),
        "{}",
        case.name
    );
}

#[tokio::test]
async fn loop_entry_sanitizes_a_recovered_checkpoint_as_one_projection() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("continued".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let call = Message::Assistant {
        request_id: None,
        id: None,
        content: vec![AssistantContent::ToolCall(
            gents_inference::llm::message::ToolCall {
                raw_arguments: None,
                id: "restored-call".to_string(),
                call_id: Some("restored-call".to_string()),
                function: gents_inference::llm::message::ToolFunction {
                    name: "read".to_string(),
                    arguments: serde_json::json!({}),
                },
                signature: None,
                additional_params: None,
            },
        )],
    };
    let result = Message::User {
        content: vec![UserContent::ToolResult(
            gents_inference::llm::message::ToolResult {
                id: "restored-call".to_string(),
                call_id: Some("restored-call".to_string()),
                content: vec![ToolResultContent::text("restored result")],
            },
        )],
    };

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None::<gents_toolcalls::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(result),
        (vec![call])
            .into_iter()
            .map(TaggedMessage::unassociated)
            .collect(),
        Arc::new(Vec::new()),
        config(0),
    ))
    .await;

    assert_eq!(collected.error, None);
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 1);
    assert!(history_has_tool_call(&histories[0], "read"));
    assert!(history_has_tool_result_text(
        &histories[0],
        "restored result"
    ));
}

fn assert_provider_request_invariants(turn: usize, history: &[Message]) {
    let mut pending_call_keys: Vec<String> = Vec::new();
    for message in history {
        match message {
            Message::Assistant { content, .. } => {
                assert!(
                    pending_call_keys.is_empty(),
                    "turn {turn}: assistant message before prior turn's tool calls were resolved"
                );
                let mut seen_tool_call = false;
                for item in content.iter() {
                    match item {
                        AssistantContent::ToolCall(tool_call) => {
                            seen_tool_call = true;
                            pending_call_keys.push(
                                tool_call
                                    .call_id
                                    .clone()
                                    .unwrap_or_else(|| tool_call.id.clone()),
                            );
                        }
                        _ => assert!(
                            !seen_tool_call,
                            "turn {turn}: assistant content after a tool call (providers reject)"
                        ),
                    }
                }
            }
            Message::User { content } => {
                let has_tool_results = content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_)));
                assert!(
                    has_tool_results || pending_call_keys.is_empty(),
                    "turn {turn}: ordinary user content before prior tool calls were resolved"
                );
                for item in content.iter() {
                    if let UserContent::ToolResult(tool_result) = item {
                        let key = tool_result
                            .call_id
                            .clone()
                            .unwrap_or_else(|| tool_result.id.clone());
                        let position = pending_call_keys.iter().position(|call| call == &key);
                        assert!(
                            position.is_some(),
                            "turn {turn}: tool result '{key}' without a preceding tool call"
                        );
                        pending_call_keys.remove(position.unwrap());
                    }
                }
            }
            Message::System { .. } => assert!(
                pending_call_keys.is_empty(),
                "turn {turn}: system message before prior tool calls were resolved"
            ),
        }
    }
    assert!(
        pending_call_keys.is_empty(),
        "turn {turn}: unpaired tool calls reached the provider: {pending_call_keys:?}"
    );
}

#[tokio::test]
async fn every_request_in_a_tool_loop_satisfies_provider_invariants() {
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::Message("let me check".to_string()),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-2".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);

    let stream = run_loop_stream(
        model.clone(),
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("run the tools")),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        3,
        "expected three completion turns; got {histories:?}"
    );
    for (index, history) in histories.iter().enumerate() {
        assert_provider_request_invariants(index + 1, history);
    }
}

#[tokio::test]
async fn dirty_caller_history_is_sanitized_at_loop_entry() {
    let unpaired_call = gents_inference::llm::message::ToolCall {
        raw_arguments: None,
        id: "call-unpaired".to_string(),
        call_id: Some("call-unpaired".to_string()),
        function: gents_inference::llm::message::ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
        signature: None,
        additional_params: None,
    };
    let paired_call = gents_inference::llm::message::ToolCall {
        raw_arguments: None,
        id: "call-paired".to_string(),
        call_id: Some("call-paired".to_string()),
        function: gents_inference::llm::message::ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
        signature: None,
        additional_params: None,
    };
    let dirty_history = vec![
        Message::User {
            content: vec![UserContent::tool_result(
                "call-gone".to_string(),
                vec![gents_inference::llm::message::ToolResultContent::text(
                    "orphaned",
                )],
            )],
        },
        Message::Assistant {
            request_id: None,
            id: None,
            content: vec![
                AssistantContent::ToolCall(paired_call),
                AssistantContent::ToolCall(unpaired_call),
                AssistantContent::Text(gents_inference::llm::message::Text {
                    text: "stale ordering".to_string(),
                }),
            ],
        },
        Message::User {
            content: vec![UserContent::tool_result(
                "call-paired".to_string(),
                vec![gents_inference::llm::message::ToolResultContent::text("ok")],
            )],
        },
    ];

    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("hi".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let stream = run_loop_stream(
        model.clone(),
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("continue")),
        (dirty_history)
            .into_iter()
            .map(TaggedMessage::unassociated)
            .collect(),
        Arc::new(Vec::new()),
        config(1),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 1);
    assert_provider_request_invariants(1, &histories[0]);
    let kept_calls: Vec<String> = histories[0]
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::ToolCall(call) => Some(call.id.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        kept_calls,
        vec!["call-paired".to_string()],
        "unpaired call dropped, paired call kept; history: {:?}",
        histories[0]
    );
}

#[tokio::test]
async fn corrupt_589_tool_args_salvage_runs_and_history_stays_object_shaped() {
    use gents_loop::tool::{Tool, ToolDefinition};

    struct DescribeTool;
    #[derive(Debug, thiserror::Error)]
    #[error("describe tool error")]
    struct DescribeToolError;
    #[derive(serde::Deserialize)]
    struct DescribeArgs {
        tool_name: String,
    }
    impl Tool for DescribeTool {
        const NAME: &'static str = "describe_tool";
        type Error = DescribeToolError;
        type Args = DescribeArgs;
        type Output = String;
        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
            Ok(format!("described:{}", args.tool_name))
        }
    }

    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();

    let poison =
        serde_json::Value::String(gents_store::test_support::CORRUPT_TOOL_ARGS_589.to_string());
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "describe_tool".to_string(),
                poison,
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(DescribeTool)];

    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("describe list_hosts")),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);

    assert_eq!(
        collected.tool_results,
        vec!["described:list_hosts".to_string()],
        "the salvageable #589 payload must run the intended call, not waste a turn"
    );

    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert_all_history_tool_args_object_shaped(&histories[1]);
    drop(node);
}

#[tokio::test]
async fn nonobject_tool_args_never_reach_durable_history_or_provider() {
    use gents_loop::tool::{Tool, ToolDefinition};

    struct StrictTool;
    #[derive(Debug, thiserror::Error)]
    #[error("strict tool error")]
    struct StrictToolError;
    #[derive(serde::Deserialize)]
    struct StrictArgs {
        #[allow(dead_code)]
        tool_name: String,
    }
    impl Tool for StrictTool {
        const NAME: &'static str = "describe_tool";
        type Error = StrictToolError;
        type Args = StrictArgs;
        type Output = String;
        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
            panic!("the tool must not run on non-object arguments");
        }
    }

    let (_db, hook, writer, mut lifecycle) = owned_test_hook().await;

    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "describe_tool".to_string(),
                serde_json::json!([]),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictTool)];

    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("describe")),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert!(
        collected.error.is_none(),
        "non-object args must be notified, not raised: {:?}",
        collected.error
    );

    let resp = agent_response(
        &lifecycle.access().unwrap(),
        "query { AgentToolCall { tool_name lifecycle_state tool_failure_class } }",
    )
    .await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|v| v.as_str()) == Some("describe_tool")
                && row.get("lifecycle_state").and_then(|v| v.as_str()) == Some("failed")
                && row.get("tool_failure_class").and_then(|v| v.as_str()) == Some("argumentInvalid")
        }),
        "a non-object-args call must terminalize failed/argumentInvalid, got: {rows:?}"
    );

    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert_all_history_tool_args_object_shaped(&histories[1]);
}

fn assert_all_history_tool_args_object_shaped(history: &[Message]) {
    for message in history {
        if let Message::Assistant { content, .. } = message {
            for item in content {
                if let AssistantContent::ToolCall(tool_call) = item {
                    assert!(
                        tool_call.function.arguments.is_object(),
                        "non-object tool-call arguments reached the provider: {:?}",
                        tool_call.function.arguments
                    );
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn repair_sanitizes_poisoned_tool_args_in_loaded_history() {
    let poison = format!("bad{}value", '\u{0007}');

    let poisoned_call = gents_inference::llm::message::ToolCall {
        raw_arguments: None,
        id: "call-historic".to_string(),
        call_id: Some("call-historic".to_string()),
        function: gents_inference::llm::message::ToolFunction {
            name: "echo".to_string(),
            arguments: serde_json::json!({ "note": poison }),
        },
        signature: None,
        additional_params: None,
    };
    let history = vec![
        Message::user("earlier"),
        Message::Assistant {
            request_id: None,
            id: None,
            content: vec![AssistantContent::ToolCall(poisoned_call)],
        },
        Message::User {
            content: vec![UserContent::tool_result(
                "call-historic",
                vec![ToolResultContent::text("ok")],
            )],
        },
    ];
    assert!(
        history_has_control_char_tool_arg(&history),
        "the fixture must actually carry poisoned history"
    );

    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("repaired".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let stream = run_loop_stream(
        model.clone(),
        None::<gents_toolcalls::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("hi")),
        (history)
            .into_iter()
            .map(TaggedMessage::unassociated)
            .collect(),
        Arc::new(Vec::new()),
        config(0),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text.as_deref(), Some("repaired"));
    assert_eq!(collected.error, None);

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        3,
        "parse failure, resample, and the repaired retry"
    );
    assert!(
        history_has_control_char_tool_arg(&histories[0]),
        "the poisoned history must reach the provider before repair: {:?}",
        histories[0]
    );
    assert!(
        !history_has_control_char_tool_arg(&histories[2]),
        "repair must sanitize tool arguments in the LOADED HISTORY, not only \
         the run-threaded messages — otherwise it re-issues the same poisoned \
         input and fails identically (#652): {:?}",
        histories[2]
    );
}

fn sole_user_text(message: &Message) -> String {
    match message {
        Message::User { content } => match content.as_slice() {
            [UserContent::Text(text)] => text.text.clone(),
            other => panic!("layer fence built an unexpected user message: {other:?}"),
        },
        other => panic!("layer fence built an unexpected message: {other:?}"),
    }
}

fn classify_slot(message: &Message, is_last: bool, conversation_index: &mut usize) -> String {
    if super::is_request_context_message(message) {
        return "contextPreamble".to_string();
    }
    if is_last {
        return "prompt".to_string();
    }
    let text = sole_user_text(message);
    if let Some(body) = text.strip_prefix("<system-reminder>\n") {
        if body.starts_with("Continuation checkpoints from earlier conversation") {
            return "summaryReminder".to_string();
        }
        if let Some(rest) = body.strip_prefix("skill-") {
            let digits = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            return format!("skillReminder:{digits}");
        }
    }
    let slot = format!("conversation:{conversation_index}");
    *conversation_index += 1;
    slot
}

#[tokio::test]
async fn generated_layer_cases_pin_the_assembled_request_order() {
    use crate::turn::prompt::{LayeredPromptBuilder, PromptBuilder};
    use gents_lean_vocab::lean_prompt_assembly_layer_cases;

    let cases = lean_prompt_assembly_layer_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly layer cases"
    );

    for case in cases {
        let builder =
            LayeredPromptBuilder::for_agent("system prompt", "fence", &["bash"], false, &[]);

        let conversation = (0..case.conversation_len)
            .map(|index| {
                TaggedMessage::unassociated(Message::user(format!("conversation-{index}")))
            })
            .collect::<Vec<_>>();
        let summaries = (0..case.summary_count)
            .map(|index| format!("summary-{index}"))
            .collect::<Vec<_>>();
        let skill_reminders = (0..case.skill_count)
            .map(|index| LayeredPromptBuilder::system_reminder(&format!("skill-{index}")))
            .collect::<Vec<_>>();

        let built = builder
            .build(&conversation, &summaries)
            .await
            .expect("build layered prompt");

        let mut assembled = skill_reminders
            .into_iter()
            .map(TaggedMessage::unassociated)
            .collect::<Vec<_>>();
        assembled.extend(built.messages);
        assembled.extend(super::assemble_new_messages(
            None,
            TaggedMessage::unassociated(Message::user("prompt")),
        ));

        assert!(
            !builder.preamble().is_empty(),
            "the preamble slot must be carried by the system-prompt field"
        );
        let mut slots = vec!["preamble".to_string()];
        let mut conversation_index = 0usize;
        let assembled_len = assembled.len();
        for (position, message) in assembled.iter().enumerate() {
            slots.push(classify_slot(
                &message.message,
                position + 1 == assembled_len,
                &mut conversation_index,
            ));
        }

        assert_eq!(
            slots, case.slots,
            "assembled layer order drifted from the Lean model on case {:?}",
            case.name
        );
    }
}

fn generated_authored_key(
    key: &gents_lean_vocab::LeanFoldAuthoredKey,
    docs: &rapidhash::RapidHashMap<u64, String>,
) -> String {
    use gents_lean_vocab::LeanFoldAuthoredKey as Key;
    match key {
        Key::Context => "context".to_owned(),
        Key::Prompt => "prompt".to_owned(),
        Key::Folded { request_id } => {
            gents_session::request::lifecycle::queue::folded_input_key(&docs[request_id])
        }
    }
}

async fn drive_generated_turn_input(
    case: &gents_lean_vocab::LeanFoldTurnInputCase,
    first_turn_compaction: bool,
) {
    let (db, hook, writer, mut lifecycle, docs) = owned_test_hook_with_folded_case(case).await;
    let node = db.node.clone();
    let head = lifecycle.request().clone();
    assert_eq!(
        lifecycle
            .folded_selection()
            .iter()
            .map(|row| row.request_doc_id.clone())
            .collect::<Vec<_>>(),
        case.folded
            .iter()
            .map(|row| docs[&row.request_id].clone())
            .collect::<Vec<_>>(),
        "{}: native claim selects the modeled queue",
        case.name,
    );
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("answered".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let mut config = owned_config(0);
    config.context_message = case.context.clone().map(Message::user);
    config.folded_prompts = case
        .folded
        .iter()
        .map(|folded| crate::runtime::agent::loop_stream::FoldedPrompt {
            key: gents_session::request::lifecycle::queue::folded_input_key(
                &docs[&folded.request_id],
            ),
            message: Message::user(folded.content.clone()),
        })
        .collect();
    let compactor_saw = Arc::new(std::sync::Mutex::new(Vec::<Message>::new()));
    let published_before_reduction = Arc::new(std::sync::Mutex::new(0_usize));
    if first_turn_compaction {
        config.max_tokens = Some(6_000);
        config.context_window = 6_500;
        config.compaction_threshold = 0.25;
        let saw = compactor_saw.clone();
        let published_before_reduction = published_before_reduction.clone();
        let compaction_reader = lifecycle.access().unwrap();
        let request_doc_id = head.doc_id.clone();
        config.turn_compactor = Some(Arc::new(move |request| {
            *saw.lock().unwrap() = request
                .messages
                .iter()
                .map(|row| row.message.clone())
                .collect();
            let published_before_reduction = published_before_reduction.clone();
            let reader = compaction_reader.clone();
            let request_doc_id = request_doc_id.clone();
            Box::pin(async move {
                let observed = agent_response(&reader, &format!(
                        r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }}, role: {{ _eq: "user" }} }}) {{ _docID }} }}"#,
                        gents_store::graphql::escape_graphql_string(&request_doc_id)
                    )).await;
                *published_before_reduction.lock().unwrap() = observed.data.unwrap()
                    ["AgentMessage"]
                    .as_array()
                    .unwrap()
                    .len();
                Ok(TurnCompactionOutcome::Reduced {
                    messages: vec![TaggedMessage::unassociated(Message::user(
                        "compacted prompt",
                    ))],
                    reduction_key: "folded-first-turn".to_string(),
                    commit: TurnReductionCommit::new(|| async { Ok(()) }),
                })
            })
        }));
    }
    let collected = collect_owned_scripted_stream(
        run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user(case.head.clone())),
            if first_turn_compaction {
                vec![
                    TaggedMessage::unassociated(Message::user("x".repeat(8_000))),
                    TaggedMessage::unassociated(Message::assistant("noted")),
                ]
            } else {
                Vec::new()
            },
            Arc::new(Vec::new()),
            config,
        ),
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert_eq!(collected.error, None, "{}", case.name);

    let expected_input = case
        .provider_input
        .iter()
        .cloned()
        .map(Message::user)
        .collect::<Vec<_>>();
    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        1,
        "{}: one inference answers every message",
        case.name
    );
    if first_turn_compaction {
        let saw = compactor_saw.lock().unwrap().clone();
        assert!(
            saw.ends_with(&expected_input),
            "{}: compaction input",
            case.name
        );
        assert_eq!(
            *published_before_reduction.lock().unwrap(),
            case.authored.len(),
            "{}: the admitted input is accepted before any reduction",
            case.name
        );
        assert_eq!(
            histories[0].last(),
            Some(&Message::user("compacted prompt"))
        );
    } else {
        assert!(
            histories[0].ends_with(&expected_input),
            "{}: provider order",
            case.name
        );
    }

    let scoped_doc_id = gents_store::graphql::escape_graphql_string(&head.doc_id);
    let observed = agent_response(&lifecycle.access().unwrap(), &format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{scoped_doc_id}" }}, role: {{ _eq: "user" }} }}, order: {{ sequence: ASC }}) {{ _docID message_key }} }}"#
        )).await;
    assert!(!observed.has_errors(), "{:?}", observed.errors);
    let observed = observed.data.unwrap();
    let headers = observed["AgentMessage"].as_array().unwrap();
    assert_eq!(
        headers.len(),
        case.authored.len(),
        "{}: authored entries",
        case.name
    );
    for (header, authored) in headers.iter().zip(&case.authored) {
        assert_eq!(
            header["message_key"],
            gents_protocol::output::rows::authored_message_key(
                &head.doc_id,
                &generated_authored_key(&authored.key, &docs)
            ),
            "{}",
            case.name
        );
        let (_, native) = gents_session::session::load_canonical_message(
            &lifecycle.access().unwrap(),
            header["_docID"].as_str().unwrap(),
            &head.node_did,
            head.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(
            native,
            Message::user(authored.content.clone()),
            "{}",
            case.name
        );
    }
    for doc in docs.values() {
        let response = agent_query(
            &lifecycle.access().unwrap(),
            &format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state superseded_by_request_doc_id }} }}"#,
                gents_store::graphql::escape_graphql_string(doc)
            ),
        )
        .await;
        let row = &response["data"]["AgentRequest"][0];
        assert_eq!(row["lifecycle_state"], "superseded", "{}", case.name);
        assert_eq!(
            row["superseded_by_request_doc_id"],
            head.doc_id.as_str(),
            "{}",
            case.name
        );
    }

    let history = gents_session::session::load_sequenced_history_for_request(
        &node,
        lifecycle.request(),
        None,
        None,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        false,
    )
    .await
    .unwrap();
    assert!(history
        .iter()
        .all(|row| !matches!(&row.message, Message::User { .. })));
    let resumed_history = gents_session::session::load_sequenced_history_for_request(
        &node,
        lifecycle.request(),
        None,
        None,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        true,
    )
    .await
    .unwrap();
    assert!(resumed_history
        .iter()
        .any(|row| matches!(&row.message, Message::User { .. })));
}

#[tokio::test]
async fn folded_prompts_publish_and_send_in_queue_order() {
    let cases = gents_lean_vocab::lean_fold_turn_input_cases();
    assert!(!cases.is_empty());
    for case in cases {
        drive_generated_turn_input(case, false).await;
    }
}

#[tokio::test]
async fn first_turn_compaction_keeps_folded_authored_input() {
    for case in gents_lean_vocab::lean_fold_turn_input_cases() {
        drive_generated_turn_input(case, true).await;
    }
}
