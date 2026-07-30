use super::*;

pub(super) struct LiveCodexShim {
    pub(super) tempdir: tempfile::TempDir,
    pub(super) home_dir: std::path::PathBuf,
    pub(super) codex_home: std::path::PathBuf,
    pub(super) graphql: String,
    pub(super) agent_did: String,
    pub(super) behavior_id: String,
    tool_selection_id: String,
    pub(super) backend_id: String,
    pub(super) inference_profile_id: String,
    pub(super) model_name: String,
    pub(super) shim_port: u16,
    pub(super) shim_trace: std::path::PathBuf,
    pub(super) _server: ServeProcess,
}

pub(super) async fn start_live_codex_shim() -> Result<LiveCodexShim> {
    start_live_codex_shim_with_write_tools(false, None).await
}

pub(super) fn create_existing_client_codex_home(
    smoke: &LiveCodexShim,
    label: &str,
) -> Result<std::path::PathBuf> {
    let codex_home = smoke
        .tempdir
        .path()
        .join(format!("client-codex-home-{label}"));
    fs::create_dir_all(&codex_home)
        .with_context(|| format!("creating client Codex home {}", codex_home.display()))?;
    fs::write(
        codex_home.join("config.toml"),
        "# Existing user Codex config should remain client-side.\n",
    )
    .with_context(|| format!("writing client Codex config in {}", codex_home.display()))?;
    Ok(codex_home)
}

pub(super) async fn start_live_codex_shim_with_write_tools(
    write_tools: bool,
    tool_root: Option<&std::path::Path>,
) -> Result<LiveCodexShim> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-live-{}", Uuid::new_v4().simple());
    let tool_root_string = tool_root.map(|root| root.to_string_lossy().to_string());
    let model_endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME")
        .unwrap_or_else(|_| DEFAULT_MODEL_NAME.to_string());
    let mut init_args = vec![
        "--agent-name",
        &agent_name,
        "--model-name",
        model_name.as_str(),
        "--inference-url",
        model_endpoint.as_str(),
    ];
    if std::env::var_os("GENTS_CLI_E2E_API_KEY").is_some() {
        init_args.push("--api-key-env-var");
        init_args.push("GENTS_CLI_E2E_API_KEY");
    }
    if write_tools {
        init_args.push("--write-tools");
    }
    if let Some(tool_root) = &tool_root_string {
        init_args.push("--tool-root");
        init_args.push(tool_root.as_str());
    }
    let init = run_init_json(&home_dir, &init_args)?;
    let agent_did = agent_did_from_init(&init)?;
    let behavior_id = init_output_string(&init, "default_behavior_id")?;
    let tool_selection_id = init_output_string(&init, "tool_selection_id")?;
    let backend_id = init_output_string(&init, "backend_id")?;
    let inference_profile_id = init_output_string(&init, "inference_profile_id")?;
    let model_name = init_output_string(&init, "model_name")?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let codex_home = home_dir.join(".gents").join("codex-ui");
    let shim_trace = codex_home.join("log").join("codex-shim-events.jsonl");
    let mut server = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim",
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "250",
            "--codex-shim-timeout-secs",
            LIVE_CODEX_SHIM_TIMEOUT_SECS,
        ],
        &[("RUST_LOG", "error,gents_cli::commands::codex_shim=info")],
    )?;
    wait_for_port(server_port, &mut server)?;
    wait_for_port(shim_port, &mut server)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    Ok(LiveCodexShim {
        codex_home,
        tempdir,
        home_dir,
        graphql,
        agent_did,
        behavior_id,
        tool_selection_id,
        backend_id,
        inference_profile_id,
        model_name,
        shim_port,
        shim_trace,
        _server: server,
    })
}

fn init_output_string(init: &Value, key: &str) -> Result<String> {
    let nested = format!("/init/{key}");
    init.get(key)
        .or_else(|| init.pointer(&nested))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("init output missing {key}: {init}"))
}

pub(super) async fn configure_live_local_subagent(smoke: &LiveCodexShim) -> Result<String> {
    let child_behavior_id = format!("{}:codex-live-child", smoke.agent_did);
    let child_tool_selection_id = format!("{child_behavior_id}:tools");
    let parent_prompt_path = smoke.tempdir.path().join("parent-subagent-prompt.txt");
    let child_prompt_path = smoke.tempdir.path().join("child-subagent-prompt.txt");
    fs::write(
        &parent_prompt_path,
        "You are a coordinator. Follow explicit delegation instructions exactly. When the user \
         requests spawn_subagent, call it with the named target, prompt, and await_mode before \
         answering. Never replace a requested tool call with a textual simulation.",
    )?;
    fs::write(
        &child_prompt_path,
        "You are a leaf subagent. Never delegate or call tools. Follow the assigned prompt \
         directly and keep the answer exact and concise.",
    )?;

    run_cli_json(
        &smoke.home_dir,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &smoke.graphql,
            "--agent-did",
            &smoke.agent_did,
            "--selection-id",
            &child_tool_selection_id,
            "--display-name",
            "Codex Live Child Tools",
            "--clear-subagent-targets",
            "--subagent-spawn-enabled",
            "false",
            "--subagent-background-enabled",
            "false",
            "--subagent-steering-enabled",
            "false",
            "--subagent-allow-cross-deployment",
            "false",
            "--enable-file-tools",
            "false",
            "--enable-bash",
            "false",
            "--enable-meta-tools",
            "false",
            "--enable-memory",
            "false",
            "--enable-session-history-tool",
            "false",
            "--enable-context-budget",
            "false",
            "--enable-defra-query",
            "false",
        ],
    )?;
    run_cli_json(
        &smoke.home_dir,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &smoke.graphql,
            "--agent-did",
            &smoke.agent_did,
            "--behavior-id",
            &child_behavior_id,
            "--display-name",
            "Codex Live Child",
            "--system-prompt-file",
            child_prompt_path
                .to_str()
                .ok_or_else(|| anyhow!("child prompt path is not UTF-8"))?,
            "--backend-id",
            &smoke.backend_id,
            "--model-name",
            &smoke.model_name,
            "--tool-selection-id",
            &child_tool_selection_id,
            "--inference-profile-id",
            &smoke.inference_profile_id,
        ],
    )?;

    let child_target = subagent_target_entry(
        "codex-live-child",
        &smoke.agent_did,
        &child_behavior_id,
        Some("Live leaf child used by the Codex shim e2e".to_string()),
    );
    run_cli_json(
        &smoke.home_dir,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &smoke.graphql,
            "--agent-did",
            &smoke.agent_did,
            "--selection-id",
            &smoke.tool_selection_id,
            "--display-name",
            "Codex Live Coordinator Tools",
            "--subagent-target",
            &child_target,
            "--subagent-spawn-enabled",
            "true",
            "--subagent-background-enabled",
            "false",
            "--subagent-steering-enabled",
            "false",
            "--subagent-allow-cross-deployment",
            "false",
            "--enable-file-tools",
            "false",
            "--enable-bash",
            "false",
            "--enable-meta-tools",
            "false",
            "--enable-memory",
            "false",
            "--enable-session-history-tool",
            "false",
            "--enable-context-budget",
            "false",
            "--enable-defra-query",
            "false",
        ],
    )?;
    run_cli_json(
        &smoke.home_dir,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &smoke.graphql,
            "--agent-did",
            &smoke.agent_did,
            "--behavior-id",
            &smoke.behavior_id,
            "--display-name",
            "Codex Live Coordinator",
            "--system-prompt-file",
            parent_prompt_path
                .to_str()
                .ok_or_else(|| anyhow!("parent prompt path is not UTF-8"))?,
            "--backend-id",
            &smoke.backend_id,
            "--model-name",
            &smoke.model_name,
            "--tool-selection-id",
            &smoke.tool_selection_id,
            "--inference-profile-id",
            &smoke.inference_profile_id,
        ],
    )?;

    wait_for_runtime_quiescence(&smoke.graphql, &smoke.agent_did, 2, Duration::from_secs(2))
        .await?;
    Ok(child_behavior_id)
}

