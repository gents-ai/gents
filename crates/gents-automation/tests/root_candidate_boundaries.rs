use futures::StreamExt;
use gents_automation::test_support::loop_stream::invalid_tool_progress::*;
use gents_automation::test_support::loop_stream::repeated_tool_failure::*;
use gents_automation::test_support::loop_stream::request_assembly::*;
use gents_automation::test_support::loop_stream::tool_execution::*;
use gents_automation::test_support::loop_stream::*;
use gents_inference::llm::message::{Message, ToolResultContent};
use gents_inference::llm::ToolCallHookAction;
use gents_loop::loop_stream::*;
use gents_loop::tool::ToolDyn;
use gents_toolcalls::call::hook::fixtures::*;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall};
use std::sync::Arc;
use std::time::Duration;
#[tokio::test]
async fn other_provider_loop_has_no_codex_affinity() {
    let (_db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("done".into()),
        RawStreamingChoice::FinalResponse(()),
    ])
    .with_provider_observer(Arc::new(|_| {
        assert!(gents_loop::provider_input::routing_affinity::current().is_none());
    }));
    let stream = run_loop_stream(
        model,
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("hi")),
        Vec::new(),
        Arc::new(Vec::new()),
        owned_config(0),
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert_eq!(collected.error, None, "{collected:?}");
}

