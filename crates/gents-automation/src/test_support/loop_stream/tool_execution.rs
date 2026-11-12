use super::*;

pub enum PolicyDeniedOwner {
    ReadOnlyBash,
    CliArgvPrefix,
    McpAllowlist,
}

pub struct PolicyDeniedCall {
    pub tool_name: &'static str,
    pub arguments: serde_json::Value,
    pub tools: Vec<Box<dyn ToolDyn>>,
}

impl PolicyDeniedOwner {
    pub const ALL: [PolicyDeniedOwner; 3] = [
        PolicyDeniedOwner::ReadOnlyBash,
        PolicyDeniedOwner::CliArgvPrefix,
        PolicyDeniedOwner::McpAllowlist,
    ];

    pub fn owner(&self) -> &'static str {
        match self {
            Self::ReadOnlyBash => "read_only_bash",
            Self::CliArgvPrefix => "cli_argv_prefix",
            Self::McpAllowlist => "mcp_allowlist",
        }
    }

    pub fn denied_call(
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
                        actor: identity::Did::new(crate::triggers::callback::test_agent_did())
                            .unwrap(),
                        node: node.clone(),
                        mcp_pool: gents_tools::mcp::pool::McpPool::new().for_agent(
                            node.node_identity_did()
                                .expect("fixture node has an identity"),
                        ),
                        health: gents_tools::mcp::health::ServiceHealthMap::new(),
                        local_hostname: "local".into(),
                        local_subnet: None,
                        node_did: node
                            .node_identity_did()
                            .expect("fixture node has an identity")
                            .into(),
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

pub fn plugin_image_tool(image_bytes: usize) -> (tempfile::TempDir, Box<dyn ToolDyn>) {
    const PREFIX: &[u8] = br#"{"response":{"width":2,"height":2},"parts":[{"type":"image","mimeType":"image/png","data":""#;
    const SUFFIX: &[u8] = br#""}]}"#;
    let (home, record) = gents_pack::plugin::test_fixtures::installed_plugin(
        &gents_pack::plugin::test_fixtures::filled_output_wat(PREFIX, image_bytes, SUFFIX),
        None,
    );
    let executor = Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(
        Some(home.path().to_owned()),
    ));
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

pub fn plugin_call_then_text_turns() -> Vec<Vec<RawStreamingChoice<()>>> {
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

pub fn provider_input_counter(
    provider: gents_inference::backends::provider::BackendProviderKind,
    wire: gents_loop::openai_wire::OpenAiWireApi,
) -> Arc<gents_loop::provider_input::ProviderInputCounter> {
    Arc::new(gents_loop::provider_input::ProviderInputCounter::new(
        provider,
        wire,
        "test-model",
    ))
}

fn first_tool_result(history: &[Message]) -> Option<Vec<ToolResultContent>> {
    history.iter().find_map(|message| match message {
        Message::User { content } => content.iter().find_map(|part| match part {
            UserContent::ToolResult(result) => Some(result.content.clone()),
            _ => None,
        }),
        _ => None,
    })
}

pub async fn plugin_image_result_seen_by(
    counter: Arc<gents_loop::provider_input::ProviderInputCounter>,
    image_bytes: usize,
) -> Vec<ToolResultContent> {
    let (_home, tool) = plugin_image_tool(image_bytes);
    let tools: Vec<Box<dyn ToolDyn>> = vec![tool];
    let model = ScriptedModel::new_turns(plugin_call_then_text_turns()).without_capture();
    let mut config = config(4);
    config.provider_input_counter = counter;
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
            Err(error) => panic!("{error}"),
            Ok(LoopStreamItem::Final { text }) => final_text = Some(text),
            Ok(_) => {}
        }
    }
    assert_eq!(final_text.as_deref(), Some("seen"));
    let histories = model.seen_histories().await;
    first_tool_result(&histories[1]).expect("the second request carries the plugin's tool result")
}

pub async fn owned_tool_result_threaded(
    counter: Arc<gents_loop::provider_input::ProviderInputCounter>,
    tool: Box<dyn ToolDyn>,
) -> Vec<ToolResultContent> {
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let profile = counter.profile();
    let mut config = owned_config(4);
    config.provider_input_counter = counter;
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("read the big thing")),
        Vec::new(),
        Arc::new(vec![tool]),
        config,
    );
    let collected =
        collect_owned_scripted_stream(stream, &hook, &writer, &mut lifecycle, profile).await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.final_text.as_deref(), Some("ok"));
    let histories = model.seen_histories().await;
    node.shutdown().await.expect("node shutdown");
    first_tool_result(&histories[1]).expect("the second request carries the tool result")
}