#[derive(Debug)]
pub(super) struct RealSpawnProjection {
    pub(super) parent_session_id: String,
    pub(super) child_session_id: String,
}

pub(super) async fn wait_for_real_spawn_projection(
    graphql: &str,
    parent_request_id: &str,
    child_agent_did: &str,
    child_behavior_id: &str,
    child_token: &str,
) -> Result<RealSpawnProjection> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        request_id: {{ _eq: "{parent_request_id}" }},
                        tool_name: {{ _eq: "spawn_subagent" }}
                    }},
                    order: {{ started_at: ASC }}
                ) {{
                    request_id
                    session_id
                    tool_call_id
                    child_request_id
                    spawn_target_did
                    await_mode
                    status
                    lifecycle_state
                }}
                AgentRequest(
                    filter: {{
                        caused_by_parent_request_id: {{ _eq: "{parent_request_id}" }}
                    }},
                    order: {{ created_at: ASC }}
                ) {{
                    request_id
                    session_id
                    agent_did
                    behavior_id
                    lifecycle_state
                    caused_by_parent_request_id
                    caused_by_parent_tool_call_id
                    subagent_depth
                }}
            }}"#,
            parent_request_id = escape_graphql_string(parent_request_id),
        );
        let response = graphql_query(graphql, &query).await?;
        let tools = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let children = response
            .pointer("/data/AgentRequest")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if tools.len() > 1 || children.len() > 1 {
            bail!(
                "live parent must produce exactly one real spawn and one child; \
                 tools={tools:?}, children={children:?}"
            );
        }
        if let (Some(tool), Some(child)) = (tools.first(), children.first()) {
            let child_request_id = child
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("spawned child missing request_id: {child}"))?;
            let tool_child_request_id = tool
                .get("child_request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("spawn tool missing child_request_id: {tool}"))?;
            anyhow::ensure!(
                tool_child_request_id == child_request_id,
                "spawn bridge and child lineage disagree: tool={tool}, child={child}"
            );
            anyhow::ensure!(
                tool.get("spawn_target_did").and_then(Value::as_str) == Some(child_agent_did)
                    && child.get("agent_did").and_then(Value::as_str) == Some(child_agent_did),
                "real spawn targeted the wrong principal: tool={tool}, child={child}"
            );
            anyhow::ensure!(
                child.get("behavior_id").and_then(Value::as_str) == Some(child_behavior_id),
                "real spawn targeted the wrong behavior: {child}"
            );
            anyhow::ensure!(
                child
                    .get("caused_by_parent_request_id")
                    .and_then(Value::as_str)
                    == Some(parent_request_id),
                "real child lost parent request lineage: {child}"
            );
            anyhow::ensure!(
                child
                    .get("caused_by_parent_tool_call_id")
                    .and_then(Value::as_str)
                    == tool.get("tool_call_id").and_then(Value::as_str),
                "real child and spawn bridge lost reciprocal tool-call lineage: \
                 tool={tool}, child={child}"
            );
            anyhow::ensure!(
                child.get("subagent_depth").and_then(Value::as_i64) == Some(1),
                "real child must be persisted at subagent depth 1: {child}"
            );
            anyhow::ensure!(
                tool.get("await_mode").and_then(Value::as_str) == Some("foreground"),
                "real spawn did not use foreground await mode: {tool}"
            );

            let child_state = child
                .get("lifecycle_state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let tool_state = tool
                .get("lifecycle_state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if ["failed", "cancelled", "timedOut"].contains(&child_state)
                || ["failed", "cancelled", "timedOut"].contains(&tool_state)
            {
                bail!(
                    "real subagent spawn terminalized unsuccessfully: tool={tool}, child={child}"
                );
            }
            if child_state == "completed" && tool_state == "completed" {
                let child_projection = graphql_query(
                    graphql,
                    &format!(
                        r#"{{
                            AgentResponse(
                                filter: {{ request_id: {{ _eq: "{}" }} }},
                                limit: 1
                            ) {{ request_id content status }}
                            AgentMessage(
                                filter: {{
                                    session_id: {{ _eq: "{}" }},
                                    role: {{ _eq: "assistant" }}
                                }},
                                order: {{ sequence: ASC }}
                            ) {{ content role sequence }}
                        }}"#,
                        escape_graphql_string(child_request_id),
                        escape_graphql_string(
                            child
                                .get("session_id")
                                .and_then(Value::as_str)
                                .ok_or_else(|| anyhow!("spawned child missing session: {child}"))?
                        ),
                    ),
                )
                .await?;
                if let Ok(response_row) = first_graphql_row(&child_projection, "AgentResponse") {
                    let mut content = response_row
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    for message in child_projection
                        .pointer("/data/AgentMessage")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(message_content) =
                            message.get("content").and_then(Value::as_str)
                        {
                            content.push_str(message_content);
                        }
                    }
                    if response_row.get("status").and_then(Value::as_str) == Some("complete")
                        && content.contains(child_token)
                    {
                        let parent_session_id = tool
                            .get("session_id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow!("spawn tool missing parent session: {tool}"))?;
                        let child_session_id = child
                            .get("session_id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow!("spawned child missing session: {child}"))?;
                        return Ok(RealSpawnProjection {
                            parent_session_id: parent_session_id.to_string(),
                            child_session_id: child_session_id.to_string(),
                        });
                    }
                }
            }
        }

        if std::time::Instant::now() >= deadline {
            bail!("timed out waiting for real runtime subagent spawn projection; last={response}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub(super) async fn initialize_config_and_thread(
    ws: &mut ShimWebSocket,
    _home_dir: &std::path::Path,
) -> Result<()> {
    send_client_request(
        ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(101),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "gents-live-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _: codex::InitializeResponse = read_typed_response(ws, request_id(101)).await?;
    send_client_notification(ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        ws,
        codex::ClientRequest::ConfigRead {
            request_id: request_id(102),
            params: codex::ConfigReadParams {
                include_layers: false,
                cwd: None,
            },
        },
    )
    .await?;
    let _: codex::ConfigReadResponse = read_typed_response(ws, request_id(102)).await?;
    Ok(())
}

pub(super) async fn start_thread(
    ws: &mut ShimWebSocket,
    home_dir: &std::path::Path,
) -> Result<String> {
    send_client_request(
        ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(103),
            params: codex::ThreadStartParams {
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_start: codex::ThreadStartResponse = read_typed_response(ws, request_id(103)).await?;
    Ok(thread_start.thread.id)
}

pub(super) async fn send_turn(ws: &mut ShimWebSocket, thread_id: &str, prompt: &str) -> Result<()> {
    send_client_request(
        ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(104),
            params: codex::TurnStartParams {
                thread_id: thread_id.to_string(),
                input: vec![codex::UserInput::Text {
                    text: prompt.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(ws, request_id(104)).await?;
    Ok(())
}

pub(super) async fn seed_blank_materialized_completion(
    graphql: &str,
    request_id: &str,
    agent_did: &str,
    behavior_id: &str,
    session_id: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let message_key = format!("{session_id}:blank-terminal");
    let blank_assistant = "\n\n\n";
    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{session_id}",
                sequence: 2,
                role: "assistant",
                content: "{blank_assistant}",
                timestamp: "{now}"
            }}) {{ _docID }}
            upsert_AgentResponse(
                filter: {{ response_key: {{ _eq: "{request_id}" }} }},
                add: {{
                    response_key: "{request_id}",
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    session_id: "{session_id}",
                    content: "",
                    reasoning: "",
                    status: "complete",
                    error_message: "",
                    token_count: 0,
                    progress_seq: 0,
                    materialized_message_sequence: 2,
                    materialized_at: "{now}",
                    created_at: "{now}",
                    completed_at: "{now}"
                }},
                update: {{
                    content: "",
                    reasoning: "",
                    status: "complete",
                    error_message: "",
                    progress_seq: 0,
                    materialized_message_sequence: 2,
                    materialized_at: "{now}",
                    completed_at: "{now}"
                }}
            ) {{ _docID }}
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    status: "completed",
                    lifecycle_state: "completed",
                    failure_reason: ""
                }}
            ) {{ _docID }}
        }}"#,
        message_key = escape_graphql_string(&message_key),
        session_id = escape_graphql_string(session_id),
        blank_assistant = escape_graphql_string(blank_assistant),
        now = escape_graphql_string(&now),
        request_id = escape_graphql_string(request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_id = escape_graphql_string(behavior_id),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

pub(super) async fn seed_running_background_tool(
    graphql: &str,
    request_id: &str,
    session_id: &str,
    tool_call_key: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{request_id}",
                session_id: "{session_id}",
                message_sequence: 1,
                tool_name: "bash",
                tool_call_id: "codex-bg-interrupt",
                args: "{{\"command\":\"sleep 600\"}}",
                result: "",
                status: "called",
                lifecycle_state: "running",
                started_at: "{now}",
                await_mode: "background"
            }}) {{ _docID }}
        }}"#,
        tool_call_key = escape_graphql_string(tool_call_key),
        request_id = escape_graphql_string(request_id),
        session_id = escape_graphql_string(session_id),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn seed_authorized_subagent_link(
    graphql: &str,
    agent_did: &str,
    child_behavior_id: &str,
    parent_request_id: &str,
    parent_session_id: &str,
    child_request_id: &str,
    child_session_id: &str,
    tool_call_id: &str,
    tool_call_key: &str,
    child_backend_id: &str,
    child_model_name: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let args = serde_json::to_string(&json!({
        "name": "reviewer",
        "prompt": "Inspect the parent change"
    }))?;
    let result = serde_json::to_string(&json!({
        "child_request_id": child_request_id,
        "child_session_id": child_session_id
    }))?;
    let mutation = format!(
        r#"mutation {{
            create_AgentBehavior(input: {{
                behavior_id: "{child_behavior_id}",
                agent_did: "{agent_did}",
                display_name: "reviewer",
                system_prompt: "",
                backend_id: "{child_backend_id}",
                model_name: "{child_model_name}",
                tool_selection_id: "",
                inference_profile_id: "",
                compaction_strategy: "StripThenSummarize",
                compaction_threshold: 0.75,
                enabled: false,
                created_at: "{now}"
            }}) {{ _docID }}
            create_AgentSession(input: {{
                session_id: "{child_session_id}",
                agent_name: "reviewer",
                agent_did: "{agent_did}",
                behavior_id: "{child_behavior_id}",
                started: "{now}",
                status: "active"
            }}) {{ _docID }}
            create_AgentRequest(input: {{
                request_id: "{child_request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{child_behavior_id}",
                session_id: "{child_session_id}",
                content: "Inspect the parent change",
                metadata: "{{}}",
                status: "processing",
                lifecycle_state: "processing",
                execution_origin: "subagent",
                failure_reason: "",
                created_at: "{now}",
                retry_count: 0,
                max_retries: 3,
                subagent_depth: 1,
                caused_by_parent_request_id: "{parent_request_id}",
                caused_by_parent_tool_call_id: "{tool_call_id}"
            }}) {{ _docID }}
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{parent_request_id}",
                session_id: "{parent_session_id}",
                agent_did: "{agent_did}",
                message_sequence: 1,
                tool_name: "spawn_subagent",
                tool_call_id: "{tool_call_id}",
                args: "{args}",
                result: "{result}",
                status: "completed",
                lifecycle_state: "completed",
                child_request_id: "{child_request_id}",
                spawn_target_did: "{agent_did}",
                started_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        child_session_id = escape_graphql_string(child_session_id),
        agent_did = escape_graphql_string(agent_did),
        child_behavior_id = escape_graphql_string(child_behavior_id),
        child_backend_id = escape_graphql_string(child_backend_id),
        child_model_name = escape_graphql_string(child_model_name),
        now = escape_graphql_string(&now),
        child_request_id = escape_graphql_string(child_request_id),
        parent_request_id = escape_graphql_string(parent_request_id),
        tool_call_id = escape_graphql_string(tool_call_id),
        tool_call_key = escape_graphql_string(tool_call_key),
        parent_session_id = escape_graphql_string(parent_session_id),
        args = escape_graphql_string(&args),
        result = escape_graphql_string(&result),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

pub(super) async fn delete_agent_behavior(graphql: &str, behavior_id: &str) -> Result<()> {
    let behavior_id = escape_graphql_string(behavior_id);
    let response = graphql_query(
        graphql,
        &format!(
            r#"mutation {{
                delete_AgentBehavior(filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }}) {{
                    _docID
                }}
            }}"#
        ),
    )
    .await?;
    anyhow::ensure!(
        response
            .pointer("/data/delete_AgentBehavior")
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty()),
        "expected child AgentBehavior to be deleted: {response}"
    );
    Ok(())
}

