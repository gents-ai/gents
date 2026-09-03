#[tokio::test]
async fn tool_call_turn_executes_threads_result_and_completes() {
    let (node, hook) = test_hook().await;
    let prompt = Message::user("use the echo tool");

    // Turn 1: the model calls `echo`. Turn 2: it answers with text.
    let model = ScriptedModel::new_turns(vec![
        vec![
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
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })];

    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    let mut tool_results = Vec::new();
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        match item.expect("loop item should be Ok") {
            LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
                StreamedUserContent::ToolResult { tool_result, .. },
            )) => {
                tool_results.push(
                    tool_result_text(&crate::llm::rig_compat::from_rig_tool_result_content(
                        &tool_result.content.first(),
                    ))
                    .to_string(),
                );
            }
            LoopStreamItem::Item(MultiTurnStreamItem::FinalResponse(final_response)) => {
                final_text = Some(final_response.response().to_string());
            }
            _ => {}
        }
    }

    // The tool ran, its (bounded) result was threaded/yielded, and the loop
    // reached a text response on the next turn.
    assert_eq!(tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(final_text.as_deref(), Some("done"));

    // The generator drove the tool-call lifecycle directly: on_tool_call started
    // it and on_tool_result completed it with the result. (The tool-result
    // *message* persistence is split with StreamProcessor — exercised once the
    // generator is wired into the consumer in step 3 — so it is not asserted
    // here against the standalone generator.)
    let resp = node
        .execute("query { AgentToolCall { tool_name lifecycle_state result } }")
        .await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall query failed: {:?}",
        resp.errors
    );
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|value| value.as_str()) == Some("echo")
                && row.get("lifecycle_state").and_then(|value| value.as_str()) == Some("completed")
                && row
                    .get("result")
                    .and_then(|value| value.as_str())
                    .is_some_and(|result| result.contains("ECHOED"))
        }),
        "expected a completed echo tool call recording the result; rows: {rows:?}"
    );
}

#[tokio::test]
async fn tool_executes_before_provider_stalls_mid_stream() {
    // P2 regression: a provider that emits a tool call then stalls before EOF
    // must still have its tool executed. Rig runs each tool inline as its
    // ToolCall arrives, so the lifecycle / AgentToolCall row exists before the
    // stall; the daemon liveness timeout then has something to cancel. The old
    // design collected tool calls and dispatched only after the stream drained,
    // so a mid-stream stall left the tool unrun with nothing to mark.
    let (node, hook) = test_hook().await;
    let prompt = Message::user("use the echo tool then stall");

    // One turn: emit a tool call, then hang (no FinalResponse, no EOF).
    let model = ScriptedModel::new_stalling(vec![RawStreamingChoice::ToolCall(
        RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        ),
    )]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })];

    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    // Item 1 is the tool call. Resuming the stream then runs the tool inline and
    // afterwards blocks forever on the stalled provider — so the second poll
    // never returns, but the tool executes before that block. Bound it.
    let first = stream.next().await.expect("should yield the tool call");
    assert!(
        matches!(
            first,
            Ok(LoopStreamItem::Item(
                MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall { .. })
            ))
        ),
        "first item should be the tool call; got {first:?}"
    );
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(3), stream.next()).await;

    // Despite the stall, the tool ran to completion (its row exists, recorded).
    let resp = node
        .execute("query { AgentToolCall { tool_name lifecycle_state result } }")
        .await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall query failed: {:?}",
        resp.errors
    );
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|value| value.as_str()) == Some("echo")
                && row
                    .get("result")
                    .and_then(|value| value.as_str())
                    .is_some_and(|result| result.contains("ECHOED"))
        }),
        "tool must execute inline before the provider stall; rows: {rows:?}"
    );
}