pub async fn persisted_plugin_image_result_replay(
    counter: Arc<gents_loop::provider_input::ProviderInputCounter>,
    image_bytes: usize,
) -> Vec<ToolResultContent> {
    let (_home, tool) = plugin_image_tool(image_bytes);
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let profile = counter.profile();
    let mut config = owned_config(4);
    config.provider_input_counter = counter;
    let stream = run_loop_stream(
        ScriptedModel::new_turns(plugin_call_then_text_turns()),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("draw it")),
        Vec::new(),
        Arc::new(vec![tool]),
        config,
    );
    let collected =
        collect_owned_scripted_stream(stream, &hook, &writer, &mut lifecycle, profile).await;
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
    node.shutdown().await.expect("node shutdown");
    history
        .iter()
        .flat_map(|message| match message {
            Message::User { content } => content.as_slice(),
            _ => &[],
        })
        .filter_map(|part| match part {
            UserContent::ToolResult(result) => Some(result.content.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

pub async fn terminalization_with_principal_output(
    turns: usize,
    output_bytes: usize,
    noise: usize,
) -> std::time::Duration {
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
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

pub struct ComposedChild {
    pub name: &'static str,
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

pub struct ComposingParent {
    pub children: Vec<(u32, &'static str, String)>,
    pub completed: Arc<std::sync::Mutex<Vec<(u32, bool)>>>,
}

pub const SPAWNED_PROCESS: &str = "$spawned_process";
pub const NEW_SESSION: &str = "$new_session";

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
                if let Ok(gents_loop::tool_call_lifecycle::ToolOutcome::Completed(receipt)) =
                    &outcome
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

pub struct AdmissionChild {
    pub name: &'static str,
    pub invoked: Arc<std::sync::Mutex<Vec<u64>>>,
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

pub const SPAWNED_ORDINAL: u64 = u64::MAX;

pub fn hook_owned_children() -> Vec<(&'static str, serde_json::Value, &'static str)> {
    use gents_protocol::tool_names as names;
    let process = serde_json::json!({"tool_call_id": SPAWNED_PROCESS});
    vec![
        (
            gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
            serde_json::json!({}),
            "completed",
        ),
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
        (
            names::AGENT_LIST_TOOL_NAME,
            serde_json::json!({}),
            "completed",
        ),
        (
            names::AGENT_SESSION_LIST_TOOL_NAME,
            serde_json::json!({}),
            "completed",
        ),
        (
            names::AGENT_SESSION_READ_TOOL_NAME,
            serde_json::json!({"session_id": "absent"}),
            "completed",
        ),
        (
            names::AGENT_INTERRUPT_TOOL_NAME,
            serde_json::json!({"session_id": "absent"}),
            "completed",
        ),
        (
            names::AGENT_STOP_TOOL_NAME,
            serde_json::json!({"session_id": "absent"}),
            "completed",
        ),
        (
            names::AGENT_RESUME_TOOL_NAME,
            serde_json::json!({"session_id": "absent"}),
            "completed",
        ),
        (
            names::AGENT_NEW_TOOL_NAME,
            serde_json::json!({"agent": "general", "prompt": "work"}),
            "running",
        ),
        (
            names::AGENT_MESSAGE_TOOL_NAME,
            serde_json::json!({"session_id": NEW_SESSION, "message": "more"}),
            "running",
        ),
        (
            names::SPAWN_PROCESS_TOOL_NAME,
            serde_json::json!({"tool_name": "child", "args": {"ordinal": SPAWNED_ORDINAL}}),
            "completed",
        ),
        (
            names::LIST_PROCESSES_TOOL_NAME,
            serde_json::json!({}),
            "completed",
        ),
        (names::WAIT_PROCESS_TOOL_NAME, process.clone(), "completed"),
        (names::READ_PROCESS_TOOL_NAME, process.clone(), "completed"),
        (names::CANCEL_PROCESS_TOOL_NAME, process, "completed"),
    ]
}

pub async fn allow_general_agent_target(
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
            .map(
                |(collection, value)| gents_documents::DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                },
            )
            .collect(),
    )
    .unwrap();
    let actor = node
        .node_identity_did()
        .expect("fixture node has an identity");
    gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        ::identity::Did::new(actor).unwrap(),
    )
    .transact_write("test.allow_general_agent_target", |txn| {
        let plan = &plan;
        Box::pin(async move {
            gents_documents::apply_desired_state_plan(
                txn,
                plan,
                &gents_inference::resolved::ProviderProfilePolicy,
            )
            .await
        })
    })
    .await
    .unwrap();
}

pub struct StoppingParent;

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

pub struct Stall;

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

pub struct ParallelReadProbe {
    pub gate: Arc<tokio::sync::Barrier>,
    pub second_finished: Arc<tokio::sync::Notify>,
    pub fail_first: bool,
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

pub async fn assert_overlapping_reads_keep_call_order(fail_first: bool) {
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

pub struct SerialReadMeasurement(pub Box<dyn ToolDyn>);
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
