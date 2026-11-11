#[tokio::test]
async fn dispatch_receipt_loss_gates_real_hook_loop_invocation() {
    let cases =
        &gents_lean_vocab::lean_contract_snapshot().canonical_dispatch_observation_cases;
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
                let (collected, fired) = gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
                        Some("test.unrelated_transaction"), collect,
                    ).await;
                assert!(
                    !fired,
                    "unrelated commits must not consume a targeted fault"
                );
                collected
            } else {
                let (collected, fired) = gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
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
                let rows = lifecycle.access().unwrap()
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
            node.shutdown().await.expect("node shutdown");
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
        node: &Arc<defra_node::Node>,
        root: &std::path::Path,
    ) -> PolicyDeniedCall {
        match self {
            Self::ReadOnlyBash => PolicyDeniedCall {
                tool_name: "bash",
                arguments: serde_json::json!({"command": "rm", "args": ["-rf", "."]}),
                tools: vec![gents_tools::native::read_only_bash_for_test(
                    root,
                    vec!["ls".into()],
                )],
            },
            Self::CliArgvPrefix => PolicyDeniedCall {
                tool_name: "git",
                arguments: serde_json::json!({"argv": ["push", "--force"]}),
                tools: vec![gents_tools::native::cli_tool_for_test(
                    gents_tools::native::CliToolConfig {
                        name: "git".into(),
                        binary_path: "/bin/echo".into(),
                        description: String::new(),
                        allowed_argv_prefixes: vec![vec!["status".into()]],
                        env_vars: rapidhash::RapidHashMap::default(),
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
                tools: vec![Box::new(gents_tools::mcp::meta::CallToolTool::new(
                    gents_tools::mcp::meta::MetaToolContext {
                        configuration: gents_inference::resolved::ResolvedConfiguration::Static,
                        actor: identity::Did::new(gents_automation::triggers::callback::test_agent_did()).unwrap(),
                        node: node.clone(),
                        mcp_pool: gents_tools::mcp::pool::McpPool::new()
                            .for_agent(node.node_identity_did().expect("fixture node has an identity")),
                        health: gents_tools::mcp::health::ServiceHealthMap::new(),
                        local_hostname: "local".into(),
                        local_subnet: None,
                        node_did: node.node_identity_did().expect("fixture node has an identity").into(),
                        allowed_mcp_service_ids: vec!["selected-service".into()],
                        remote_tools: gents_documents::RemoteTools {
                            services: vec![gents_documents::RemoteServiceTools {
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
    let cases: Vec<_> = gents_lean_vocab::lean_contract_snapshot()
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
                let (node, hook, writer, mut lifecycle) = owned_test_hook_with_policy(policy).await;
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
                let (collected, fired) = gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
                        Some("tool_call.start_running_canonical"), collect,
                    ).await;

                let rows = lifecycle.access().unwrap()
                    .execute(
                        "query { AgentToolCall { lifecycle_state started_at tool_failure_class } }",
                    )
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
                        gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
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
                node.shutdown().await.expect("node shutdown");
            }
        }
    }
}

#[tokio::test]
async fn tool_call_turn_executes_threads_result_and_completes() {
    let (_node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let prompt = Message::user("use the echo tool");

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
        model.clone(),
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
    assert_eq!(collected.tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(collected.final_text.as_deref(), Some("done"));

    let dispatched = model.seen_histories().await;
    let fresh_result = dispatched[1]
        .iter()
        .find_map(|message| match message {
            Message::User { content } => content.iter().find_map(|item| match item {
                UserContent::ToolResult(result) => Some(result),
                _ => None,
            }),
            _ => None,
        })
        .expect("the next provider dispatch carries the fresh tool result");
    assert!(
        tool_result_text(&fresh_result.content[0]).contains("ECHOED"),
        "fresh result must reach the next dispatch intact below threshold"
    );

    let persisted = gents_session::session::load_history_with_access(
        &lifecycle.access().unwrap(),
        &lifecycle.request().session_id,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
    .await
    .expect("load persisted session history");
    let persisted_view = crate::runtime::agent::loop_stream::provider_view_tagged(
        persisted
            .into_iter()
            .map(crate::runtime::agent::loop_stream::TaggedMessage::unassociated)
            .collect(),
    )
    .expect("project durable history for a later request");
    let durable_result = persisted_view
        .iter()
        .flat_map(|row| match &row.message {
            Message::User { content } => content.iter().collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .find_map(|item| match item {
            UserContent::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("persisted tool result remains represented in provider history");
    let durable_stub = tool_result_text(&durable_result.content[0]);
    assert!(durable_stub.contains("full output: sessions"));
    assert!(durable_stub.contains("\"action\":\"output\""));
    assert!(durable_stub.contains("\"call_id\":\"call-1\""));
    assert!(!durable_stub.contains("ECHOED"));

    let resp = agent_response(&lifecycle.access().unwrap(), "query { AgentToolCall { _docID tool_name lifecycle_state } }").await;
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
    let output = agent_tool_output(&lifecycle, tool_doc_id).await;
    assert!(output.contains("ECHOED"));
}

#[tokio::test(start_paused = true)]
async fn tool_does_not_execute_when_provider_stalls_before_turn_closure() {
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
        matches!(first, Ok(LoopStreamItem::ToolCall { .. })),
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
async fn toolset_is_attached_to_every_completion_request_in_the_loop() {
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

    let big_line = "x".repeat(200);
    let big_output = std::iter::repeat_n(big_line, 10_000)
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
    node.shutdown().await.expect("node shutdown");
}

fn plugin_image_tool(image_bytes: usize) -> (tempfile::TempDir, Box<dyn ToolDyn>) {
    const PREFIX: &[u8] = br#"{"response":{"width":2,"height":2},"parts":[{"type":"image","mimeType":"image/png","data":""#;
    const SUFFIX: &[u8] = br#""}]}"#;
    let (home, record) = gents_toolcalls::test_support::plugin::installed_plugin(
        &gents_pack::plugin::test_fixtures::filled_output_wat(PREFIX, image_bytes, SUFFIX),
        None,
    );
    let executor = Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let tool = gents_toolcalls::plugin::tool::PluginTool::resolve(
        executor,
        &gents_documents::PluginToolRef {
            tool_calls: false,
            plugin: "team/plugin".into(),
            digest: Some(record.digest.clone()),
            input_fields: Vec::new(),
            job: None,
        },
        None,
    )
    .unwrap();
    (home, Box::new(tool))
}

fn plugin_call_then_text_turns() -> Vec<Vec<RawStreamingChoice<()>>> {
    vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "plugin".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("seen".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]
}

async fn plugin_image_result_seen_by(
    provider: gents_inference::backends::provider::BackendProviderKind,
    image_bytes: usize,
) -> Vec<ToolResultContent> {
    let (_home, tool) = plugin_image_tool(image_bytes);
    let tools: Vec<Box<dyn ToolDyn>> = vec![tool];
    let model = ScriptedModel::new_turns(plugin_call_then_text_turns()).without_capture();
    let mut config = config(4);
    config.provider_input_counter =
        Arc::new(crate::turn::provider_input::ProviderInputCounter::new(
            provider,
            gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
            "test-model",
        ));
    let stream = run_loop_stream(
        model.clone(),
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("draw it")),
        Vec::new(),
        Arc::new(tools),
        config,
    );
    futures::pin_mut!(stream);
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        match item {
            Err(error) => panic!("{provider:?}: {error}"),
            Ok(LoopStreamItem::Final { text }) => final_text = Some(text),
            Ok(_) => {}
        }
    }
    assert_eq!(final_text.as_deref(), Some("seen"));
    let histories = model.seen_histories().await;
    histories[1]
        .iter()
        .find_map(|message| match message {
            Message::User { content } => content.iter().find_map(|part| match part {
                UserContent::ToolResult(result) => Some(result.content.clone()),
                _ => None,
            }),
            _ => None,
        })
        .expect("the second request carries the plugin's tool result")
}

#[tokio::test]
async fn plugin_image_parts_reach_the_provider_whole() {
    const IMAGE_BYTES: usize = 900_000;
    let result =
        plugin_image_result_seen_by(gents_inference::backends::provider::BackendProviderKind::AnthropicApiKey, IMAGE_BYTES).await;
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

async fn persisted_plugin_image_result_replay(image_bytes: usize) {
    let (_home, tool) = plugin_image_tool(image_bytes);
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let stream = run_loop_stream(
        ScriptedModel::new_turns(plugin_call_then_text_turns()),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("draw it")),
        Vec::new(),
        Arc::new(vec![tool]),
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
    assert_eq!(collected.final_text.as_deref(), Some("seen"));

    let session_id = hook.session_id().await.expect("session id");
    let history = gents_session::session::load_history_with_access(
        &lifecycle.access().unwrap(),
        &session_id,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
        .await
        .unwrap();
    let replayed: Vec<String> = history
        .iter()
        .flat_map(|message| match message {
            Message::User { content } => content.as_slice(),
            _ => &[],
        })
        .filter_map(|part| match part {
            UserContent::ToolResult(result) => Some(&result.content),
            _ => None,
        })
        .flatten()
        .map(|part| tool_result_text(part).to_string())
        .collect();
    assert_eq!(
        replayed,
        vec![format!(
            "{}\n{}",
            r#"{"height":2,"width":2}"#,
            gents_loop::loop_stream::TOOL_RESULT_IMAGE_NOT_REPLAYED
        )],
        "image of {image_bytes} bytes"
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn plugin_image_parts_become_a_note_where_the_provider_refuses_them() {
    let result =
        plugin_image_result_seen_by(gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible, 64).await;
    assert_eq!(
        result.iter().map(tool_result_text).collect::<Vec<_>>(),
        vec![
            r#"{"height":2,"width":2}"#,
            gents_loop::loop_stream::TOOL_RESULT_IMAGE_OMITTED,
        ]
    );
}

#[tokio::test]
async fn dispatch_tool_types_unparseable_args_as_argument_invalid() {
    use gents_loop::tool::{Tool, ToolDefinition};

    struct StrictArgsTool {
        calls: Arc<AtomicUsize>,
    }
    #[derive(Debug, thiserror::Error)]
    #[error("strict tool error")]
    struct StrictToolError;
    #[derive(serde::Deserialize)]
    struct StrictArgs {
        #[allow(dead_code)]
        text: String,
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
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ran".to_string())
        }
    }

    #[derive(serde::Deserialize)]
    struct Case {
        termination: String,
        arguments: String,
        kind: String,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/tool_argument_failure.json"
    )))
    .unwrap();
    let cases: Vec<_> = cases
        .into_iter()
        .filter(|case| case.termination == "unreported" && case.kind == "malformed")
        .collect();
    assert!(!cases.is_empty());
    let calls = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictArgsTool {
        calls: calls.clone(),
    })];
    for case in cases {
        let result = super::dispatch_tool(&tools, "strict", case.arguments, None, None).await;
        match &result {
            gents_loop::tool_call_lifecycle::ToolOutcome::Failed {
                class,
                denial: None,
                text,
            } => {
                assert_eq!(
                    *class,
                    gents_loop::tool_call_lifecycle::FailureClass::ArgumentInvalid
                );
                assert!(
                    text.contains(&format!("arguments were rejected ({})", case.kind)),
                    "{text}"
                );
                assert!(
                    !text.contains("ran") && !text.contains("token limit"),
                    "{text}"
                );
            }
            other => panic!("unparseable args must classify ArgumentInvalid, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn missing_tool_args_notify_model_and_terminalize_failed() {
    use gents_loop::tool::{Tool, ToolDefinition};

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
            panic!("the tool must not run on unparseable arguments");
        }
    }

    let (_node, hook, writer, mut lifecycle) = owned_test_hook().await;

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

    let resp = agent_response(&lifecycle.access().unwrap(), "query { AgentToolCall { tool_name lifecycle_state tool_failure_class } }").await;
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
    let (result, scans) = gents_session::session::count_request_output_scans(lifecycle.terminalize_owned(
        gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
        selection,
        None,
    ))
    .await;
    assert_eq!(
        result.unwrap(),
        gents_session::request::lifecycle::TerminalizeResult::Won
    );
    assert_eq!(scans, 1, "request output scanned once per accepted tool");
    node.shutdown().await.expect("node shutdown");
}

async fn terminalization_with_principal_output(
    turns: usize,
    output_bytes: usize,
    noise: usize,
) -> std::time::Duration {
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
    let finished_at = chrono::Utc::now();
    let generation = lifecycle.execution_generation().unwrap().to_owned();
    let request_doc_id = lifecycle.request().doc_id.clone();
    let segments = agent_response(&lifecycle.access().unwrap(), &format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {} }} }}"#,
            gents_protocol::output::rows::AGENT_OUTPUT_SEGMENT_FIELDS
        )).await;
    let segments = segments.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| gents_protocol::output::rows::decode_output_segment_row(row).unwrap())
        .collect::<Vec<_>>();
    let messages = agent_response(&lifecycle.access().unwrap(), &format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {} }} }}"#,
            gents_protocol::output::rows::AGENT_MESSAGE_FIELDS
        )).await;
    let messages = messages.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| gents_protocol::output::rows::decode_transcript_message_row(row).unwrap())
        .collect::<Vec<_>>();
    for replica in 0..noise {
        let other_request = format!("bae-noise-request-{replica}");
        let other_session = format!("noise-session-{replica}");
        for row in &segments {
            let mut segment = row.segment.clone();
            segment.request_doc_id = other_request.clone();
            segment.session_id = other_session.clone();
            let response = node
                .execute_request(
                    defra_node::QueryRequest::new(
                        gents_protocol::output::rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                    )
                    .with_variables(
                        gents_protocol::output::rows::output_segment_create_variables(&segment)
                            .unwrap(),
                    ),
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
                .execute_request(
                    defra_node::QueryRequest::new(
                        gents_protocol::output::rows::CREATE_AGENT_MESSAGE_MUTATION,
                    )
                    .with_variables(
                        gents_protocol::output::rows::transcript_message_create_variables(&message)
                            .unwrap(),
                    ),
                )
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
    }
    let selection = writer.terminal_output(&request_doc_id).await;
    let started = std::time::Instant::now();
    let result = gents_session::request::lifecycle::terminalize_owned_at_with_access(
        &lifecycle.access().unwrap(),
        &request_doc_id,
        &generation,
        gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
        selection,
        finished_at,
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(
        result.unwrap(),
        gents_session::request::lifecycle::TerminalizeResult::Won
    );
    drop(lifecycle);
    eprintln!(
        "terminalization turns={turns} bytes={output_bytes} noise={noise} segments={} messages={} terminalize_ms={}",
        segments.len(),
        messages.len(),
        elapsed.as_millis()
    );
    node.shutdown().await.expect("node shutdown");
    elapsed
}

#[tokio::test]
#[ignore = "manual terminalization scale measurement"]
async fn terminalization_scale_grid() {
    let grid = std::env::var("TERMINALIZE_SCALE").unwrap_or_else(|_| "24:4096:0,24:4096:40".into());
    for point in grid.split(',') {
        let values = point
            .split(':')
            .map(|v| v.parse::<usize>().unwrap())
            .collect::<Vec<_>>();
        terminalization_with_principal_output(values[0], values[1], values[2]).await;
    }
}

#[tokio::test]
async fn background_output_query_work_is_independent_of_principal_history() {
    use gents_protocol::output::rows::{
        output_segment_create_variables, request_output_segments_query,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{OutputSource, OutputWriter};
    let (node, _hook, _writer, lifecycle) = owned_test_hook().await;
    let request_id = lifecycle.request().doc_id.clone();
    let template = gents_protocol::output::OutputSegment {
        node_did: lifecycle.request().node_did.clone(),
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
        node: &defra_node::Node,
        segment: &gents_protocol::output::OutputSegment,
    ) {
        let response = node
            .execute_request(
                defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(segment).unwrap()),
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
        let response = gents_store::graphql::graphql_with_transaction_retry(
            &node,
            &format!("query @explain(type: execute) {query}"),
            "explain request output",
        )
        .await
        .unwrap();
        let data = response.data.unwrap();
        let request_scan = scan(&data, "indexFetches")
            .unwrap_or_else(|| panic!("request output must report index execution work: {data}"));
        let plan = gents_store::graphql::graphql_with_transaction_retry(
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
        let output = gents_session::session::tool_output_rows::canonical_tool_output(
            &node,
            "tool-doc",
            &request_id,
            "session",
            &lifecycle.request().node_did,
            None,
        )
        .await
        .unwrap();
        assert_eq!(output, "output");
        assert_eq!(
            gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
                &gents_store::config_client::ConfigAccess::local(node.clone()),
                "tool-doc",
                &request_id,
                "session",
                &lifecycle.request().node_did,
                None,
            )
            .await
            .unwrap(),
            gents_session::session::tool_output_rows::CanonicalToolOutputObservation::Closed(output)
        );
    }
    assert_eq!(
        work[0], work[1],
        "request query work grew with unrelated history"
    );
    assert!(work[0] > 0);
    drop(lifecycle);
    node.shutdown().await.expect("node shutdown");
}

struct ComposedChild {
    name: &'static str,
}

impl ToolDyn for ComposedChild {
    fn name(&self) -> String {
        self.name.to_owned()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.to_owned(),
                description: "composed child".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            if self.name == "slow" {
                tokio::time::sleep(
                    gents_pack::plugin::PluginBudget::default().wall_clock + Duration::from_secs(1),
                )
                .await;
                return Ok("\"slow done\"".into());
            }
            let local =
                tokio::time::timeout(Duration::from_millis(50), std::future::pending::<()>()).await;
            assert!(local.is_err());
            Err(ToolError::ReportedFailure {
                class: gents_loop::tool_call_lifecycle::FailureClass::External,
                text: "late timed out after 50ms".into(),
            })
        })
    }
}

#[tokio::test]
async fn composed_children_outlast_the_guest_clock_and_fail_without_ending_the_parent() {
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let request = serde_json::json!({"tool_calls": {"requests": [
        {"id": "slow", "tool_name": "slow", "arguments": {}},
        {"id": "late", "tool_name": "late", "arguments": {}}]}});
    let (home, record) = gents_toolcalls::test_support::plugin::installed_plugin(
        &gents_toolcalls::test_support::plugin::asking_plugin_wat(
            "tool_calls",
            "tool_results",
            &request,
            false,
        ),
        None,
    );
    let plugin = gents_toolcalls::plugin::tool::PluginTool::resolve(
        Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        ))),
        &gents_documents::PluginToolRef {
            tool_calls: true,
            plugin: "team/plugin".into(),
            digest: Some(record.digest.clone()),
            input_fields: Vec::new(),
            job: None,
        },
        None,
    )
    .unwrap();
    let tools: Vec<Box<dyn ToolDyn>> = vec![
        Box::new(plugin),
        Box::new(ComposedChild { name: "slow" }),
        Box::new(ComposedChild { name: "late" }),
    ];
    let stream = run_loop_stream(
        ScriptedModel::new_turns(plugin_call_then_text_turns()),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("compose")),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );
    let collected = gents_tools::runtime_context::scope_request_tool_execution(
        Some(chrono::Utc::now() + chrono::Duration::seconds(60)),
        tokio_util::sync::CancellationToken::new(),
        Box::pin(collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )),
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.final_text.as_deref(), Some("seen"));
    let output: serde_json::Value = serde_json::from_str(&collected.tool_results[0]).unwrap();
    assert_eq!(output["tool_results"]["slow"]["result"], "slow done", "{output}");
    assert_eq!(
        output["tool_results"]["late"]["error"],
        "late timed out after 50ms"
    );
    let rows = agent_response(&lifecycle.access().unwrap(), "query { AgentToolCall { tool_name lifecycle_state } }").await;
    let rows = rows.data.unwrap()["AgentToolCall"].clone();
    for (name, state) in [
        ("plugin", "completed"),
        ("slow", "completed"),
        ("late", "failed"),
    ] {
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .any(|row| row["tool_name"] == name && row["lifecycle_state"] == state),
            "{name} {state}: {rows}"
        );
    }
    node.shutdown().await.expect("node shutdown");
}

