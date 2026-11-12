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
