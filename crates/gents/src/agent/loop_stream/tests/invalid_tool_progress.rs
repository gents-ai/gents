struct InvalidProgressProbe;

impl ToolDyn for InvalidProgressProbe {
    fn name(&self) -> String {
        "invalid_progress_probe".into()
    }
    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "invalid_progress_probe".into(),
                description: "typed outcome fixture".into(),
                parameters: serde_json::json!({"type":"object","properties":{"outcome":{"type":"string"}},"required":["outcome"]}),
            }
        })
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            use crate::tool_call_lifecycle::FailureClass;
            let value: serde_json::Value = serde_json::from_str(&args).unwrap();
            let class = match value["outcome"].as_str().unwrap() {
                "invalidArguments" => FailureClass::ArgumentInvalid,
                "policyDenied" => FailureClass::PolicyDenied,
                "ordinaryFailure" => FailureClass::ToolReturnedError,
                // Successful arbitrary output must never impersonate typed failure.
                "success" => return Ok(r#"{"failure_class":"policyDenied","ok":false}"#.into()),
                other => panic!("unknown fixture outcome {other}"),
            };
            Err(ToolError::ReportedFailure {
                class,
                text: format!("fixture {}", class.as_str()),
            })
        })
    }
}

fn invalid_progress_call(index: usize, outcome: &str) -> RawStreamingChoice<()> {
    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
        format!("invalid-progress-{index}"),
        if outcome == "unknownTool" {
            "missing"
        } else {
            "invalid_progress_probe"
        }
        .into(),
        serde_json::json!({"outcome":outcome}),
    ))
}

#[tokio::test]
async fn generated_invalid_tool_progress_cases_drive_owned_loop() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().invalid_tool_progress_cases;
    assert_eq!(cases.len(), 11);
    for case in cases {
        let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
        let outcomes = case["outcomes"].as_array().unwrap();
        let mut turns = outcomes
            .iter()
            .enumerate()
            .map(|(index, outcome)| {
                vec![
                    invalid_progress_call(index, outcome.as_str().unwrap()),
                    RawStreamingChoice::FinalResponse(()),
                ]
            })
            .collect::<Vec<_>>();
        turns.push(vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let model = ScriptedModel::new_turns(turns);
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            Message::user("exercise typed outcomes"),
            Vec::new(),
            Arc::new(vec![Box::new(InvalidProgressProbe) as Box<dyn ToolDyn>]),
            owned_config(64),
        );
        let collected = collect_owned_scripted_stream(stream, &hook, &writer, &mut lifecycle).await;
        let name = case["name"].as_str().unwrap();
        let actions = case["composed_actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action.as_str().unwrap())
            .collect::<Vec<_>>();
        let ending = case["composed_ending"].as_str();
        let error = collected.error.as_deref();
        assert_eq!(error.is_some(), ending.is_some(), "{name}: {error:?}");
        match (ending, error) {
            (Some("invalidExhausted"), Some(error)) => {
                assert!(error.contains("invalid_tool_call_budget_exhausted:"), "{error}")
            }
            (Some("repeatedFailure"), Some(error)) => {
                assert!(error.contains(REPEATED_TOOL_FAILURE_PREFIX), "{error}")
            }
            (None, None) => {}
            other => panic!("{name}: unexpected ending {other:?}"),
        }
        let answered = actions.iter().filter(|action| **action != "stop").count();
        assert_eq!(
            collected.tool_results.len(),
            answered,
            "{name} must emit the last result before failing"
        );
        assert_eq!(
            model.seen_requests().await.len(),
            actions.len() + usize::from(ending.is_none()),
            "{name} must not dispatch a suffix"
        );
        let rows = tool_call_rows(&node).await;
        let answered_rows = (0..answered)
            .map(|index| {
                rows.iter()
                    .find(|row| row["tool_call_id"] == format!("invalid-progress-{index}"))
                    .unwrap_or_else(|| panic!("{name}: missing durable row {index}"))
            })
            .collect::<Vec<_>>();
        assert!(answered_rows.iter().all(|row| matches!(
            row["lifecycle_state"].as_str(),
            Some("completed" | "failed")
        )));
        let charged = answered_rows
            .iter()
            .filter(|row| {
                matches!(
                    row["tool_failure_class"].as_str(),
                    Some("argumentInvalid" | "policyDenied")
                )
            })
            .count();
        assert_eq!(
            charged as u64,
            case["composed_invalid_used"].as_u64().unwrap(),
            "{name} typed outcome mapping"
        );
        for (index, row) in answered_rows.iter().enumerate() {
            let output = crate::background_tools::canonical_tool_output(
                &node,
                row["_docID"].as_str().unwrap(),
                &lifecycle.request().doc_id,
                &lifecycle.request().session_id,
                "did:test:test",
                None,
            )
            .await
            .unwrap();
            assert!(
                !output.is_empty(),
                "{name}: canonical output for {index} must not be empty"
            );
        }
        node.shutdown().await;
    }
}