struct ComposingParent {
    children: Vec<(u32, &'static str, String)>,
    completed: Arc<std::sync::Mutex<Vec<(u32, bool)>>>,
}

/// Stand in a child's arguments for the `tool_call_id` the `spawn_process`
/// child returned and the `session_id` the `agent_new` child returned.
const SPAWNED_PROCESS: &str = "$spawned_process";
const NEW_SESSION: &str = "$new_session";

impl ToolDyn for ComposingParent {
    fn name(&self) -> String {
        "plugin".into()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: "plugin".into(),
                description: "composing parent".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let dispatcher = gents_loop::tool_effects::current().expect("live dispatcher");
            let mut spawned = String::new();
            let mut session = String::new();
            for (ordinal, name, arguments) in &self.children {
                let arguments = arguments
                    .replace(SPAWNED_PROCESS, &spawned)
                    .replace(NEW_SESSION, &session);
                let outcome = dispatcher.call(*ordinal, name, &arguments).await;
                assert!(
                    !matches!(
                        outcome,
                        Err(gents_loop::tool_effects::ToolEffectError::Fatal(_))
                    ),
                    "{name} #{ordinal} ended the parent: {outcome:?}"
                );
                if let Ok(gents_loop::tool_call_lifecycle::ToolOutcome::Completed(receipt)) = &outcome
                {
                    let receipt: serde_json::Value =
                        serde_json::from_str(receipt).unwrap_or_default();
                    let field = |name: &str| receipt[name].as_str().map(str::to_owned);
                    if *name == gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME {
                        spawned = field("tool_call_id").expect("spawn receipt names its process");
                    } else if *name == gents_protocol::tool_names::AGENT_NEW_TOOL_NAME {
                        session = field("session_id").expect("agent_new receipt names its session");
                    }
                }
                let completed = matches!(
                    outcome,
                    Ok(gents_loop::tool_call_lifecycle::ToolOutcome::Completed(_))
                );
                self.completed.lock().unwrap().push((*ordinal, completed));
            }
            Ok("\"composed\"".into())
        })
    }
}

