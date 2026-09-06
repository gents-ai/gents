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
        let (node, hook) = test_hook().await;
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
            Some(hook),
            Message::user("exercise typed outcomes"),
            Vec::new(),
            Arc::new(vec![Box::new(InvalidProgressProbe) as Box<dyn ToolDyn>]),
            config(64),
        );
        futures::pin_mut!(stream);
        let mut error = None;
        let mut yielded_results = 0;
        while let Some(item) = stream.next().await {
            match item {
                Err(value) => {
                    error = Some(value.to_string());
                    break;
                }
                Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
                    StreamedUserContent::ToolResult { .. },
                ))) => yielded_results += 1,
                _ => {}
            }
        }
        let exhausted = case["expected_exhausted"].as_bool().unwrap();
        assert_eq!(error.is_some(), exhausted, "{}: {error:?}", case["name"]);
        if let Some(error) = error {
            assert!(
                error.contains("invalid_tool_call_budget_exhausted:"),
                "{error}"
            );
        }
        let observed = case["expected_observed_outcomes"].as_u64().unwrap() as usize;
        assert_eq!(
            yielded_results, observed,
            "{} must emit the last result before failing",
            case["name"]
        );
        assert_eq!(
            model.seen_requests().await.len(),
            observed + usize::from(!exhausted),
            "{} must not dispatch a suffix",
            case["name"]
        );
        let response = node
            .execute("{ AgentToolCall { lifecycle_state tool_failure_class result } }")
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let data = response.data.unwrap();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), observed, "{} durable outcomes", case["name"]);
        assert!(rows.iter().all(|row| matches!(
            row["lifecycle_state"].as_str(),
            Some("completed" | "failed")
        )));
        let charged = rows
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
            case["expected_invalid_used"].as_u64().unwrap(),
            "{} typed outcome mapping",
            case["name"]
        );
        assert!(rows
            .iter()
            .all(|row| row["result"].as_str().is_some_and(|text| !text.is_empty())));
        node.shutdown().await;
    }
}

#[tokio::test]
async fn invalid_tool_budget_closes_eighth_result_before_stalling_or_batched_ninth() {
    for batched in [false, true] {
        let (node, hook) = test_hook().await;
        let mut chunks = (0..8)
            .map(|index| invalid_progress_call(index, "policyDenied"))
            .collect::<Vec<_>>();
        if batched {
            chunks.push(invalid_progress_call(8, "success"));
        }
        // No final usage/EOF: exhaustion must not wait for the provider to close.
        let model = ScriptedModel::new_stalling(chunks);
        let stream = run_loop_stream(
            model.clone(),
            Some(hook),
            Message::user("bound invalid batch"),
            Vec::new(),
            Arc::new(vec![Box::new(InvalidProgressProbe) as Box<dyn ToolDyn>]),
            config(500),
        );
        futures::pin_mut!(stream);
        let (count, error) = tokio::time::timeout(Duration::from_secs(10), async {
            let mut count = 0;
            while let Some(item) = stream.next().await {
                match item {
                    Err(error) => return (count, error.to_string()),
                    Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
                        StreamedUserContent::ToolResult { .. },
                    ))) => count += 1,
                    _ => {}
                }
            }
            panic!("invalid batch unexpectedly completed")
        })
        .await
        .expect("must terminate without waiting for provider EOF");
        assert_eq!(count, 8);
        assert!(
            error.contains("invalid_tool_call_budget_exhausted:"),
            "{error}"
        );
        assert_eq!(model.seen_requests().await.len(), 1);
        let response = node
            .execute("{ AgentToolCall { lifecycle_state result } }")
            .await;
        assert!(!response.has_errors());
        let data = response.data.unwrap();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            8,
            "no ninth effect, even within a provider batch"
        );
        assert!(rows.iter().all(|row| row["lifecycle_state"] == "failed"
            && row["result"].as_str().is_some_and(|s| !s.is_empty())));
        node.shutdown().await;
    }
}

#[tokio::test]
async fn malformed_bash_feedback_reaches_next_request_and_corrected_argv_succeeds() {
    let (node, hook) = test_hook().await;
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
        Some(hook),
        Message::user("inspect crates"),
        Vec::new(),
        Arc::new(tools),
        config(10),
    );
    futures::pin_mut!(stream);
    while let Some(item) = stream.next().await {
        item.expect("corrected arguments must allow completion");
    }
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
        .execute("{ AgentToolCall { tool_call_id lifecycle_state tool_failure_class result } }")
        .await;
    assert!(!response.has_errors());
    let data = response.data.unwrap();
    let rows = data["AgentToolCall"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|row| row["lifecycle_state"] == "failed"
            && row["tool_failure_class"] == "argumentInvalid"));
    assert!(rows.iter().any(|row| row["lifecycle_state"] == "completed"
        && row["result"]
            .as_str()
            .unwrap()
            .contains("visible-proof.txt")));
    node.shutdown().await;
}

#[tokio::test]
async fn empty_bash_arguments_exhaust_owned_loop_without_side_effects() {
    for arguments in [serde_json::json!({}), serde_json::json!("")] {
        let (node, hook) = test_hook().await;
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
            Some(hook),
            Message::user("inspect source"),
            Vec::new(),
            Arc::new(tools),
            config(500),
        );
        futures::pin_mut!(stream);
        let mut error = None;
        while let Some(item) = stream.next().await {
            if let Err(value) = item {
                error = Some(value.to_string());
                break;
            }
        }
        assert!(error
            .unwrap()
            .contains("invalid_tool_call_budget_exhausted:"));
        assert_eq!(model.seen_requests().await.len(), 8);
        let response = node
            .execute("{ AgentToolCall { lifecycle_state tool_failure_class result } }")
            .await;
        assert!(!response.has_errors());
        let data = response.data.unwrap();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().all(|row| row["lifecycle_state"] == "failed"
            && row["tool_failure_class"] == "argumentInvalid"
            && row["result"].as_str().is_some_and(|s| !s.is_empty())));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        node.shutdown().await;
    }
}