pub(super) async fn seed_unresolved_completed_subagent_tool(
    graphql: &str,
    agent_did: &str,
    parent_request_id: &str,
    parent_session_id: &str,
    tool_call_key: &str,
) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let completed_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let missing_child_request_id = Uuid::new_v4().to_string();
    let args = serde_json::to_string(&json!({
        "name": "replication-lagged",
        "prompt": "This child edge is intentionally unavailable"
    }))?;
    let result = serde_json::to_string(&json!({
        "child_request_id": missing_child_request_id
    }))?;
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{parent_request_id}",
                session_id: "{parent_session_id}",
                agent_did: "{agent_did}",
                message_sequence: 2,
                tool_name: "spawn_subagent",
                tool_call_id: "unresolved-spawn",
                args: "{args}",
                result: "{result}",
                status: "completed",
                lifecycle_state: "completed",
                child_request_id: "{missing_child_request_id}",
                spawn_target_did: "{agent_did}",
                selected_service_id: "runtime-subagents",
                selected_tool_name: "spawn",
                latency_ms: 23,
                started_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        tool_call_key = escape_graphql_string(tool_call_key),
        parent_request_id = escape_graphql_string(parent_request_id),
        parent_session_id = escape_graphql_string(parent_session_id),
        agent_did = escape_graphql_string(agent_did),
        args = escape_graphql_string(&args),
        result = escape_graphql_string(&result),
        missing_child_request_id = escape_graphql_string(&missing_child_request_id),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(completed_at_ms)
}