struct AdmissionChild {
    name: &'static str,
    invoked: Arc<std::sync::Mutex<Vec<u64>>>,
}

impl ToolDyn for AdmissionChild {
    fn name(&self) -> String {
        self.name.to_owned()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.to_owned(),
                description: "admission child".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn admit(&self, args: &str) -> Result<(), ToolError> {
        let args: serde_json::Value = serde_json::from_str(args).unwrap();
        if args.get("admit").is_none_or(|value| value == true) {
            return Ok(());
        }
        Err(ToolError::ReportedFailure {
            class: gents_loop::tool_call_lifecycle::FailureClass::PolicyDenied,
            text: "admission refused".into(),
        })
    }

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let args: serde_json::Value = serde_json::from_str(&args).unwrap();
            self.invoked
                .lock()
                .unwrap()
                .push(args["ordinal"].as_u64().unwrap());
            Ok("\"ran\"".into())
        })
    }
}

/// The process the `spawn_process` route starts: any ordinal no composed
/// child uses, so its run is not read as a child's own invocation.
const SPAWNED_ORDINAL: u64 = u64::MAX;

/// Every `HookOwnedTool::ROUTES` entry as a composed child its owner admits
/// and settles: the arguments it is called with and the durable state its row
/// reaches. The goal routes run in the order that gives `update_goal` a goal,
/// the process routes after the spawn whose handle they name, and
/// `agent_message` after the `agent_new` whose session it addresses.
fn hook_owned_children() -> Vec<(&'static str, serde_json::Value, &'static str)> {
    use gents_protocol::tool_names as names;
    let process = serde_json::json!({"tool_call_id": SPAWNED_PROCESS});
    vec![
        (gents_protocol::goal_names::GET_GOAL_TOOL_NAME, serde_json::json!({}), "completed"),
        (
            gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME,
            serde_json::json!({"objective": "drive every route"}),
            "completed",
        ),
        (
            gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
            serde_json::json!({"objective": "drive every hook-owned route"}),
            "completed",
        ),
        (names::AGENT_LIST_TOOL_NAME, serde_json::json!({}), "completed"),
        (names::AGENT_SESSION_LIST_TOOL_NAME, serde_json::json!({}), "completed"),
        (names::AGENT_SESSION_READ_TOOL_NAME, serde_json::json!({"session_id": "absent"}), "completed"),
        (names::AGENT_INTERRUPT_TOOL_NAME, serde_json::json!({"session_id": "absent"}), "completed"),
        (names::AGENT_STOP_TOOL_NAME, serde_json::json!({"session_id": "absent"}), "completed"),
        (names::AGENT_RESUME_TOOL_NAME, serde_json::json!({"session_id": "absent"}), "completed"),
        (names::AGENT_NEW_TOOL_NAME, serde_json::json!({"agent": "general", "prompt": "work"}), "running"),
        (names::AGENT_MESSAGE_TOOL_NAME, serde_json::json!({"session_id": NEW_SESSION, "message": "more"}), "running"),
        (
            names::SPAWN_PROCESS_TOOL_NAME,
            serde_json::json!({"tool_name": "child", "args": {"ordinal": SPAWNED_ORDINAL}}),
            "completed",
        ),
        (names::LIST_PROCESSES_TOOL_NAME, serde_json::json!({}), "completed"),
        (names::WAIT_PROCESS_TOOL_NAME, process.clone(), "completed"),
        (names::READ_PROCESS_TOOL_NAME, process.clone(), "completed"),
        (names::CANCEL_PROCESS_TOOL_NAME, process, "completed"),
    ]
}

