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
        init_args.push("--write");
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
