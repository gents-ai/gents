#[tokio::test]
async fn dispatch_receipt_loss_gates_real_hook_loop_invocation() {
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().canonical_dispatch_observation_cases;
    let cases: Vec<_> = cases
        .iter()
        .filter(|case| case.inputs[0].policy_allows)
        .collect();
    assert!(cases.iter().any(|case| case.inputs[0].acknowledged));
    assert!(cases.iter().any(|case| !case.inputs[0].acknowledged));
    for case in cases {
        let input = &case.inputs[0];
        let expected = &case.expected[0];
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            let (node, hook, writer, mut lifecycle) = owned_test_hook_with_policy(policy).await;
            let calls = Arc::new(AtomicUsize::new(0));
            let model = ScriptedModel::new_turns(vec![
                vec![
                    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                        "dispatch-call".into(),
                        "echo".into(),
                        serde_json::json!({}),
                    )),
                    RawStreamingChoice::FinalResponse(()),
                ],
                vec![
                    RawStreamingChoice::Message("done".into()),
                    RawStreamingChoice::FinalResponse(()),
                ],
            ]);
            let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(CountingTool {
                name: "echo".into(),
                output: "observed".into(),
                calls: calls.clone(),
            })];
            let stream = run_loop_stream(
                model,
                Some(hook.clone()),
                TaggedMessage::unassociated(Message::user("run echo")),
                Vec::new(),
                Arc::new(tools),
                owned_config(4),
            );
            let collect = collect_owned_scripted_stream(
                stream,
                &hook,
                &writer,
                &mut lifecycle,
                gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
            );
            let collected = if input.acknowledged {
                let (collected, fired) = crate::config_client::ConfigApplyTxn::
                    with_post_commit_receipt_loss_for_operation(
                        Some("test.unrelated_transaction"), collect,
                    ).await;
                assert!(
                    !fired,
                    "unrelated commits must not consume a targeted fault"
                );
                collected
            } else {
                let (collected, fired) = crate::config_client::ConfigApplyTxn::
                    with_post_commit_receipt_loss_for_operation(
                        Some("tool_call.start_running_canonical"), collect,
                    ).await;
                assert!(fired, "{}: dispatch commit must be reached", case.name);
                collected
            };
            assert_eq!(
                calls.load(Ordering::SeqCst),
                usize::from(expected.may_invoke),
                "{}: policy {policy:?}",
                case.name
            );
            if expected.may_invoke {
                assert!(collected.error.is_none(), "{:?}", collected.error);
                assert_eq!(collected.tool_results, ["observed"]);
                assert_eq!(collected.final_text.as_deref(), Some("done"));
            } else {
                assert!(
                    collected.error.is_some(),
                    "lost authority must stop the loop"
                );
                assert!(collected.tool_results.is_empty());
                let rows = crate::config_client::ConfigAccess::Local(node.clone())
                    .execute("query { AgentToolCall { lifecycle_state } }")
                    .await
                    .unwrap();
                let rows = rows["data"]["AgentToolCall"].as_array().unwrap();
                assert_eq!(rows.len(), 1, "publication must precede dispatch");
                assert_eq!(
                    rows[0]["lifecycle_state"], "running",
                    "receipt loss must not roll back the committed dispatch"
                );
            }
            node.shutdown().await;
        }
    }
}

enum PolicyDeniedOwner {
    ReadOnlyBash,
    CliArgvPrefix,
    McpAllowlist,
}

struct PolicyDeniedCall {
    tool_name: &'static str,
    arguments: serde_json::Value,
    tools: Vec<Box<dyn ToolDyn>>,
}

impl PolicyDeniedOwner {
    const ALL: [PolicyDeniedOwner; 3] = [
        PolicyDeniedOwner::ReadOnlyBash,
        PolicyDeniedOwner::CliArgvPrefix,
        PolicyDeniedOwner::McpAllowlist,
    ];