/// Selects the request's own Agent as the `general` target of its Tools, so
/// the session-message routes have an agent they may start and address.
async fn allow_general_agent_target(
    node: &Arc<defra_node::Node>,
    request: &gents_protocol::row::AgentRequest,
) {
    let node_did = request.node_did.as_str();
    let agent_did = request.agent_did.as_str();
    let documents = [
        (
            gents_store::collection::Collection::AgentTarget,
            serde_json::to_value(gents_documents::AgentTargetDocument {
                target_id: "general:general".to_owned(),
                node_did: node_did.to_owned(),
                target_node_did: node_did.to_owned(),
                agent_did: agent_did.to_owned(),
                name: "general".to_owned(),
                description: None,
                tags: Vec::new(),
            })
            .unwrap(),
        ),
        (
            gents_store::collection::Collection::Tools,
            serde_json::json!({
                "node_did": node_did,
                "tools_id": format!("{agent_did}:tools"),
                "agents": { "enabled": true, "target_ids": ["general:general"] }
            }),
        ),
    ];
    let plan = gents_documents::DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| gents_documents::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    let actor = node.node_identity_did().expect("fixture node has an identity");
    gents_store::config_client::ConfigAccess::local_as(node.clone(), ::identity::Did::new(actor).unwrap())
        .transact_write("test.allow_general_agent_target", |txn| {
            let plan = &plan;
            Box::pin(async move { gents_documents::apply_desired_state_plan(txn, plan, &gents_inference::resolved::ProviderProfilePolicy).await })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn generated_child_admission_cases_drive_real_dispatcher() {
    let cases = gents_lean_vocab::lean_contract_snapshot().plugin_resource_cases
        ["child_admission"]
        .as_array()
        .unwrap();
    let hook_owned = hook_owned_children();
    let mut driven: Vec<&str> = hook_owned.iter().map(|(name, ..)| *name).collect();
    let mut routes: Vec<&str> = gents_toolcalls::call::hook::persistence::HookOwnedTool::ROUTES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    driven.sort_unstable();
    routes.sort_unstable();
    assert_eq!(driven, routes, "every hook-owned route is driven exactly once");
    for selected in [false, true] {
        let mut children = Vec::new();
        let mut expected = std::collections::BTreeMap::new();
        for case in cases.iter().filter(|case| case["selected"] == selected) {
            let admitted = case["admitted"] == true;
            let refused = |ordinal: u32| serde_json::json!({"admit": false, "ordinal": ordinal});
            let calls: Vec<(&'static str, serde_json::Value, &'static str)> =
                match case["route"].as_str().unwrap() {
                    "ordinary" => vec![("child", serde_json::json!({"admit": admitted}), "completed")],
                    "hook_owned" => hook_owned.clone(),
                    route => panic!("unknown route {route}"),
                };
            for (name, mut arguments, settled) in calls {
                let ordinal = children.len() as u32 + 1;
                if !admitted {
                    arguments = refused(ordinal);
                } else if name == "child" {
                    arguments["ordinal"] = ordinal.into();
                }
                children.push((ordinal, name, arguments.to_string()));
                expected.insert(ordinal, (case.clone(), name, case["expected"] == true, settled));
            }
        }
        let completed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let invoked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(ComposingParent {
            children,
            completed: completed.clone(),
        })];
        if selected {
            for name in std::iter::once("child").chain(hook_owned.iter().map(|(name, ..)| *name)) {
                tools.push(Box::new(AdmissionChild {
                    name,
                    invoked: invoked.clone(),
                }));
            }
        }
        let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
        let hook = hook
            .with_goal_tool_authority(true, true)
            .with_background_tool_registry(
                gents_toolcalls::call::hook::BackgroundToolRegistry::from_tools(
                    vec![Box::new(AdmissionChild {
                        name: "child",
                        invoked: invoked.clone(),
                    })],
                    &["child".into()],
                ),
            );
        allow_general_agent_target(&node, lifecycle.request()).await;
        let stream = run_loop_stream(
            ScriptedModel::new_turns(plugin_call_then_text_turns()),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("compose")),
            Vec::new(),
            Arc::new(tools),
            owned_config(4),
        );
        let collected = gents_tools::runtime_context::scope_request_tool_execution(
            Some(chrono::Utc::now() + chrono::Duration::seconds(60)),
            tokio_util::sync::CancellationToken::new(),
            Box::pin(collect_owned_scripted_stream(
                stream,
                &hook,
                &writer,
                &mut lifecycle,
                gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
            )),
        )
        .await;
        assert!(collected.error.is_none(), "{:?}", collected.error);
        let completed = completed.lock().unwrap().clone();
        let invoked = invoked.lock().unwrap().clone();
        assert_eq!(completed.len(), expected.len());
        let rows = lifecycle.access().unwrap()
            .execute("query { AgentToolCall { plugin_effect_ordinal lifecycle_state } }")
            .await
            .unwrap();
        let states: std::collections::BTreeMap<u64, String> = rows["data"]["AgentToolCall"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| {
                Some((
                    row["plugin_effect_ordinal"].as_u64()?,
                    row["lifecycle_state"].as_str()?.to_owned(),
                ))
            })
            .collect();
        let mut mismatches = Vec::new();
        for (ordinal, observed) in completed {
            let (case, name, admissible, settled) = &expected[&ordinal];
            if observed != *admissible {
                mismatches.push(format!("dispatcher result {observed} for {name} {case}"));
            }
            if invoked.contains(&u64::from(ordinal))
                != (*admissible && case["route"] == "ordinary")
            {
                mismatches.push(format!("invocation of {name} {case}"));
            }
            let state = &states[&u64::from(ordinal)];
            if state != if *admissible { *settled } else { "failed" } {
                mismatches.push(format!("durable child {name} is {state} {case}"));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
        if selected {
            assert!(
                invoked.contains(&SPAWNED_ORDINAL),
                "spawn_process ran the process it admitted"
            );
        }
        node.shutdown().await.expect("node shutdown");
    }
}

struct StoppingParent;

impl ToolDyn for StoppingParent {
    fn name(&self) -> String {
        "plugin".into()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: "plugin".into(),
                description: "stops while its child is in flight".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let dispatcher = gents_loop::tool_effects::current().expect("live dispatcher");
            // A script that exhausts its own allowance drops the child call it awaits.
            let stopped = tokio::time::timeout(
                std::time::Duration::from_millis(200),
                dispatcher.call(1, "stall", "{}"),
            )
            .await;
            assert!(stopped.is_err(), "the child outlives the script");
            Err(ToolError::ReportedFailure {
                class: gents_loop::tool_call_lifecycle::FailureClass::External,
                text: "the plugin exceeded its wall-clock budget".into(),
            })
        })
    }
}

struct Stall;

impl ToolDyn for Stall {
    fn name(&self) -> String {
        "stall".into()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: "stall".into(),
                description: "never answers".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn a_script_that_stops_mid_child_settles_the_child_and_the_request_finishes() {
    let (node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StoppingParent), Box::new(Stall)];
    let stream = run_loop_stream(
        ScriptedModel::new_turns(plugin_call_then_text_turns()),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("compose")),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );
    let collected = gents_tools::runtime_context::scope_request_tool_execution(
        Some(chrono::Utc::now() + chrono::Duration::seconds(60)),
        tokio_util::sync::CancellationToken::new(),
        Box::pin(collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )),
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.final_text.as_deref(), Some("seen"));
    let rows = lifecycle
        .access()
        .unwrap()
        .execute("query { AgentToolCall { tool_name lifecycle_state cancel_cause } }")
        .await
        .unwrap();
    let rows = rows["data"]["AgentToolCall"].as_array().unwrap().clone();
    let state = |name: &str| {
        rows.iter()
            .find(|row| row["tool_name"] == name)
            .map(|row| row["lifecycle_state"].as_str().unwrap().to_owned())
    };
    assert_eq!(state("stall").as_deref(), Some("cancelled"), "{rows:?}");
    assert_eq!(state("plugin").as_deref(), Some("failed"), "{rows:?}");
    // With the child still running, normal completion is refused with ForegroundRunning.
    let selection = writer.terminal_output(&lifecycle.request().doc_id).await;
    lifecycle
        .terminalize_owned(
            gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
            selection,
            None,
        )
        .await
        .expect("the request completes once its script's child is settled");
    node.shutdown().await.expect("node shutdown");
}