pub(super) async fn seed_child_streaming_response(
    graphql: &str,
    agent_did: &str,
    behavior_id: &str,
    request_id: &str,
    session_id: &str,
    content: &str,
    reasoning: &str,
) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let created_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{request_id}",
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                content: "{content}",
                reasoning: "{reasoning}",
                status: "streaming",
                error_message: "",
                token_count: 0,
                progress_seq: 1,
                reasoning_progress_seq: 1,
                created_at: "{now}",
                completed_at: ""
            }}) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_id = escape_graphql_string(behavior_id),
        session_id = escape_graphql_string(session_id),
        content = escape_graphql_string(content),
        reasoning = escape_graphql_string(reasoning),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(created_at_ms)
}

pub(super) async fn update_streaming_response_reasoning(
    graphql: &str,
    request_id: &str,
    reasoning: &str,
    reasoning_progress_seq: i64,
) -> Result<()> {
    let mutation = format!(
        r#"mutation {{
            update_AgentResponse(
                filter: {{ response_key: {{ _eq: "{request_id}" }} }},
                input: {{
                    reasoning: "{reasoning}",
                    reasoning_progress_seq: {reasoning_progress_seq}
                }}
            ) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        reasoning = escape_graphql_string(reasoning),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

pub(super) async fn materialize_child_response_before_terminal(
    graphql: &str,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    reasoning: &str,
) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let materialized_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let message_key = format!("{session_id}:2");
    let content = r#"{"role":"assistant","id":null,"content":[{"text":"durable child answer"}]}"#;
    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{session_id}",
                agent_did: "{agent_did}",
                request_id: "{request_id}",
                sequence: 2,
                role: "assistant",
                content: "{content}",
                reasoning: "{reasoning}",
                timestamp: "{now}"
            }}) {{ _docID }}
            update_AgentResponse(
                filter: {{ response_key: {{ _eq: "{request_id}" }} }},
                input: {{
                    content: "",
                    reasoning: "",
                    progress_seq: 2,
                    materialized_message_sequence: 2,
                    materialized_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#,
        message_key = escape_graphql_string(&message_key),
        session_id = escape_graphql_string(session_id),
        agent_did = escape_graphql_string(agent_did),
        request_id = escape_graphql_string(request_id),
        content = escape_graphql_string(content),
        reasoning = escape_graphql_string(reasoning),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(materialized_at_ms)
}

pub(super) async fn finalize_child_response_after_materialization(
    graphql: &str,
    request_id: &str,
) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let completed_at_ms = chrono::DateTime::parse_from_rfc3339(&now)?.timestamp_millis();
    let mutation = format!(
        r#"mutation {{
            update_AgentResponse(
                filter: {{ response_key: {{ _eq: "{request_id}" }} }},
                input: {{
                    status: "complete",
                    completed_at: "{now}"
                }}
            ) {{ _docID }}
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    status: "completed",
                    lifecycle_state: "completed",
                    failure_reason: ""
                }}
            ) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(completed_at_ms)
}

pub(super) async fn update_request_lifecycle(
    graphql: &str,
    request_id: &str,
    lifecycle_state: &str,
) -> Result<()> {
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    status: "{lifecycle_state}",
                    lifecycle_state: "{lifecycle_state}",
                    failure_reason: ""
                }}
            ) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        lifecycle_state = escape_graphql_string(lifecycle_state),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

pub(super) fn require_command(name: &str) -> Result<()> {
    if which(name).is_some() {
        Ok(())
    } else {
        bail!("{name} is required for this smoke test")
    }
}

pub(super) fn run_git_command(cwd: &std::path::Path, args: &[&str]) -> Result<()> {
    let _ = run_git_output(cwd, args)?;
    Ok(())
}

