/// Returns the modeled outcome of each dispatched call, in dispatch order. A
/// dispatch the model decided to suppress, skip or stop finds the wrong event
/// or an empty script and fails the test.
struct RepeatProbe {
    script: Arc<std::sync::Mutex<std::collections::VecDeque<serde_json::Value>>>,
}

impl ToolDyn for RepeatProbe {
    fn name(&self) -> String {
        "repeat_probe".into()
    }
    /// Registered as a command-envelope owner so a malformed envelope has no
    /// error identity.
    fn emits_command_envelope(&self) -> bool {
        true
    }
    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "repeat_probe".into(),
                description: "scripted repeated-failure fixture".into(),
                parameters: serde_json::json!({"type":"object","properties":{"call":{"type":"integer"}},"required":["call"]}),
            }
        })
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            use crate::tool_call_lifecycle::FailureClass;
            let args: serde_json::Value = serde_json::from_str(&args).unwrap();
            let event = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("dispatched undispatchable call {args}"));
            assert_eq!(event["call"], args["call"], "dispatched out of model order");
            let (class, text) = match (event["outcome"].as_str().unwrap(), &event["error"]) {
                ("success", _) => return Ok("ok".into()),
                ("ordinaryFailure", serde_json::Value::Null) => (
                    FailureClass::ToolReturnedError,
                    "gents_exec: {unparseable\nfixture error".to_string(),
                ),
                ("ordinaryFailure", error) => {
                    (FailureClass::ToolReturnedError, format!("fixture error {error}"))
                }
                ("invalidArguments", _) => {
                    (FailureClass::ArgumentInvalid, "fixture invalid".to_string())
                }
                ("policyDenied", _) => (FailureClass::PolicyDenied, "fixture denied".to_string()),
                (other, _) => panic!("fixture outcome {other} is not dispatched to the probe"),
            };
            Err(ToolError::ReportedFailure { class, text })
        })
    }
}

async fn tool_call_rows(node: &Arc<defra_node::EmbeddedNode>) -> Vec<serde_json::Value> {
    let response = crate::config_client::ConfigAccess::Local(node.clone())
        .execute("query { AgentToolCall { _docID tool_call_id lifecycle_state tool_failure_class } }")
        .await
        .unwrap();
    response["data"]["AgentToolCall"].as_array().unwrap().clone()
}

fn repeat_call(index: usize, event: &serde_json::Value) -> RawStreamingChoice<()> {
    let (tool, args) = match event["outcome"].as_str().unwrap() {
        "skipped" => ("list_processes", serde_json::json!({})),
        "unknownTool" => ("missing", serde_json::json!({"call": event["call"]})),
        _ => ("repeat_probe", serde_json::json!({"call": event["call"]})),
    };
    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
        format!("repeat-{index}"),
        tool.into(),
        args,
    ))
}