struct ParallelReadProbe {
    gate: Arc<tokio::sync::Barrier>,
    second_finished: Arc<tokio::sync::Notify>,
    fail_first: bool,
}

impl ToolDyn for ParallelReadProbe {
    fn name(&self) -> String {
        "parallel_read_probe".into()
    }
    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "parallel_read_probe".into(),
                description: String::new(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }
    fn footprint(&self, _: &str) -> gents_loop::tool::ToolFootprint {
        gents_loop::tool::ToolFootprint::Read
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            self.gate.wait().await;
            if args.contains("first") {
                self.second_finished.notified().await;
                if self.fail_first {
                    return Err(ToolError::ReportedFailure {
                        class: gents_loop::tool_call_lifecycle::FailureClass::External,
                        text: "first read failed".into(),
                    });
                }
                Ok("first result".into())
            } else {
                self.second_finished.notify_one();
                Ok("second result".into())
            }
        })
    }
}

#[tokio::test]
async fn independent_reads_overlap_and_results_keep_call_order() {
    for fail_first in [false, true] {
        assert_overlapping_reads_keep_call_order(fail_first).await;
    }
}

async fn assert_overlapping_reads_keep_call_order(fail_first: bool) {
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "first".into(),
                "parallel_read_probe".into(),
                serde_json::json!({"value":"first"}),
            )),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "second".into(),
                "parallel_read_probe".into(),
                serde_json::json!({"value":"second"}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(ParallelReadProbe {
        gate: Arc::new(tokio::sync::Barrier::new(2)),
        second_finished: Arc::new(tokio::sync::Notify::new()),
        fail_first,
    })];
    let stream = run_loop_stream(
        model.clone(),
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("read")),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(item) = stream.next().await {
            item.expect("loop succeeds");
        }
    })
    .await
    .expect("both calls must reach the gate");
    let histories = model.seen_histories().await;
    let results: Vec<_> = histories[1]
        .iter()
        .flat_map(|message| match message {
            Message::User { content } => content
                .iter()
                .filter_map(|part| match part {
                    UserContent::ToolResult(result) => Some(result.content.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(results.len(), 2);
    if fail_first {
        let failure = serde_json::to_string(&results[0]).unwrap();
        assert!(failure.contains("first read failed"), "{failure}");
    } else {
        assert_eq!(
            results[0],
            ToolResultContent::from_tool_output("first result")
        );
    }
    assert_eq!(
        results[1],
        ToolResultContent::from_tool_output("second result")
    );
}

#[tokio::test]
async fn parallel_native_reads_publish_distinct_bound_unchanged_receipts() {
    let identity_dir = tempfile::tempdir().unwrap();
    let identity: Arc<dyn gents_store::identity::NodeIdentity> = Arc::new(
        gents_store::identity::KeyIdentity::load_or_create(identity_dir.path().join("native-read.key"), None)
            .unwrap(),
    );
    let (node, hook, writer, mut lifecycle) = owned_test_hook_with_identity(identity.clone()).await;
    assert_eq!(node.node_identity_did(), Some(identity.did()));
    assert_eq!(lifecycle.request().node_did, identity.did());
    let root = tempfile::tempdir().unwrap();
    tokio::fs::write(root.path().join("a.txt"), "alpha")
        .await
        .unwrap();
    tokio::fs::write(root.path().join("b.txt"), "beta")
        .await
        .unwrap();
    let tools = gents_tools::native::ToolSet::builder()
        .read_root(root.path())
        .read_file()
        .build()
        .build_native_tools()
        .unwrap();
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "read-a".into(),
                "read_file".into(),
                serde_json::json!({"path":"a.txt"}),
            )),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "read-b".into(),
                "read_file".into(),
                serde_json::json!({"path":"b.txt"}),
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
        TaggedMessage::unassociated(Message::user("read both")),
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
    let rows = lifecycle.access().unwrap().execute("{ AgentToolCall { _docID tool_name lifecycle_state } ToolFileInventory { tool_call_doc_id request_doc_id verified_unchanged } }").await.unwrap();
    let calls: Vec<_> = rows["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["tool_name"] == "read_file")
        .collect();
    let receipts = rows["data"]["ToolFileInventory"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(call["lifecycle_state"], "completed");
        let matches: Vec<_> = receipts
            .iter()
            .filter(|receipt| receipt["tool_call_doc_id"] == call["_docID"])
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["request_doc_id"], lifecycle.request().doc_id);
        assert_eq!(matches[0]["verified_unchanged"], true);
    }
    node.shutdown().await.expect("node shutdown");
}

struct SerialReadMeasurement(Box<dyn ToolDyn>);
impl ToolDyn for SerialReadMeasurement {
    fn name(&self) -> String {
        self.0.name()
    }
    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition> {
        self.0.definition(prompt)
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        self.0.call(args)
    }
    fn call_with_receipt<'a>(
        &'a self,
        args: String,
    ) -> BoxFuture<'a, gents_loop::tool::ToolDispatchResult> {
        self.0.call_with_receipt(args)
    }
    fn admit(&self, args: &str) -> Result<(), ToolError> {
        self.0.admit(args)
    }
}