    fn owner(&self) -> &'static str {
        match self {
            Self::ReadOnlyBash => "read_only_bash",
            Self::CliArgvPrefix => "cli_argv_prefix",
            Self::McpAllowlist => "mcp_allowlist",
        }
    }

    fn denied_call(
        &self,
        node: &Arc<defra_node::EmbeddedNode>,
        root: &std::path::Path,
    ) -> PolicyDeniedCall {
        match self {
            Self::ReadOnlyBash => PolicyDeniedCall {
                tool_name: "bash",
                arguments: serde_json::json!({"command": "rm", "args": ["-rf", "."]}),
                tools: vec![crate::toolset::read_only_bash_for_test(
                    root,
                    vec!["ls".into()],
                )],
            },
            Self::CliArgvPrefix => PolicyDeniedCall {
                tool_name: "git",
                arguments: serde_json::json!({"argv": ["push", "--force"]}),
                tools: vec![crate::toolset::cli_tool_for_test(
                    crate::toolset::CliToolConfig {
                        name: "git".into(),
                        binary_path: "/bin/echo".into(),
                        description: String::new(),
                        allowed_argv_prefixes: vec![vec!["status".into()]],
                        env_vars: std::collections::HashMap::new(),
                        working_dir: Some(root.to_path_buf()),
                        timeout_secs: 5,
                        max_output_chars: 4096,
                    },
                )],
            },
            Self::McpAllowlist => PolicyDeniedCall {
                tool_name: "call_tool",
                arguments: serde_json::json!({
                    "service_id": "selected-service",
                    "tool_name": "unselected-tool",
                    "arguments": {}
                }),
                tools: vec![Box::new(crate::meta_tools::CallToolTool::new(
                    crate::meta_tools::MetaToolContext {
                        node: node.clone(),
                        mcp_pool: crate::mcp_pool::McpPool::new().for_agent("did:test:test"),
                        health: crate::health_checker::ServiceHealthMap::new(),
                        local_hostname: "local".into(),
                        local_subnet: None,
                        agent_did: "did:test:test".into(),
                        allowed_mcp_service_ids: vec!["selected-service".into()],
                        remote_tools: crate::document_config::RemoteTools {
                            services: vec![crate::document_config::RemoteServiceTools {
                                mcp_service_id: "selected-service".into(),
                                tool_names: vec!["selected-tool".into()],
                                ..Default::default()
                            }],
                        },
                    },
                ))],
            },
        }
    }
}

#[tokio::test]
async fn policy_rejection_settles_pending_call_without_dispatch_election() {
    let cases: Vec<_> = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_dispatch_observation_cases
        .iter()
        .filter_map(|case| {
            case.expected_after_policy_settlement
                .as_ref()
                .map(|settlement| (case, settlement))
        })
        .collect();
    assert!(!cases.is_empty());
    for (case, settlement) in cases {
        assert!(case
            .inputs
            .iter()
            .all(|input| input.acknowledged && !input.policy_allows));
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            for owner in PolicyDeniedOwner::ALL {
                let (node, hook, writer, mut lifecycle) =
                    owned_test_hook_with_policy(policy).await;
                let root = tempfile::tempdir().unwrap();
                let denied = owner.denied_call(&node, root.path());
                let model = ScriptedModel::new_turns(vec![
                    vec![
                        RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                            "policy-call".into(),
                            denied.tool_name.into(),
                            denied.arguments,
                        )),
                        RawStreamingChoice::FinalResponse(()),
                    ],
                    vec![
                        RawStreamingChoice::Message("done".into()),
                        RawStreamingChoice::FinalResponse(()),
                    ],
                ]);
                let stream = run_loop_stream(
                    model,
                    Some(hook.clone()),
                    TaggedMessage::unassociated(Message::user("run the denied call")),
                    Vec::new(),
                    Arc::new(denied.tools),
                    owned_config(4),
                );
                let collect = collect_owned_scripted_stream(
                    stream,
                    &hook,
                    &writer,
                    &mut lifecycle,
                    gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
                );
                let (collected, fired) = crate::config_client::ConfigApplyTxn::
                    with_post_commit_receipt_loss_for_operation(
                        Some("tool_call.start_running_canonical"), collect,
                    ).await;

                let rows = crate::config_client::ConfigAccess::Local(node.clone())
                    .execute("query { AgentToolCall { lifecycle_state started_at tool_failure_class } }")
                    .await
                    .unwrap();
                let rows = rows["data"]["AgentToolCall"].as_array().unwrap();
                assert_eq!(rows.len(), 1, "publication precedes admission");
                let row = &rows[0];
                assert_eq!(
                    (
                        row["lifecycle_state"] == "failed",
                        row["lifecycle_state"] == "running",
                        row["started_at"].is_string(),
                        row["tool_failure_class"].as_str(),
                    ),
                    (
                        settlement.failed,
                        settlement.running,
                        settlement.started,
                        settlement.failure_class.as_deref(),
                    ),
                    "{}/{}: policy {policy:?}",
                    case.name,
                    owner.owner(),
                );
                assert_eq!(
                    fired,
                    case.expected.iter().any(|expected| expected.running),
                    "{}/{}: the dispatch election commits only for a modeled Running call",
                    case.name,
                    owner.owner(),
                );
                assert!(collected.error.is_none(), "{:?}", collected.error);
                assert_eq!(collected.tool_results.len(), 1);
                assert_eq!(collected.final_text.as_deref(), Some("done"));
                assert!(root.path().exists());

                let completion = lifecycle
                    .terminalize_owned(
                        crate::lifecycle::RequestTerminalOutcome::Completed,
                        writer.terminal_output(&lifecycle.request().doc_id).await,
                        None,
                    )
                    .await;
                assert_eq!(
                    completion.is_ok(),
                    settlement.completion_accepted,
                    "{}/{}: {completion:?}",
                    case.name,
                    owner.owner(),
                );
                node.shutdown().await;
            }
        }
    }
}