fn run_git_output(cwd: &std::path::Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("running git {} in {}", args.join(" "), cwd.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(super) fn init_test_git_repo(cwd: &std::path::Path, branch: &str) -> Result<String> {
    require_command("git")?;
    run_git_command(cwd, &["init"])?;
    run_git_command(cwd, &["checkout", "-B", branch])?;
    fs::write(cwd.join(".codex-shim-git-fixture"), "base\n")
        .with_context(|| format!("writing git fixture in {}", cwd.display()))?;
    run_git_command(cwd, &["add", ".codex-shim-git-fixture"])?;
    run_git_command(
        cwd,
        &[
            "-c",
            "user.name=Gents Test",
            "-c",
            "user.email=gents-test@example.invalid",
            "commit",
            "-m",
            "base",
        ],
    )?;
    Ok(run_git_output(cwd, &["rev-parse", "HEAD"])?
        .trim()
        .to_string())
}

pub(super) fn gh_is_authenticated() -> bool {
    Command::new("gh")
        .arg("auth")
        .arg("status")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub(super) fn which(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(std::path::Path::new)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.exists())
}

pub(super) fn workspace_root() -> Result<std::path::PathBuf> {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| anyhow!("unable to resolve workspace root from CARGO_MANIFEST_DIR"))
}

pub(super) fn write_expect_smoke(
    script: &std::path::Path,
    transcript: &std::path::Path,
    client_codex_home: &std::path::Path,
    shim_port: u16,
    prompt_token: &str,
) -> Result<()> {
    let prompt = smoke_prompt(prompt_token);
    let token_match_regex = tcl_regex_terminal_tolerant_literal(prompt_token);
    let contents = format!(
        r#"set timeout 120
set env(CODEX_HOME) {{{codex_home}}}
set env(TERM) xterm-256color
stty rows 40 columns 120
log_user 0
spawn codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{shim_port}/ {{{prompt}}}
log_file -a {{{transcript}}}
set match_count 0
expect {{
  -ex "\033\[6n" {{
    send "\033\[24;1R"
    exp_continue
  }}
  -ex "\033\[?u" {{
    send "\033\[?0u"
    exp_continue
  }}
  -ex "\033\[c" {{
    send "\033\[?1;2c"
    exp_continue
  }}
  -ex "\033]10;?\033\\" {{
    send "\033]10;rgb:ffff/ffff/ffff\033\\"
    exp_continue
  }}
  -ex "\033]11;?\033\\" {{
    send "\033]11;rgb:0000/0000/0000\033\\"
    exp_continue
  }}
  -re {{{token_match_regex}}} {{
    incr match_count
    if {{$match_count >= 2}} {{
      after 2000
      send "\003"
      expect {{
        eof {{ exit 0 }}
        timeout {{ exit 0 }}
      }}
    }}
    exp_continue
  }}
  timeout {{
    send "\003"
    expect {{
      eof {{ exit 0 }}
      timeout {{ exit 0 }}
    }}
  }}
  eof {{ exit 2 }}
}}
"#,
        transcript = tcl_brace(transcript),
        codex_home = tcl_brace(client_codex_home),
        prompt = tcl_brace_str(&prompt),
        token_match_regex = tcl_brace_str(&token_match_regex),
    );
    fs::write(script, contents).with_context(|| format!("writing {}", script.display()))
}

pub(super) fn smoke_prompt(prompt_token: &str) -> String {
    format!("Reply with exactly this token and no extra words: {prompt_token}")
}

pub(super) fn multiturn_first_prompt(memory_token: &str) -> String {
    format!(
        "The project codeword for this conversation is {memory_token}. Reply with exactly READY and no extra words."
    )
}

pub(super) fn multiturn_second_prompt() -> &'static str {
    "Take the project codeword I gave earlier, replace LIME with MINT, keep the digit, and reply with exactly the transformed codeword and no extra words."
}

pub(super) fn assert_shim_trace_methods(path: &std::path::Path, methods: &[&str]) -> Result<()> {
    let trace = fs::read_to_string(path)
        .with_context(|| format!("reading shim trace {}", path.display()))?;
    for method in methods {
        assert!(
            trace.contains(method),
            "expected shim trace to contain {method}, got:\n{trace}"
        );
    }
    Ok(())
}

pub(super) fn assert_shim_trace_method_count_at_least(
    path: &std::path::Path,
    method: &str,
    minimum: usize,
) -> Result<()> {
    let trace = fs::read_to_string(path)
        .with_context(|| format!("reading shim trace {}", path.display()))?;
    let count = trace.matches(method).count();
    assert!(
        count >= minimum,
        "expected shim trace to contain {method} at least {minimum} times, got {count}:\n{trace}"
    );
    Ok(())
}

pub(super) fn wait_for_tmux_token_occurrences(
    session: &str,
    needle: &str,
    required_count: usize,
    timeout: Duration,
) -> Result<String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = String::new();
    loop {
        let output = Command::new("tmux")
            .args(["capture-pane", "-pt", session])
            .output()
            .context("capturing tmux pane")?;
        if output.status.success() {
            last = String::from_utf8_lossy(&output.stdout).into_owned();
            if token_occurrences(&terminal_token_search_text(&last), needle) >= required_count {
                return Ok(last);
            }
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "timed out waiting for {required_count} occurrences of {needle} in tmux pane; last transcript:\n{last}"
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub(super) fn shell_quote_path(path: &std::path::Path) -> String {
    shell_quote(&path.display().to_string())
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn tcl_brace(path: &std::path::Path) -> String {
    tcl_brace_str(&path.display().to_string())
}

fn tcl_brace_str(value: &str) -> String {
    value.replace('\\', r"\\").replace('}', r"\}")
}

fn tcl_regex_terminal_tolerant_literal(value: &str) -> String {
    let mut regex = String::from("(?s)");
    for (index, ch) in value.chars().enumerate() {
        if index > 0 {
            regex.push_str(".*");
        }
        if matches!(
            ch,
            '.' | '\\'
                | '+'
                | '*'
                | '?'
                | '['
                | '^'
                | ']'
                | '$'
                | '('
                | ')'
                | '{'
                | '}'
                | '='
                | '!'
                | '<'
                | '>'
                | '|'
                | ':'
                | '-'
        ) {
            regex.push('\\');
        }
        regex.push(ch);
    }
    regex
}

pub(super) fn terminal_token_search_text(value: &str) -> String {
    terminal_visible_text(value)
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect()
}

pub(super) fn token_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

fn terminal_visible_text(value: &str) -> String {
    let mut visible = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            skip_escape_sequence(&mut chars);
        } else if ch == '\r' || ch == '\n' {
            visible.push('\n');
        } else if !ch.is_control() {
            visible.push(ch);
        }
    }
    visible
}

fn skip_escape_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        }
        Some(']') => {
            chars.next();
            let mut saw_escape = false;
            for ch in chars.by_ref() {
                if ch == '\u{7}' || (saw_escape && ch == '\\') {
                    break;
                }
                saw_escape = ch == '\u{1b}';
            }
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

pub(super) fn request_id(value: i64) -> codex::RequestId {
    codex::RequestId::Integer(value)
}

pub(super) async fn send_client_request(
    ws: &mut ShimWebSocket,
    request: codex::ClientRequest,
) -> Result<()> {
    let value = serde_json::to_value(request).context("serializing Codex client request")?;
    let request: codex::JSONRPCRequest =
        serde_json::from_value(value).context("building JSON-RPC request")?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Request(request)).await
}