#[tokio::test]
async fn invalid_tool_budget_closes_eighth_result_and_cancels_accepted_ninth() {
    for batched in [false, true] {
        let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
        let mut chunks = (0..8)
            .map(|index| invalid_progress_call(index, "policyDenied"))
            .collect::<Vec<_>>();
        if batched {
            chunks.push(invalid_progress_call(8, "success"));
        }
        // Canonical publication accepts the whole provider turn before any
        // dispatch. Budget exhaustion must retain the eighth result and account
        // for an accepted ninth call without executing it.
        chunks.push(RawStreamingChoice::FinalResponse(()));
        let model = ScriptedModel::new(chunks);
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            Message::user("bound invalid batch"),
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
                crate::lifecycle::RequestTerminalOutcome::Failed,
                writer.terminal_output(&lifecycle.request().doc_id).await,
                Some(&error),
            )
            .await
            .expect("terminal owner settles every published undispatched call");
        let response = node
            .execute("{ AgentToolCall { _docID tool_call_id lifecycle_state } }")
            .await;
        assert!(!response.has_errors());
        let data = response.data.unwrap();
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
            let output = crate::background_tools::canonical_tool_output(
                &node,
                row["_docID"].as_str().unwrap(),
                &lifecycle.request().doc_id,
                &lifecycle.request().session_id,
                "did:test:test",
                None,
            )
            .await
            .unwrap();
            assert!(
                !output.is_empty(),
                "canonical output {index} must be delivered"
            );
        }
        node.shutdown().await;
    }
}

#[tokio::test]
async fn malformed_bash_feedback_reaches_next_request_and_corrected_argv_succeeds() {
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
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
    let tools = crate::toolset::ToolSet::builder()
        .read_root(root.path())
        .bash_read_only()
        .build()
        .build_native_tools()
        .unwrap();
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        Message::user("inspect crates"),
        Vec::new(),
        Arc::new(tools),
        owned_config(10),
    );
    let collected = collect_owned_scripted_stream(stream, &hook, &writer, &mut lifecycle).await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    let requests = model.seen_requests().await;
    assert_eq!(requests.len(), 3);
    let feedback = serde_json::to_string(
        &requests[1]
            .chat_history
            .iter()
            .map(rig_compat::from_rig_message)
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
            .map(rig_compat::from_rig_message)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(
        corrected.contains("visible-proof.txt"),
        "successful corrected result must reach provider"
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
    let response = node
        .execute("{ AgentToolCall { _docID tool_call_id lifecycle_state tool_failure_class } }")
        .await;
    assert!(!response.has_errors());
    let data = response.data.unwrap();
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
    let output = crate::background_tools::canonical_tool_output(
        &node,
        tool_doc_id,
        &lifecycle.request().doc_id,
        &lifecycle.request().session_id,
        "did:test:test",
        None,
    )
    .await
    .unwrap();
    assert!(
        output.contains("visible-proof.txt"),
        "canonical output must carry the successful bash listing: {output}"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn empty_bash_arguments_exhaust_owned_loop_without_side_effects() {
    for arguments in [serde_json::json!({}), serde_json::json!("")] {
        let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
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
        let tools = crate::toolset::ToolSet::builder()
            .read_root(root.path())
            .bash_read_only()
            .build()
            .build_native_tools()
            .unwrap();
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            Message::user("inspect source"),
            Vec::new(),
            Arc::new(tools),
            owned_config(500),
        );
        let collected = collect_owned_scripted_stream(stream, &hook, &writer, &mut lifecycle).await;
        let error = collected
            .error
            .expect("budget exhaustion must fail the loop");
        assert!(error.contains("invalid_tool_call_budget_exhausted:"));
        assert_eq!(model.seen_requests().await.len(), 8);
        let response = node
            .execute("{ AgentToolCall { _docID tool_call_id lifecycle_state tool_failure_class } }")
            .await;
        assert!(!response.has_errors());
        let data = response.data.unwrap();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().all(|row| row["lifecycle_state"] == "failed"
            && row["tool_failure_class"] == "argumentInvalid"));
        for index in 0..8 {
            let row = rows
                .iter()
                .find(|row| row["tool_call_id"] == format!("empty-bash-{index}"))
                .unwrap_or_else(|| panic!("missing durable row {index}"));
            let output = crate::background_tools::canonical_tool_output(
                &node,
                row["_docID"].as_str().unwrap(),
                &lifecycle.request().doc_id,
                &lifecycle.request().session_id,
                "did:test:test",
                None,
            )
            .await
            .unwrap();
            assert!(
                !output.is_empty(),
                "canonical output {index} must be delivered"
            );
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        node.shutdown().await;
    }
}