#[tokio::test]
async fn generated_repeated_tool_failure_cases_drive_owned_loop() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().repeated_tool_failure_cases;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let events = case["events"].as_array().unwrap();
        let actions = case["expected_actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action.as_str().unwrap())
            .collect::<Vec<_>>();
        let ending = case["expected_ending"].as_str();
        let observed = &events[..actions.len()];
        let script = Arc::new(std::sync::Mutex::new(
            observed
                .iter()
                .zip(&actions)
                .filter(|(event, action)| **action == "dispatch" && event["outcome"] != "unknownTool")
                .map(|(event, _)| event.clone())
                .collect::<std::collections::VecDeque<_>>(),
        ));
        let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
        let mut turns = observed
            .iter()
            .enumerate()
            .map(|(index, event)| vec![repeat_call(index, event), RawStreamingChoice::FinalResponse(())])
            .collect::<Vec<_>>();
        if ending.is_none() {
            turns.push(vec![
                RawStreamingChoice::Message("done".into()),
                RawStreamingChoice::FinalResponse(()),
            ]);
        }
        let model = ScriptedModel::new_turns(turns);
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            Message::user("exercise repeated failures"),
            Vec::new(),
            Arc::new(vec![Box::new(RepeatProbe {
                script: Arc::clone(&script),
            }) as Box<dyn ToolDyn>]),
            owned_config(64),
        );
        let collected = collect_owned_scripted_stream(stream, &hook, &writer, &mut lifecycle).await;
        assert!(
            script.lock().unwrap().is_empty(),
            "{name}: every modeled dispatch must run"
        );
        let error = collected.error.as_deref();
        assert_eq!(error.is_some(), ending.is_some(), "{name}: {error:?}");
        match (ending, error) {
            (Some("repeatedFailure"), Some(error)) => {
                assert!(error.contains(REPEATED_TOOL_FAILURE_PREFIX), "{error}");
                assert!(error.contains("fixture error"), "{name}: {error}");
            }
            (Some("invalidExhausted"), Some(error)) => {
                assert!(error.contains("invalid_tool_call_budget_exhausted:"), "{error}")
            }
            (None, None) => {}
            other => panic!("{name}: unexpected ending {other:?}"),
        }
        let answered = actions.iter().filter(|action| **action != "stop").count();
        assert_eq!(collected.tool_results.len(), answered, "{name}: results");
        for (result, action) in collected.tool_results.iter().zip(&actions) {
            assert_eq!(
                result.contains("repeated_tool_call_not_run"),
                *action == "suppress",
                "{name}: {result}"
            );
        }
        assert_eq!(
            model.seen_requests().await.len(),
            actions.len() + usize::from(ending.is_none()),
            "{name}: provider requests"
        );
        let rows = tool_call_rows(&node).await;
        let mut charged = 0;
        for (index, action) in actions.iter().enumerate() {
            if *action == "stop" {
                continue;
            }
            let row = rows
                .iter()
                .find(|row| row["tool_call_id"] == format!("repeat-{index}"))
                .unwrap_or_else(|| panic!("{name}: missing durable row {index}"));
            if matches!(
                row["tool_failure_class"].as_str(),
                Some("argumentInvalid" | "policyDenied")
            ) {
                charged += 1;
            }
            if *action == "suppress" {
                assert_eq!(row["lifecycle_state"], "failed", "{name}: {row}");
                assert_eq!(row["tool_failure_class"], "policyDenied", "{name}: {row}");
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
                    output.contains("repeated_tool_call_not_run"),
                    "{name}: durable notice {output}"
                );
            }
        }
        assert_eq!(
            charged,
            case["expected_invalid_used"].as_u64().unwrap(),
            "{name}: invalid allowance"
        );
        if ending == Some("repeatedFailure") {
            lifecycle
                .terminalize_owned(
                    crate::lifecycle::RequestTerminalOutcome::Failed,
                    writer.terminal_output(&lifecycle.request().doc_id).await,
                    error,
                )
                .await
                .expect("terminal owner settles the accepted, undispatched repeat");
            let stop = tool_call_rows(&node)
                .await
                .into_iter()
                .find(|row| row["tool_call_id"] == format!("repeat-{}", actions.len() - 1))
                .unwrap_or_else(|| panic!("{name}: accepted stop call is durable"));
            assert_eq!(stop["lifecycle_state"], "cancelled", "{name}: {stop}");
        }
        node.shutdown().await;
    }
}

#[tokio::test]
async fn repeated_identical_bash_failure_stops_the_loop() {
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
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
    let tools = crate::toolset::ToolSet::builder()
        .read_root(root.path())
        .bash_read_only()
        .build()
        .build_native_tools()
        .unwrap();
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        Message::user("list the missing directory"),
        Vec::new(),
        Arc::new(tools),
        owned_config(64),
    );
    let collected = collect_owned_scripted_stream(stream, &hook, &writer, &mut lifecycle).await;
    let error = collected.error.expect("the repeated bash failure must stop");
    assert!(error.contains(REPEATED_TOOL_FAILURE_PREFIX), "{error}");
    assert_eq!(collected.tool_results.len(), 4);
    assert!(collected.tool_results[..3]
        .iter()
        .all(|result| result.contains("duration_ms")));
    assert!(collected.tool_results[3].contains("repeated_tool_call_not_run"));
    assert_eq!(model.seen_requests().await.len(), 5);
    node.shutdown().await;
}

/// Not a command runner, but returns text shaped like the command envelope.
struct EnvelopeShapedTool {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl ToolDyn for EnvelopeShapedTool {
    fn name(&self) -> String {
        "envelope_shaped".into()
    }
    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "envelope_shaped".into(),
                description: "non-command tool with envelope-shaped errors".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }
    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(ToolError::ReportedFailure {
                class: crate::tool_call_lifecycle::FailureClass::ToolReturnedError,
                text: format!(
                    "gents_exec: {{\"ok\":false,\"duration_ms\":1,\"hint\":\"step {call}\"}}\nstdout:\n(empty)\nstderr:\nfailed"
                ),
            })
        })
    }
}

#[tokio::test]
async fn envelope_shaped_errors_from_other_tools_keep_their_hint() {
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
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
        Message::user("call the tool"),
        Vec::new(),
        Arc::new(vec![Box::new(EnvelopeShapedTool {
            calls: Arc::clone(&calls),
        }) as Box<dyn ToolDyn>]),
        owned_config(64),
    );
    let collected = collect_owned_scripted_stream(stream, &hook, &writer, &mut lifecycle).await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 5);
    assert!(collected
        .tool_results
        .iter()
        .all(|result| !result.contains("repeated_tool_call_not_run")));
    node.shutdown().await;
}