pub(super) async fn send_raw_client_request(
    ws: &mut ShimWebSocket,
    request_id: codex::RequestId,
    method: &str,
    params: Value,
) -> Result<()> {
    let request: codex::JSONRPCRequest = serde_json::from_value(json!({
        "id": request_id,
        "method": method,
        "params": params,
    }))
    .with_context(|| format!("building raw JSON-RPC request for {method}"))?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Request(request)).await
}

pub(super) async fn send_client_notification(
    ws: &mut ShimWebSocket,
    notification: codex::ClientNotification,
) -> Result<()> {
    let value =
        serde_json::to_value(notification).context("serializing Codex client notification")?;
    let notification: codex::JSONRPCNotification =
        serde_json::from_value(value).context("building JSON-RPC notification")?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Notification(notification)).await
}

async fn write_jsonrpc(ws: &mut ShimWebSocket, message: codex::JSONRPCMessage) -> Result<()> {
    let text = serde_json::to_string(&message).context("encoding JSON-RPC message")?;
    ws.send(WsMessage::Text(text.into()))
        .await
        .context("sending JSON-RPC websocket message")
}

pub(super) async fn read_typed_response<T>(
    ws: &mut ShimWebSocket,
    expected_id: codex::RequestId,
) -> Result<T>
where
    T: DeserializeOwned,
{
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                return serde_json::from_value(response.result)
                    .context("decoding typed Codex response");
            }
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                bail!(
                    "Codex shim returned error for request {}: {}",
                    expected_id,
                    error.error.message
                );
            }
            codex::JSONRPCMessage::Notification(_) => {}
            other => {
                bail!("unexpected JSON-RPC message while waiting for {expected_id}: {other:?}")
            }
        }
    }
}

pub(super) async fn read_error_response(
    ws: &mut ShimWebSocket,
    expected_id: codex::RequestId,
) -> Result<codex::JSONRPCErrorError> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                return Ok(error.error);
            }
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                bail!("expected JSON-RPC error for {expected_id}, got response {response:?}");
            }
            codex::JSONRPCMessage::Notification(_) => {}
            other => {
                bail!(
                    "unexpected JSON-RPC message while waiting for error {expected_id}: {other:?}"
                )
            }
        }
    }
}

pub(super) async fn read_turn_started(
    ws: &mut ShimWebSocket,
) -> Result<codex::TurnStartedNotification> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::TurnStarted(started) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(started);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) async fn read_thread_status_changed(
    ws: &mut ShimWebSocket,
    expected_thread_id: &str,
) -> Result<codex::ThreadStatus> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ThreadStatusChanged(changed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if changed.thread_id == expected_thread_id {
                        return Ok(changed.status);
                    }
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) async fn read_background_command_started(
    ws: &mut ShimWebSocket,
    expected_tool_call_key: &str,
) -> Result<String> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ItemStarted(started) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if let codex::ThreadItem::CommandExecution { id, process_id, .. } = started.item
                    {
                        if id == expected_tool_call_key
                            && process_id.as_deref() == Some(expected_tool_call_key)
                        {
                            return Ok(id);
                        }
                    }
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) async fn read_collab_agent_status(
    ws: &mut ShimWebSocket,
    expected_tool_call_key: &str,
    child_thread_id: &str,
) -> Result<(codex::CollabAgentStatus, Option<String>, bool)> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ItemCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if let codex::ThreadItem::CollabAgentToolCall {
                        id,
                        model,
                        reasoning_effort,
                        agents_states,
                        ..
                    } = completed.item
                    {
                        if id == expected_tool_call_key {
                            let status = agents_states
                                .get(child_thread_id)
                                .map(|state| state.status.clone())
                                .with_context(|| {
                                    format!(
                                        "collab item {id} missing child state for {child_thread_id}"
                                    )
                                })?;
                            return Ok((status, model, reasoning_effort.is_none()));
                        }
                    }
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) async fn read_mcp_tool_completion(
    ws: &mut ShimWebSocket,
    expected_tool_call_key: &str,
    expected_completed_at_ms: i64,
) -> Result<()> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ItemCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if let codex::ThreadItem::McpToolCall {
                        id,
                        server,
                        tool,
                        status,
                        duration_ms,
                        ..
                    } = completed.item
                    {
                        if id == expected_tool_call_key {
                            assert_eq!(status, codex::McpToolCallStatus::Completed);
                            assert_eq!(server, "runtime-subagents");
                            assert_eq!(tool, "spawn");
                            assert_eq!(duration_ms, Some(23));
                            assert_eq!(completed.completed_at_ms, expected_completed_at_ms);
                            return Ok(());
                        }
                    }
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) async fn read_child_agent_and_reasoning_deltas(
    ws: &mut ShimWebSocket,
    child_thread_id: &str,
    child_turn_id: &str,
) -> Result<(String, String, i64)> {
    let mut agent_delta = None;
    let mut reasoning_delta = None;
    let mut reasoning_started_at_ms = None;
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                match server_notification_from_jsonrpc(notification)? {
                    codex::ServerNotification::ItemStarted(started)
                        if started.thread_id == child_thread_id
                            && started.turn_id == child_turn_id
                            && matches!(started.item, codex::ThreadItem::Reasoning { .. }) =>
                    {
                        reasoning_started_at_ms = Some(started.started_at_ms);
                    }
                    codex::ServerNotification::AgentMessageDelta(delta)
                        if delta.thread_id == child_thread_id && delta.turn_id == child_turn_id =>
                    {
                        agent_delta = Some(delta.delta);
                    }
                    codex::ServerNotification::ReasoningTextDelta(delta)
                        if delta.thread_id == child_thread_id && delta.turn_id == child_turn_id =>
                    {
                        assert_eq!(delta.content_index, 0);
                        reasoning_delta = Some(delta.delta);
                    }
                    _ => {}
                }
                if reasoning_started_at_ms.is_some()
                    && agent_delta.is_some()
                    && reasoning_delta.is_some()
                {
                    return Ok((
                        agent_delta.take().expect("checked agent delta"),
                        reasoning_delta.take().expect("checked reasoning delta"),
                        reasoning_started_at_ms.expect("checked reasoning start"),
                    ));
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) async fn read_child_reasoning_delta(
    ws: &mut ShimWebSocket,
    child_thread_id: &str,
    child_turn_id: &str,
) -> Result<String> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ReasoningTextDelta(delta) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if delta.thread_id == child_thread_id && delta.turn_id == child_turn_id {
                        assert_eq!(delta.content_index, 0);
                        return Ok(delta.delta);
                    }
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) async fn read_child_reasoning_completion(
    ws: &mut ShimWebSocket,
    child_thread_id: &str,
    child_turn_id: &str,
) -> Result<(String, i64)> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ItemCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    if completed.thread_id == child_thread_id && completed.turn_id == child_turn_id
                    {
                        if let codex::ThreadItem::Reasoning {
                            content, summary, ..
                        } = completed.item
                        {
                            assert!(summary.is_empty());
                            return Ok((content.concat(), completed.completed_at_ms));
                        }
                    }
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) async fn read_terminal_child_without_reasoning_replay(
    ws: &mut ShimWebSocket,
    child_thread_id: &str,
    child_turn_id: &str,
    expected_tool_call_key: &str,
) -> Result<(codex::CollabAgentStatus, codex::ThreadStatus, i64)> {
    let mut child_status = None;
    let mut thread_status = None;
    let mut agent_completed_at_ms = None;
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                match server_notification_from_jsonrpc(notification)? {
                    codex::ServerNotification::ReasoningTextDelta(delta)
                        if delta.thread_id == child_thread_id && delta.turn_id == child_turn_id =>
                    {
                        bail!(
                            "terminal durable reasoning replayed after reset-tail completion: {}",
                            delta.delta
                        );
                    }
                    codex::ServerNotification::ItemStarted(started)
                        if started.thread_id == child_thread_id
                            && started.turn_id == child_turn_id
                            && matches!(started.item, codex::ThreadItem::Reasoning { .. }) =>
                    {
                        bail!(
                            "terminal durable reasoning opened a duplicate item after reset-tail"
                        );
                    }
                    codex::ServerNotification::ItemCompleted(completed) => match completed.item {
                        codex::ThreadItem::Reasoning { .. }
                            if completed.thread_id == child_thread_id
                                && completed.turn_id == child_turn_id =>
                        {
                            bail!(
                                "terminal durable reasoning completed a duplicate item after reset-tail"
                            );
                        }
                        codex::ThreadItem::AgentMessage { .. }
                            if completed.thread_id == child_thread_id
                                && completed.turn_id == child_turn_id =>
                        {
                            agent_completed_at_ms = Some(completed.completed_at_ms);
                        }
                        codex::ThreadItem::CollabAgentToolCall {
                            id, agents_states, ..
                        } if id == expected_tool_call_key => {
                            child_status = agents_states
                                .get(child_thread_id)
                                .map(|state| state.status.clone());
                        }
                        _ => {}
                    },
                    codex::ServerNotification::ThreadStatusChanged(changed)
                        if changed.thread_id == child_thread_id =>
                    {
                        thread_status = Some(changed.status);
                    }
                    _ => {}
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }

        if child_status.is_some() && thread_status.is_some() && agent_completed_at_ms.is_some() {
            return Ok((
                child_status.take().expect("checked child status"),
                thread_status.take().expect("checked thread status"),
                agent_completed_at_ms
                    .take()
                    .expect("checked agent completion timestamp"),
            ));
        }
    }
}