#[tokio::test]
async fn tool_definition_receives_prompt_rag_text() {
    // P3/compat: tool definitions must be built with the prompt's rag text (rig
    // parity), not String::new(), so prompt-aware tools keep the task context.
    let (_node, hook) = test_hook().await;
    let seen = Arc::new(Mutex::new(None));
    let tool: Box<dyn ToolDyn> = Box::new(RecordingDefinitionTool {
        seen_prompt: seen.clone(),
    });

    // A single text-only turn; the tool is never called, but its definition is
    // still requested when the request is built.
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("hi".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let stream = run_loop_stream(
        model,
        Some(hook),
        Message::user("teach me rust"),
        Vec::new(),
        Arc::new(vec![tool]),
        config(1),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    assert_eq!(
        seen.lock().await.as_deref(),
        Some("teach me rust"),
        "tool definition should receive the prompt's rag text, not an empty string"
    );
}

#[tokio::test]
async fn toolset_is_attached_to_every_completion_request_in_the_loop() {
    // Regression for the CLI tool-loop test: rig's Agent re-sent the full tool
    // list on every turn; the owned loop must too. The follow-up request after a
    // tool result is folded in (turn 2) must still advertise the toolset, or the
    // provider sees a tool-result conversation with no tools.
    let (_node, hook) = test_hook().await;

    // Turn 1: the model calls `echo`. Turn 2: it answers with text.
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let seen_tools = model.seen_tools().await;
    assert_eq!(
        seen_tools.len(),
        2,
        "expected two completion turns; got {seen_tools:?}"
    );
    for (turn, tools) in seen_tools.iter().enumerate() {
        assert!(
            tools.contains(&"echo".to_string()),
            "completion request for turn {} must advertise the toolset; got {seen_tools:?}",
            turn + 1
        );
    }
}

#[tokio::test]
async fn oversized_tool_result_is_bounded_before_threading() {
    let (_node, hook) = test_hook().await;
    let prompt = Message::user("read the big thing");

    // A tool returning far more than the default limits: the model-facing
    // (threaded/yielded) result must be bounded, while on_tool_result still
    // receives the full output for spill (#401 closed natively).
    let big_line = "x".repeat(200);
    let big_output = std::iter::repeat(big_line)
        .take(10_000)
        .collect::<Vec<_>>()
        .join("\n");
    let full_len = big_output.len();
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(FixedTool {
        name: "echo".to_string(),
        output: big_output,
    })];
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    let mut bounded_len = None;
    while let Some(item) = stream.next().await {
        if let LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
            StreamedUserContent::ToolResult { tool_result, .. },
        )) = item.expect("loop item should be Ok")
        {
            bounded_len = Some(
                tool_result_text(&crate::llm::rig_compat::from_rig_tool_result_content(
                    &tool_result.content.first(),
                ))
                .len(),
            );
        }
    }

    let bounded_len = bounded_len.expect("a tool result should have been yielded");
    assert!(
        bounded_len < full_len,
        "expected the threaded result to be bounded: bounded={bounded_len} full={full_len}"
    );
    assert!(bounded_len > 0, "bounded result should be non-empty");
}

#[test]
fn value_to_json_string_passes_strings_through_unquoted() {
    assert_eq!(
        value_to_json_string(&serde_json::json!("plain")),
        "plain".to_string()
    );
    assert_eq!(
        value_to_json_string(&serde_json::json!({"path": "x"})),
        r#"{"path":"x"}"#.to_string()
    );
}

#[test]
fn deadline_remaining_is_zero_when_past() {
    let past = chrono::Utc::now() - chrono::Duration::seconds(5);
    assert_eq!(
        super::deadline_remaining(Some(past)),
        Some(std::time::Duration::ZERO)
    );
    assert_eq!(super::deadline_remaining(None), None);
}

#[tokio::test]
async fn dispatch_tool_calls_known_tool_and_reports_unknown() {
    // No tool runtime scope is active in this unit test, so dispatch_tool takes
    // the unscoped path: look up by name and call directly.
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })];

    assert_eq!(
        super::dispatch_tool(&tools, "echo", "{}".to_string(), None, None).await,
        crate::tool_call_lifecycle::ToolOutcome::Completed("ECHOED".to_string())
    );
    // An unresolved tool name is a dispatch FAILURE carried as typed data.
    // Classifying it `Completed` would durably record a hallucinated tool name
    // as a successful call (fenced end-to-end by
    // `hook::tests::hook_maps_unknown_tool_dispatch_to_failed_lifecycle`).
    let unknown = super::dispatch_tool(&tools, "missing", "{}".to_string(), None, None).await;
    match &unknown {
        crate::tool_call_lifecycle::ToolOutcome::Failed {
            denial: None, text, ..
        } => {
            assert_eq!(text, "error: unknown tool 'missing'");
        }
        other => panic!("unknown tool must classify as a dispatch failure, got {other:?}"),
    }
    // The model still sees exactly the text it always saw.
    assert_eq!(unknown.model_facing_text(), "error: unknown tool 'missing'");
}