#[tokio::test]
async fn tool_call_turn_executes_threads_result_and_completes() {
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
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
        Some(hook.clone()),
        TaggedMessage::unassociated(prompt),
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

    // The tool ran, its (bounded) result was threaded/yielded, and the loop
    // reached a text response on the next turn.
    assert_eq!(collected.tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(collected.final_text.as_deref(), Some("done"));

    let resp = node
        .execute("query { AgentToolCall { _docID tool_name lifecycle_state } }")
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
        }),
        "expected a completed echo tool call; rows: {rows:?}"
    );
    let tool_doc_id = rows
        .iter()
        .find(|row| row["tool_name"] == "echo")
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
    assert!(output.contains("ECHOED"));
}

#[tokio::test(start_paused = true)]
async fn tool_does_not_execute_when_provider_stalls_before_turn_closure() {
    // A streamed call is retained intent, not accepted execution authority.
    // Provider EOF/Complete acceptance must precede any host side effect.
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedModel::new_stalling(vec![RawStreamingChoice::ToolCall(
        RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        ),
    )]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(CountingTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
        calls: calls.clone(),
    })];
    let stream = run_loop_stream(
        model,
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("use the echo tool then stall")),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);
    let first = stream.next().await.expect("should observe tool intent");
    assert!(
        matches!(
            first,
            Ok(LoopStreamItem::Item(
                MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall { .. })
            ))
        ),
        "first item should be streamed intent: {first:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .is_err(),
        "a stalled provider must neither accept its turn nor dispatch a tool"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn tool_definition_receives_prompt_rag_text() {
    // P3/compat: tool definitions must be built with the prompt's rag text (rig
    // parity), not String::new(), so prompt-aware tools keep the task context.
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
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("teach me rust")),
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
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("use the echo tool")),
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
    let prompt = Message::user("read the big thing");

    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;

    // A tool returning far more than the default limits: the model-facing
    // (threaded/yielded) result must be bounded, while on_tool_result still
    // receives the full output for canonical full-output persistence
    // (#401 closed natively).
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
        Some(hook.clone()),
        TaggedMessage::unassociated(prompt),
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

    let tool_results = collected.tool_results;
    assert_eq!(
        tool_results.len(),
        1,
        "exactly one threaded tool result expected; got: {tool_results:?}"
    );
    let bounded_len = tool_results[0].len();
    assert!(
        bounded_len < full_len,
        "expected the threaded result to be bounded: bounded={bounded_len} full={full_len}"
    );
    assert!(bounded_len > 0, "bounded result should be non-empty");
    assert_eq!(collected.final_text.as_deref(), Some("ok"));
    node.shutdown().await;
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
async fn missing_tool_args_notify_model_and_terminalize_failed() {
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

    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;

    // Valid JSON missing the required `findings` field is classified precisely
    // so the model can repair the call without being told its JSON was malformed.
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
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("post a status report")),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );
    // The model is notified via a tool result (no error ends the stream); it sees
    // the actionable missing-field notice and answers on the next turn.
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    let tool_results = collected.tool_results;
    assert!(
        tool_results
            .iter()
            .any(|r| r.contains("arguments were rejected (missing field)")
                && r.contains("missing field `findings`")),
        "the model must be notified with a precise missing-field notice, got: {tool_results:?}"
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

/// Terminalization holds the process-wide write gate while it validates every
/// accepted tool reply; the request's output is scanned once, not per tool.
#[tokio::test]
async fn completed_terminalization_scans_request_output_once_for_many_tools() {
    const TOOL_TURNS: usize = 6;
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let mut script = (0..TOOL_TURNS)
        .map(|turn| {
            vec![
                RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    format!("call-{turn}"),
                    "echo".into(),
                    serde_json::json!({}),
                )),
                RawStreamingChoice::FinalResponse(()),
            ]
        })
        .collect::<Vec<_>>();
    script.push(vec![
        RawStreamingChoice::Message("done".into()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(FixedTool {
        name: "echo".into(),
        output: "observed".into(),
    })];
    let stream = run_loop_stream(
        ScriptedModel::new_turns(script),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("run echo")),
        Vec::new(),
        Arc::new(tools),
        owned_config(TOOL_TURNS + 2),
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
    assert_eq!(collected.tool_results.len(), TOOL_TURNS);
    let selection = writer.terminal_output(&lifecycle.request().doc_id).await;
    let (result, scans) = crate::session::count_request_output_scans(lifecycle.terminalize_owned(
        crate::lifecycle::RequestTerminalOutcome::Completed,
        selection,
        None,
    ))
    .await;
    assert_eq!(result.unwrap(), crate::lifecycle::TerminalizeResult::Won);
    assert_eq!(scans, 1, "request output scanned once per accepted tool");
    node.shutdown().await;
}

/// Time the terminalization of one request with `turns` tool turns whose
/// outputs are `output_bytes` long, after `noise` replicas of its canonical
/// output were written under the same principal in other sessions.
async fn terminalization_with_principal_output(turns: usize, output_bytes: usize, noise: usize) -> std::time::Duration {
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let mut script = (0..turns)
        .map(|turn| {
            vec![
                RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    format!("call-{turn}"),
                    "echo".into(),
                    serde_json::json!({}),
                )),
                RawStreamingChoice::FinalResponse(()),
            ]
        })
        .collect::<Vec<_>>();
    script.push(vec![
        RawStreamingChoice::Message("done".into()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(FixedTool {
        name: "echo".into(),
        output: "o".repeat(output_bytes),
    })];
    let stream = run_loop_stream(
        ScriptedModel::new_turns(script),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("run echo")),
        Vec::new(),
        Arc::new(tools),
        owned_config(turns + 2),
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
    assert_eq!(collected.tool_results.len(), turns);
    // Writing the surrounding output outlasts the lease; decide at the
    // instant the loop finished.
    let finished_at = chrono::Utc::now();
    let generation = lifecycle.execution_generation().unwrap().to_owned();
    let request_doc_id = lifecycle.request().doc_id.clone();
    let segments = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS
        ))
        .await;
    let segments = segments.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| crate::session::canonical_rows::decode_output_segment_row(row).unwrap())
        .collect::<Vec<_>>();
    let messages = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_MESSAGE_FIELDS
        ))
        .await;
    let messages = messages.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| crate::session::canonical_rows::decode_transcript_message_row(row).unwrap())
        .collect::<Vec<_>>();
    for replica in 0..noise {
        let other_request = format!("bae-noise-request-{replica}");
        let other_session = format!("noise-session-{replica}");
        for row in &segments {
            let mut segment = row.segment.clone();
            segment.request_doc_id = other_request.clone();
            segment.session_id = other_session.clone();
            let response = node
                .execute_request_with_retry(
                    defra_node::QueryRequest::new(
                        crate::session::canonical_rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                    )
                    .with_variables(
                        crate::session::canonical_rows::output_segment_create_variables(&segment)
                            .unwrap(),
                    ),
                    defra_node::ExecuteRetryPolicy::default(),
                )
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        for row in &messages {
            let mut message = row.message.clone();
            message.request_doc_id = Some(other_request.clone());
            message.session_id = other_session.clone();
            message.message_key = format!("{}:{replica}", message.message_key);
            let response = node
                .execute_request_with_retry(
                    defra_node::QueryRequest::new(
                        crate::session::canonical_rows::CREATE_AGENT_MESSAGE_MUTATION,
                    )
                    .with_variables(
                        crate::session::canonical_rows::transcript_message_create_variables(&message)
                            .unwrap(),
                    ),
                    defra_node::ExecuteRetryPolicy::default(),
                )
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
    }
    let selection = writer.terminal_output(&request_doc_id).await;
    let started = std::time::Instant::now();
    let result = crate::lifecycle::terminalize_owned_at(
        &node,
        &request_doc_id,
        &generation,
        crate::lifecycle::RequestTerminalOutcome::Completed,
        selection,
        finished_at,
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(result.unwrap(), crate::lifecycle::TerminalizeResult::Won);
    drop(lifecycle);
    eprintln!(
        "terminalization turns={turns} bytes={output_bytes} noise={noise} segments={} messages={} terminalize_ms={}",
        segments.len(),
        messages.len(),
        elapsed.as_millis()
    );
    node.shutdown().await;
    elapsed
}

/// Terminalization holds the process-wide write gate, so its reads must be
/// bounded by the request, not by everything its principal ever wrote: a
/// fan-out of long requests otherwise starves lease renewal behind it.
#[tokio::test]
async fn terminalization_cost_is_independent_of_principal_output() {
    let alone = terminalization_with_principal_output(24, 4096, 0).await;
    let surrounded = terminalization_with_principal_output(24, 4096, 40).await;
    assert!(
        surrounded <= alone * 2 + std::time::Duration::from_millis(200),
        "terminalization grew with unrelated principal output: {alone:?} alone, {surrounded:?} beside 40 requests"
    );
}

/// Manual scale grid: `TERMINALIZE_SCALE=turns:bytes:noise,...`.
#[tokio::test]
#[ignore = "manual terminalization scale measurement"]
async fn terminalization_scale_grid() {
    let grid = std::env::var("TERMINALIZE_SCALE").unwrap_or_else(|_| "24:4096:0,24:4096:40".into());
    for point in grid.split(',') {
        let values = point.split(':').map(|v| v.parse::<usize>().unwrap()).collect::<Vec<_>>();
        terminalization_with_principal_output(values[0], values[1], values[2]).await;
    }
}

#[tokio::test]
async fn background_output_query_work_is_independent_of_principal_history() {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("gents::agent::loop_stream::tests=info")
        .with_test_writer()
        .finish();
    let _trace = tracing::subscriber::set_default(subscriber);
    use crate::session::canonical_rows::{
        output_segment_create_variables, request_output_segments_query,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{OutputSource, OutputWriter};
    let (node, _hook, _writer, lifecycle) = owned_test_hook().await;
    let request_id = lifecycle.request().doc_id.clone();
    let template = gents_protocol::output::OutputSegment {
        agent_did: "did:test:owner".into(),
        requester_did: None,
        session_id: "session".into(),
        request_doc_id: request_id.clone(),
        source: OutputSource::ToolCall {
            tool_call_doc_id: "tool-doc".into(),
        },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: "tool-doc".into(),
        },
        ordinal: Some(0),
        runs: vec![gents_protocol::output::SegmentRun {
            stream: 0,
            bytes: 6,
            declaration: Some(gents_protocol::output::StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: gents_protocol::output::StreamPayload::ToolOutput,
            }),
        }],
        payload: "output".into(),
        close: Some(gents_protocol::output::SourceClose::Closed {
            outcome: gents_protocol::output::OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![6],
        }),
        created_at: "2026-10-02T00:00:00Z".into(),
    };
    async fn create(
        node: &defra_node::EmbeddedNode,
        segment: &gents_protocol::output::OutputSegment,
    ) {
        let response = node
            .execute_request_with_retry(
                defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(segment).unwrap()),
                defra_node::ExecuteRetryPolicy::default(),
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    fn scan<'a>(value: &'a serde_json::Value, field: &str) -> Option<&'a serde_json::Value> {
        if value.get(field).is_some() {
            return Some(value);
        }
        match value {
            serde_json::Value::Object(object) => {
                object.values().find_map(|value| scan(value, field))
            }
            serde_json::Value::Array(array) => array.iter().find_map(|value| scan(value, field)),
            _ => None,
        }
    }
    create(&node, &template).await;
    let query = request_output_segments_query(&request_id);
    let mut work = Vec::new();
    let mut previous_work = Vec::new();
    for noise in [0, 160] {
        if noise > 0 {
            for replica in 0..noise {
                let mut segment = template.clone();
                segment.request_doc_id = format!("noise-request-{replica}");
                segment.session_id = format!("noise-session-{replica}");
                segment.payload = "n".repeat(4096);
                segment.runs[0].bytes = 4096;
                segment.close = Some(gents_protocol::output::SourceClose::Closed {
                    outcome: gents_protocol::output::OutputOutcome::Complete,
                    segments: 1,
                    stream_bytes: vec![4096],
                });
                create(&node, &segment).await;
            }
        }
        let query_started = std::time::Instant::now();
        let response = crate::graphql::graphql_with_transaction_retry(
            &node,
            &format!("query @explain(type: execute) {query}"),
            "explain request output",
        )
        .await
        .unwrap();
        let query_elapsed = query_started.elapsed();
        let data = response.data.unwrap();
        let request_scan = scan(&data, "indexFetches")
            .unwrap_or_else(|| panic!("request output must report index execution work: {data}"));
        let plan = crate::graphql::graphql_with_transaction_retry(
            &node,
            &format!("query @explain {query}"),
            "explain request output plan",
        )
        .await
        .unwrap()
        .data
        .unwrap();
        let plan_scan = scan(&plan, "indexName")
            .unwrap_or_else(|| panic!("request output must use an index scan: {plan}"));
        assert!(
            plan_scan["indexName"]
                .as_str()
                .unwrap()
                .contains("request_doc_id"),
            "{plan}"
        );
        work.push(
            request_scan["indexFetches"]
                .as_u64()
                .expect("execution must report index work"),
        );
        let scoped_query = format!(
            r#"query @explain(type: execute) {{ AgentOutputSegment(filter: {{ {}, request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            crate::session::session_scope_filter("did:test:owner", "session", None),
            crate::graphql::escape_graphql_string(&request_id),
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS,
        );
        let previous_started = std::time::Instant::now();
        let previous = crate::graphql::graphql_with_transaction_retry(
            &node,
            &scoped_query,
            "explain previous background output",
        )
        .await
        .unwrap();
        let previous_elapsed = previous_started.elapsed();
        let previous_data = previous.data.unwrap();
        let previous_scan = scan(&previous_data, "indexFetches").unwrap_or_else(|| {
            panic!("previous output must report index execution work: {previous_data}")
        });
        tracing::info!(
            noise,
            request_query_micros = query_elapsed.as_micros(),
            previous_query_micros = previous_elapsed.as_micros(),
            "background query execution comparison"
        );
        previous_work.push(previous_scan["indexFetches"].as_u64().unwrap());
        let started = std::time::Instant::now();
        let output = crate::background_tools::canonical_tool_output(
            &node,
            "tool-doc",
            &request_id,
            "session",
            "did:test:owner",
            None,
        )
        .await
        .unwrap();
        let output_elapsed = started.elapsed();
        assert_eq!(output, "output");
        assert_eq!(
            crate::background_tools::observe_canonical_tool_output_with_access(
                &crate::config_client::ConfigAccess::Local(node.clone()),
                "tool-doc",
                &request_id,
                "session",
                "did:test:owner",
                None,
            )
            .await
            .unwrap(),
            crate::background_tools::CanonicalToolOutputObservation::Closed(output)
        );

        tracing::info!(
            noise,
            index_fetches = work.last().unwrap(),
            output_fetch_micros = output_elapsed.as_micros(),
            "background output query scale"
        );
    }
    assert_eq!(
        work[0], work[1],
        "request query work grew with unrelated history"
    );
    assert!(work[0] > 0);
    tracing::info!(
        ?work,
        ?previous_work,
        "request scoped background output scan comparison"
    );
    drop(lifecycle);
    node.shutdown().await;
}