#[tokio::test]
#[ignore = "manual matched multi-read timing"]
async fn measure_parallel_native_read_turns() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let root = tempfile::tempdir().unwrap();
    for index in 0..8 {
        tokio::fs::write(
            root.path().join(format!("{index}.txt")),
            vec![b'x'; 1024 * 1024],
        )
        .await
        .unwrap();
    }
    let mut totals = [Duration::ZERO; 2];
    for round in 0..10 {
        for parallel in [round % 2 == 0, round % 2 != 0] {
            let native = gents_tools::native::ToolSet::builder()
                .read_root(root.path())
                .read_file()
                .build()
                .build_native_tools()
                .unwrap()
                .pop()
                .unwrap();
            let tool: Box<dyn ToolDyn> = if parallel {
                native
            } else {
                Box::new(SerialReadMeasurement(native))
            };
            let mut turn: Vec<_> = (0..8)
                .map(|index| {
                    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                        format!("call-{index}"),
                        "read_file".into(),
                        serde_json::json!({"path":format!("{index}.txt"), "max_chars":256}),
                    ))
                })
                .collect();
            turn.push(RawStreamingChoice::FinalResponse(()));
            let model = ScriptedModel::new_turns(vec![
                turn,
                vec![
                    RawStreamingChoice::Message("done".into()),
                    RawStreamingChoice::FinalResponse(()),
                ],
            ])
            .without_capture();
            let started = std::time::Instant::now();
            let stream = run_loop_stream(
                model,
                None::<gents_loop::session_hook::NoopSessionHook>,
                TaggedMessage::unassociated(Message::user("read")),
                Vec::new(),
                Arc::new(vec![tool]),
                config(4),
            );
            futures::pin_mut!(stream);
            while let Some(item) = stream.next().await {
                item.expect("measurement turn succeeds");
            }
            totals[usize::from(parallel)] += started.elapsed();
        }
    }
    tracing::info!(revision = %option_env!("GENTS_BENCH_REVISION").unwrap_or("unspecified"), calls_per_turn = 8, bytes_per_file = 1024 * 1024, turns_per_mode = 10, serial_us = u64::try_from(totals[0].as_micros()).unwrap_or(u64::MAX), parallel_us = u64::try_from(totals[1].as_micros()).unwrap_or(u64::MAX), "matched native read turn measurement");
}