#[tokio::test]
async fn dispatch_tool_types_unparseable_args_as_argument_invalid() {
    use crate::llm::tool::{Tool, ToolDefinition};

    // A tool whose Args require fields the (valid-JSON) call omits, so the real
    // parse seam raises UnparseableArgs.
    struct StrictArgsTool;
    #[derive(Debug, thiserror::Error)]
    #[error("strict tool error")]
    struct StrictToolError;
    #[derive(serde::Deserialize)]
    struct StrictArgs {
        #[allow(dead_code)]
        body: String,
        #[allow(dead_code)]
        findings: Vec<String>,
    }
    impl Tool for StrictArgsTool {
        const NAME: &'static str = "strict";
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
            Ok("ran".to_string())
        }
    }

    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictArgsTool)];
    // Truncated mid-string: escape-only repair cannot complete it, so it stays
    // UnparseableArgs and dispatch types it `Failed(ArgumentInvalid)` carrying
    // the model-facing notice — not the tool output.
    let result = super::dispatch_tool(
        &tools,
        "strict",
        r#"{"body":"cut off"#.to_string(),
        None,
        None,
    )
    .await;
    match &result {
        crate::tool_call_lifecycle::ToolOutcome::Failed {
            class,
            denial: None,
            text,
        } => {
            assert_eq!(
                *class,
                crate::tool_call_lifecycle::FailureClass::ArgumentInvalid
            );
            assert!(
                !text.contains("ran") && text.contains("token limit"),
                "the notice must replace the tool output and guide the model to shorten, got: {text}"
            );
        }
        other => panic!("unparseable args must classify ArgumentInvalid, got {other:?}"),
    }
}

/// Loop-level fence: an unparseable-args tool call (a) does NOT run the tool,
/// (b) surfaces a clean notice to the model (the internal marker stripped) so it
/// can re-emit corrected arguments next turn, and (c) terminalizes the started
/// `AgentToolCall` as `failed`/`argumentInvalid` via `on_tool_result`. This
/// preserves the tool-call liveness invariant (Lean
/// `ToolExecution.live_call_reaches_terminal`, T5: the started call reaches a
/// terminal state) using the proven `Running → Failed` edge with the existing
/// `FailureClass::ArgumentInvalid`.
#[tokio::test]
async fn unparseable_tool_args_notify_model_and_terminalize_failed() {
    use crate::llm::tool::{Tool, ToolDefinition};

    struct StrictArgsTool;
    #[derive(Debug, thiserror::Error)]
    #[error("strict tool error")]
    struct StrictToolError;
    #[derive(serde::Deserialize)]
    struct StrictArgs {
        #[allow(dead_code)]
        report_type: String,
        #[allow(dead_code)]
        findings: Vec<String>,
    }
    impl Tool for StrictArgsTool {
        const NAME: &'static str = "post_status";
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
            // Must NOT run: the args never deserialize.
            panic!("the tool must not run on unparseable arguments");
        }
    }

    let (node, hook) = test_hook().await;

    // Valid JSON, but missing the required `findings` field: a Malformed parse
    // failure that no repair can recover into the typed args.
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "post_status".to_string(),
                serde_json::json!({ "report_type": "steward" }),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictArgsTool)];

    let stream = run_loop_stream(
        model,
        Some(hook),
        Message::user("post a status report"),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    // The model is notified via a tool result (no error ends the stream); it sees
    // the clean notice and answers on the next turn.
    let mut tool_results = Vec::new();
    while let Some(item) = stream.next().await {
        if let LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
            StreamedUserContent::ToolResult { tool_result, .. },
        )) = item.expect("loop must not fail; unparseable args are notified, not raised")
        {
            tool_results.push(
                tool_result_text(&crate::llm::rig_compat::from_rig_tool_result_content(
                    &tool_result.content.first(),
                ))
                .to_string(),
            );
        }
    }
    assert!(
        tool_results
            .iter()
            .any(|r| r.contains("could not be parsed")),
        "the model must be notified with a clean parse-failure notice, got: {tool_results:?}"
    );
    assert!(
        !tool_results
            .iter()
            .any(|r| r.contains("__gents_tool_lifecycle__")),
        "the internal marker must never leak to the model, got: {tool_results:?}"
    );

    // T5: the started call terminalized failed(argumentInvalid) — via on_tool_result
    // stripping the marker and forcing ArgumentInvalid — instead of dangling in `running`.
    let resp = node
        .execute("query { AgentToolCall { tool_name lifecycle_state tool_failure_class } }")
        .await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall query failed: {:?}",
        resp.errors
    );
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|v| v.as_str()) == Some("post_status")
                && row.get("lifecycle_state").and_then(|v| v.as_str()) == Some("failed")
                && row.get("tool_failure_class").and_then(|v| v.as_str()) == Some("argumentInvalid")
        }),
        "the started tool call must terminalize failed/argumentInvalid, got rows: {rows:?}"
    );
}