pub(super) async fn read_interrupt_response_and_completed_turn(
    ws: &mut ShimWebSocket,
    expected_id: codex::RequestId,
) -> Result<codex::Turn> {
    let mut saw_interrupt_response = false;
    let mut completed_turn = None;
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                let _: codex::TurnInterruptResponse = serde_json::from_value(response.result)
                    .context("decoding interrupt response")?;
                saw_interrupt_response = true;
            }
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                bail!(
                    "Codex shim returned error for interrupt {}: {}",
                    expected_id,
                    error.error.message
                );
            }
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::TurnCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    completed_turn = Some(completed.turn);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(response) => {
                bail!(
                    "unexpected JSON-RPC response while waiting for interrupt {expected_id}: {response:?}"
                );
            }
        }

        if saw_interrupt_response {
            if let Some(turn) = completed_turn.take() {
                return Ok(turn);
            }
        }
    }
}

pub(super) async fn read_fuzzy_file_search_update(
    ws: &mut ShimWebSocket,
) -> Result<codex::FuzzyFileSearchSessionUpdatedNotification> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::FuzzyFileSearchSessionUpdated(update) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(update);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) async fn read_fuzzy_file_search_completed(
    ws: &mut ShimWebSocket,
) -> Result<codex::FuzzyFileSearchSessionCompletedNotification> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::FuzzyFileSearchSessionCompleted(completed) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(completed);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

#[derive(Debug)]
pub(super) struct TurnCapture {
    pub(super) text: String,
    pub(super) turn: codex::Turn,
    pub(super) started_tools: Vec<String>,
    pub(super) completed_tool_ids: Vec<String>,
    pub(super) completed_tools: Vec<String>,
    pub(super) completed_collab_items: Vec<CompletedCollabItem>,
    pub(super) turn_completed_tool_ids: Vec<String>,
    pub(super) event_order: Vec<TurnStreamEvent>,
    pub(super) token_usage: Option<codex::ThreadTokenUsage>,
}