#[tokio::test]
async fn invalid_tool_budget_closes_eighth_result_and_cancels_accepted_ninth() {
    for batched in [false, true] {
        let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
        let node = db.node.clone();
        let mut chunks = (0..8)
            .map(|index| invalid_progress_call(index, "policyDenied"))
            .collect::<Vec<_>>();
        if batched {
            chunks.push(invalid_progress_call(8, "success"));
        }
        chunks.push(RawStreamingChoice::FinalResponse(()));
        let model = ScriptedModel::new(chunks);
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("bound invalid batch")),
            Vec::new(),
            Arc::new(vec![Box::new(InvalidProgressProbe) as Box<dyn ToolDyn>]),
            owned_config(500),
        );
        let collected = tokio::time::timeout(
            Duration::from_secs(10),
            Box::pin(collect_owned_scripted_stream(
                stream,
                &hook,
                &writer,
                &mut lifecycle,
                gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
            )),
        )
        .await
        .expect("accepted batch must terminate at the invalid-tool budget");
        assert_eq!(collected.tool_results.len(), 8);
        let error = collected
            .error
            .expect("budget exhaustion must fail the loop");
        assert!(
            error.contains("invalid_tool_call_budget_exhausted:"),
            "{error}"
        );
        assert_eq!(model.seen_requests().await.len(), 1);
        lifecycle
            .terminalize_owned(
                gents_session::request::lifecycle::RequestTerminalOutcome::Failed,
                writer.terminal_output(&lifecycle.request().doc_id).await,
                Some(&error),
            )
            .await
            .expect("terminal owner settles every published undispatched call");
        let data = agent_query(
            &lifecycle.access().unwrap(),
            "{ AgentToolCall { _docID tool_call_id lifecycle_state } }",
        )
        .await["data"]
            .clone();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            if batched { 9 } else { 8 },
            "all accepted intents remain durable, including the undispatched ninth"
        );
        if batched {
            let ninth = rows
                .iter()
                .find(|row| row["tool_call_id"] == "invalid-progress-8")
                .unwrap();
            assert_eq!(ninth["lifecycle_state"], "cancelled");
        }
        for index in 0..8 {
            let row = rows
                .iter()
                .find(|row| row["tool_call_id"] == format!("invalid-progress-{index}"))
                .unwrap_or_else(|| panic!("missing durable row {index}"));
            assert_eq!(row["lifecycle_state"], "failed");
            let output = agent_tool_output(&lifecycle, row["_docID"].as_str().unwrap()).await;
            assert!(
                !output.is_empty(),
                "canonical output {index} must be delivered"
            );
        }
        node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn malformed_bash_feedback_reaches_next_request_and_corrected_argv_succeeds() {
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("crates")).unwrap();
    std::fs::write(root.path().join("crates/visible-proof.txt"), "fixture").unwrap();
    let calls = [
        serde_json::json!({"command":"ls crates"}),
        serde_json::json!({"command":"ls","args":["crates"]}),
    ];
    let mut turns = calls
        .into_iter()
        .enumerate()
        .map(|(index, args)| {
            vec![
                RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    format!("bash-format-{index}"),
                    "bash".into(),
                    args,
                )),
                RawStreamingChoice::FinalResponse(()),
            ]
        })
        .collect::<Vec<_>>();
    turns.push(vec![
        RawStreamingChoice::Message("reviewed".into()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let model = ScriptedModel::new_turns(turns);
    let tools = gents_tools::native::ToolSet::builder()
        .read_root(root.path())
        .bash_read_only()
        .build()
        .build_native_tools()
        .unwrap();
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("inspect crates")),
        Vec::new(),
        Arc::new(tools),
        owned_config(10),
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
    let requests = model.seen_requests().await;
    assert_eq!(requests.len(), 3);
    let feedback = serde_json::to_string(
        &requests[1]
            .chat_history
            .iter()
            .map(gents_inference::llm::rig_compat::from_rig_message)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(
        feedback.contains("bash-format-0"),
        "feedback must match the first call ID"
    );
    assert!(
        feedback.contains("executable") && feedback.contains("args"),
        "{feedback}"
    );
    let corrected = serde_json::to_string(
        &requests[2]
            .chat_history
            .iter()
            .map(gents_inference::llm::rig_compat::from_rig_message)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(
        corrected.contains("visible-proof.txt"),
        "successful corrected result must reach provider: {corrected}"
    );
    let bash = requests[1]
        .tools
        .iter()
        .find(|tool| tool.name == "bash")
        .unwrap();
    assert!(bash.parameters["properties"]["command"]["description"]
        .as_str()
        .unwrap()
        .contains("executable"));
    let data = agent_query(
        &lifecycle.access().unwrap(),
        "{ AgentToolCall { _docID tool_call_id lifecycle_state tool_failure_class } }",
    )
    .await["data"]
        .clone();
    let rows = data["AgentToolCall"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|row| row["lifecycle_state"] == "failed"
            && row["tool_failure_class"] == "argumentInvalid"));
    let tool_doc_id = rows
        .iter()
        .find(|row| row["lifecycle_state"] == "completed")
        .and_then(|row| row["_docID"].as_str())
        .unwrap();
    let output = agent_tool_output(&lifecycle, tool_doc_id).await;
    assert!(
        output.contains("visible-proof.txt"),
        "canonical output must carry the successful bash listing: {output}"
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn empty_bash_arguments_exhaust_owned_loop_without_side_effects() {
    for arguments in [serde_json::json!({}), serde_json::json!("")] {
        let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
        let node = db.node.clone();
        let root = tempfile::tempdir().unwrap();
        let mut turns = (0..9)
            .map(|index| {
                vec![
                    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                        format!("empty-bash-{index}"),
                        "bash".into(),
                        arguments.clone(),
                    )),
                    RawStreamingChoice::FinalResponse(()),
                ]
            })
            .collect::<Vec<_>>();
        turns.push(vec![
            RawStreamingChoice::Message("unreached".into()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let model = ScriptedModel::new_turns(turns);
        let tools = gents_tools::native::ToolSet::builder()
            .read_root(root.path())
            .bash_read_only()
            .build()
            .build_native_tools()
            .unwrap();
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("inspect source")),
            Vec::new(),
            Arc::new(tools),
            owned_config(500),
        );
        let collected = collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )
        .await;
        let error = collected
            .error
            .expect("budget exhaustion must fail the loop");
        assert!(
            error.contains("invalid_tool_call_budget_exhausted:"),
            "unexpected owned loop error: {error}"
        );
        assert_eq!(model.seen_requests().await.len(), 8);
        let data = agent_query(
            &lifecycle.access().unwrap(),
            "{ AgentToolCall { _docID tool_call_id lifecycle_state tool_failure_class } }",
        )
        .await["data"]
            .clone();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().all(|row| row["lifecycle_state"] == "failed"
            && row["tool_failure_class"] == "argumentInvalid"));
        for index in 0..8 {
            let row = rows
                .iter()
                .find(|row| row["tool_call_id"] == format!("empty-bash-{index}"))
                .unwrap_or_else(|| panic!("missing durable row {index}"));
            let output = agent_tool_output(&lifecycle, row["_docID"].as_str().unwrap()).await;
            assert!(
                !output.is_empty(),
                "canonical output {index} must be delivered"
            );
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn repeated_identical_bash_failure_stops_the_loop() {
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let root = tempfile::tempdir().unwrap();
    let mut turns = (0..6)
        .map(|index| {
            vec![
                RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    format!("bash-repeat-{index}"),
                    "bash".into(),
                    serde_json::json!({"command":"ls","args":["missing-dir"]}),
                )),
                RawStreamingChoice::FinalResponse(()),
            ]
        })
        .collect::<Vec<_>>();
    turns.push(vec![
        RawStreamingChoice::Message("unreached".into()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let model = ScriptedModel::new_turns(turns);
    let tools = gents_tools::native::ToolSet::builder()
        .read_root(root.path())
        .bash_read_only()
        .build()
        .build_native_tools()
        .unwrap();
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("list the missing directory")),
        Vec::new(),
        Arc::new(tools),
        owned_config(64),
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    let error = collected
        .error
        .expect("the repeated bash failure must stop");
    assert!(error.contains(REPEATED_TOOL_FAILURE_PREFIX), "{error}");
    assert_eq!(collected.tool_results.len(), 4);
    assert!(collected.tool_results[..3]
        .iter()
        .all(|result| result.contains("duration_ms")));
    assert!(collected.tool_results[3].contains("repeated_tool_call_not_run"));
    assert_eq!(model.seen_requests().await.len(), 5);
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn envelope_shaped_errors_from_other_tools_keep_their_hint() {
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let mut turns = (0..5)
        .map(|index| {
            vec![
                RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    format!("shaped-{index}"),
                    "envelope_shaped".into(),
                    serde_json::json!({}),
                )),
                RawStreamingChoice::FinalResponse(()),
            ]
        })
        .collect::<Vec<_>>();
    turns.push(vec![
        RawStreamingChoice::Message("done".into()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let model = ScriptedModel::new_turns(turns);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("call the tool")),
        Vec::new(),
        Arc::new(vec![Box::new(EnvelopeShapedTool {
            calls: Arc::clone(&calls),
        }) as Box<dyn ToolDyn>]),
        owned_config(64),
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
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 5);
    assert!(collected
        .tool_results
        .iter()
        .all(|result| !result.contains("repeated_tool_call_not_run")));
    node.shutdown().await.expect("node shutdown");
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

#[tokio::test]
async fn unavailable_goal_attribution_stops_the_provider_before_another_turn() {
    let (_db, hook, writer, mut lifecycle) = owned_test_hook_with_unattributable_request().await;
    let hook = hook
        .with_goal_tool_authority(true, true)
        .with_goal_tools(std::sync::Arc::new(
            gents_automation::goal::goal_tools::GoalToolAnswers,
        ));
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "goal-call".into(),
                gents_protocol::goal_names::GET_GOAL_TOOL_NAME.into(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("repeat the failed status".into()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("check the Goal")),
        Vec::new(),
        Arc::new(vec![Box::new(FixedTool {
            name: gents_protocol::goal_names::GET_GOAL_TOOL_NAME.into(),
            output: "must be intercepted".into(),
        }) as Box<dyn ToolDyn>]),
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
    let error = collected
        .error
        .expect("unavailable attribution must terminate");
    assert!(error.contains("PromptCancelled"), "{error}");
    assert!(error.contains("attribution"), "{error}");
    assert_eq!(model.seen_histories().await.len(), 1);
}

#[tokio::test]
async fn plugin_image_parts_reach_the_provider_whole() {
    const IMAGE_BYTES: usize = 900_000;
    let result = plugin_image_result_seen_by(
        gents_inference::backends::provider::BackendProviderKind::AnthropicApiKey,
        IMAGE_BYTES,
    )
    .await;
    assert_eq!(result.len(), 2, "one text part and one image part");
    assert_eq!(tool_result_text(&result[0]), r#"{"height":2,"width":2}"#);
    let ToolResultContent::Image(image) = &result[1] else {
        panic!("the second part is the image: {:?}", result[1]);
    };
    assert_eq!(
        image.media_type,
        Some(gents_inference::llm::message::ImageMediaType::PNG)
    );
    let gents_inference::llm::message::DocumentSourceKind::Base64(data) = &image.data else {
        panic!("the image arrives as base64 data");
    };
    assert_eq!(
        data.len(),
        IMAGE_BYTES,
        "the image is not cut by the text bound"
    );
}

#[tokio::test]
async fn persisted_plugin_image_result_replays_text_parts_and_a_note() {
    for image_bytes in [64, 400_000] {
        persisted_plugin_image_result_replay(image_bytes).await;
    }
}

#[tokio::test]
async fn plugin_image_parts_become_a_note_where_the_provider_refuses_them() {
    let result = plugin_image_result_seen_by(
        gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible,
        64,
    )
    .await;
    assert_eq!(
        result.iter().map(tool_result_text).collect::<Vec<_>>(),
        vec![
            r#"{"height":2,"width":2}"#,
            gents_loop::loop_stream::TOOL_RESULT_IMAGE_OMITTED,
        ]
    );
}

#[tokio::test]
async fn threaded_assistant_turn_carries_provider_message_id() {
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::MessageId("msg_abc123".to_string()),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);

    let stream = run_loop_stream(
        model.clone(),
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("go")),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        2,
        "expected two completion turns; got {histories:?}"
    );
    let assistant_id = histories[1].iter().find_map(|message| match message {
        Message::Assistant { id, .. } => Some(id.clone()),
        _ => None,
    });
    assert_eq!(
        assistant_id,
        Some(Some("msg_abc123".to_string())),
        "threaded assistant turn must carry the provider message id; history: {:?}",
        histories[1]
    );
}

#[tokio::test]
async fn spawn_process_rejects_unadvertised_arguments_before_spawned_admission() {
    for args in [
        r#"{"args":[],"command":"ls","timeout_secs":"25","tool_name":"bash"}"#,
        r#"{"tool_name":"bash","args":{},"command":"ls"}"#,
        r#"{"tool_name":"bash"}"#,
        r#"{"tool_name":"bash","args":"{\"command\":\"ls\"}"}"#,
    ] {
        let root = tempfile::tempdir().unwrap();
        let registry = gents_toolcalls::call::hook::BackgroundToolRegistry::from_tools(
            vec![gents_tools::native::read_only_bash_for_test(
                root.path(),
                vec!["ls".into()],
            )],
            &["bash".into()],
        );
        let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
            "background-argument-wire",
            registry,
            Default::default(),
            gents_automation::lifecycle_hooks::composition(),
        )
        .await;
        let node = fixture.db.node.clone();
        let access = fixture.access;
        let request_id = fixture.request_id;
        let hook = fixture.hook;
        hook.on_completion_call(
            &gents_toolcalls::call::hook::fixtures::user_text_message("run in the background"),
            &[],
        )
        .await;
        accept_hook_tool_call(
            &hook,
            "spawn-malformed",
            gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME,
            args,
            None,
        )
        .await;
        let action = hook
            .on_tool_call(
                gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME,
                None,
                "spawn-malformed",
                args,
            )
            .await;
        let ToolCallHookAction::Skip { reason } = &action else {
            panic!("{args}: {action:?}");
        };
        assert!(
            reason.contains("invalid spawn_process arguments"),
            "{args}: {reason}"
        );

        let response = access
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ request_id: {{_eq: "{}"}} }}) {{ tool_name lifecycle_state tool_failure_class spawned_by_tool_call_doc_id }} }}"#,
                gents_store::graphql::escape_graphql_string(&request_id),
            ))
            .await.unwrap();
        let data = &response["data"];
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{args}: {rows:?}");
        assert_eq!(
            rows[0]["tool_name"],
            gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME
        );
        assert!(rows[0]["spawned_by_tool_call_doc_id"].is_null(), "{rows:?}");
        assert_eq!(rows[0]["lifecycle_state"], "failed", "{args}: {rows:?}");
        assert_eq!(
            rows[0]["tool_failure_class"], "argumentInvalid",
            "{args}: {rows:?}"
        );
        hook_execution_fixtures()
            .lock()
            .await
            .remove(&hook_execution_fixture_key(&hook, &request_id));
        node.shutdown().await.expect("shut down test node");
    }
}