#[derive(Debug)]
pub(super) struct CompletedCollabItem {
    pub(super) tool: codex::CollabAgentTool,
    pub(super) status: codex::CollabAgentToolCallStatus,
    pub(super) receiver_thread_ids: Vec<String>,
    pub(super) model: Option<String>,
    pub(super) child_status: Option<codex::CollabAgentStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnStreamEvent {
    AgentDelta,
    ToolStarted,
    ToolCompleted,
}

pub(super) async fn read_turn_to_completion(
    ws: &mut ShimWebSocket,
) -> Result<(String, codex::Turn)> {
    let capture = read_turn_capture(ws).await?;
    Ok((capture.text, capture.turn))
}

pub(super) async fn read_turn_capture(ws: &mut ShimWebSocket) -> Result<TurnCapture> {
    let mut text = String::new();
    let mut started_tools = Vec::new();
    let mut completed_tool_ids = Vec::new();
    let mut completed_tools = Vec::new();
    let mut completed_collab_items = Vec::new();
    let mut event_order = Vec::new();
    let mut token_usage = None;
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                match server_notification_from_jsonrpc(notification)? {
                    codex::ServerNotification::ThreadTokenUsageUpdated(update) => {
                        token_usage = Some(update.token_usage);
                    }
                    codex::ServerNotification::AgentMessageDelta(delta) => {
                        if !delta.delta.is_empty() {
                            event_order.push(TurnStreamEvent::AgentDelta);
                        }
                        text.push_str(&delta.delta);
                    }
                    codex::ServerNotification::ItemStarted(started) => match started.item {
                        codex::ThreadItem::McpToolCall { tool, .. } => {
                            event_order.push(TurnStreamEvent::ToolStarted);
                            started_tools.push(tool);
                        }
                        codex::ThreadItem::CommandExecution { command, .. } => {
                            event_order.push(TurnStreamEvent::ToolStarted);
                            started_tools.push(command);
                        }
                        _ => {}
                    },
                    codex::ServerNotification::ItemCompleted(completed) => match completed.item {
                        codex::ThreadItem::McpToolCall { id, tool, .. } => {
                            event_order.push(TurnStreamEvent::ToolCompleted);
                            completed_tool_ids.push(id);
                            completed_tools.push(tool);
                        }
                        codex::ThreadItem::CommandExecution { id, command, .. } => {
                            event_order.push(TurnStreamEvent::ToolCompleted);
                            completed_tool_ids.push(id);
                            completed_tools.push(command);
                        }
                        codex::ThreadItem::CollabAgentToolCall {
                            tool,
                            status,
                            receiver_thread_ids,
                            model,
                            agents_states,
                            ..
                        } => {
                            event_order.push(TurnStreamEvent::ToolCompleted);
                            let child_status = receiver_thread_ids
                                .first()
                                .and_then(|thread_id| agents_states.get(thread_id))
                                .map(|state| state.status.clone());
                            completed_collab_items.push(CompletedCollabItem {
                                tool,
                                status,
                                receiver_thread_ids,
                                model,
                                child_status,
                            });
                        }
                        _ => {}
                    },
                    codex::ServerNotification::TurnCompleted(completed) => {
                        let turn_completed_tool_ids = mcp_tool_ids(&completed.turn);
                        return Ok(TurnCapture {
                            text,
                            turn: completed.turn,
                            started_tools,
                            completed_tool_ids,
                            completed_tools,
                            completed_collab_items,
                            turn_completed_tool_ids,
                            event_order,
                            token_usage,
                        });
                    }
                    _ => {}
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

pub(super) fn assert_text_contains_all_case_insensitive(text: &str, label: &str, needles: &[&str]) {
    let lower = text.to_ascii_lowercase();
    for needle in needles {
        assert!(
            lower.contains(&needle.to_ascii_lowercase()),
            "{label} response did not contain {needle:?}:\n{text}"
        );
    }
}

pub(super) fn turn_had_tool_before_later_agent_text(capture: &TurnCapture) -> bool {
    let mut saw_tool = false;
    for event in &capture.event_order {
        match event {
            TurnStreamEvent::AgentDelta if saw_tool => return true,
            TurnStreamEvent::ToolStarted | TurnStreamEvent::ToolCompleted => saw_tool = true,
            TurnStreamEvent::AgentDelta => {}
        }
    }
    false
}

pub(super) fn turn_had_tool_after_final_agent_text(capture: &TurnCapture) -> bool {
    let Some(final_agent_index) = capture
        .event_order
        .iter()
        .rposition(|event| *event == TurnStreamEvent::AgentDelta)
    else {
        return false;
    };
    capture.event_order[final_agent_index + 1..]
        .iter()
        .any(|event| {
            matches!(
                event,
                TurnStreamEvent::ToolStarted | TurnStreamEvent::ToolCompleted
            )
        })
}

fn mcp_tool_ids(turn: &codex::Turn) -> Vec<String> {
    turn.items
        .iter()
        .filter_map(|item| match item {
            codex::ThreadItem::McpToolCall { id, .. } => Some(id.clone()),
            codex::ThreadItem::CommandExecution { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn assert_turn_has_user_text(turn: &codex::Turn, expected: &str) {
    assert!(
        turn.items.iter().any(|item| match item {
            codex::ThreadItem::UserMessage { content, .. } => {
                content.iter().any(|input| match input {
                    codex::UserInput::Text { text, .. } => text.contains(expected),
                    _ => false,
                })
            }
            _ => false,
        }),
        "turn {} did not include user text {expected:?}: {:?}",
        turn.id,
        turn.items
    );
}

pub(super) fn assert_turn_has_agent_text(turn: &codex::Turn, expected: &str) {
    assert!(
        turn.items.iter().any(|item| match item {
            codex::ThreadItem::AgentMessage { text, .. } => text.contains(expected),
            _ => false,
        }),
        "turn {} did not include agent text {expected:?}: {:?}",
        turn.id,
        turn.items
    );
}

pub(super) async fn wait_for_request_metadata(
    graphql: &str,
    agent_did: &str,
    content: &str,
) -> Result<(String, String, Value)> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{}" }},
                        content: {{ _eq: "{}" }}
                    }},
                    order: {{ created_at: DESC }},
                    limit: 1
                ) {{
                    request_id
                    session_id
                    metadata
                }}
            }}"#,
            escape_graphql_string(agent_did),
            escape_graphql_string(content),
        );
        let response = graphql_query(graphql, &query).await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
            let request_id = row
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing request_id: {row}"))?;
            let session_id = row
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing session_id: {row}"))?;
            let metadata_raw = row
                .get("metadata")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing metadata: {row}"))?;
            let metadata = serde_json::from_str::<Value>(metadata_raw)
                .with_context(|| format!("decoding AgentRequest metadata: {metadata_raw}"))?;
            return Ok((request_id.to_string(), session_id.to_string(), metadata));
        }

        if std::time::Instant::now() >= deadline {
            bail!("timed out waiting for AgentRequest metadata for {agent_did}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub(super) async fn read_jsonrpc(ws: &mut ShimWebSocket) -> Result<codex::JSONRPCMessage> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(60), ws.next())
            .await
            .context("timed out waiting for Codex shim websocket message")?
            .ok_or_else(|| anyhow!("Codex shim websocket closed"))?
            .context("reading Codex shim websocket message")?;
        let text = match frame {
            WsMessage::Text(text) => text,
            WsMessage::Binary(bytes) => String::from_utf8(bytes.to_vec())
                .context("decoding binary websocket payload as UTF-8")?
                .into(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            WsMessage::Close(close) => bail!("Codex shim websocket closed: {close:?}"),
            WsMessage::Frame(_) => bail!("unexpected raw websocket frame"),
        };
        return serde_json::from_str(&text)
            .with_context(|| format!("decoding JSON-RPC message: {text}"));
    }
}

fn server_notification_from_jsonrpc(
    notification: codex::JSONRPCNotification,
) -> Result<codex::ServerNotification> {
    serde_json::from_value(serde_json::to_value(notification)?)
        .context("decoding Codex server notification")
}

pub(super) async fn read_token_usage_notification(
    ws: &mut ShimWebSocket,
) -> Result<codex::ThreadTokenUsage> {
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                if let codex::ServerNotification::ThreadTokenUsageUpdated(update) =
                    server_notification_from_jsonrpc(notification)?
                {
                    return Ok(update.token_usage);
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}
