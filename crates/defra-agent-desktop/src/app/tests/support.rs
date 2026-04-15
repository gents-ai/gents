async fn insert_agent_principal(
    core: &ClientCore,
    agent_did: &str,
    display_name: &str,
    default_behavior_id: &str,
) -> Result<()> {
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
            add_AgentPrincipal(input: {{
                agent_did: "{agent_did}"
                display_name: "{display_name}"
                default_behavior_id: "{default_behavior_id}"
                enabled: true
            }}) {{ agent_did }}
        }}"#,
            agent_did = escape_graphql_string(agent_did),
            display_name = escape_graphql_string(display_name),
            default_behavior_id = escape_graphql_string(default_behavior_id),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!("add_AgentPrincipal failed: {:?}", response.errors);
    }
    Ok(())
}

async fn insert_agent_runtime(
    core: &ClientCore,
    agent_did: &str,
    default_behavior_id: &str,
) -> Result<()> {
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
            upsert_AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                add: {{
                    agent_did: "{agent_did}"
                    process_state: "ready"
                    reconcile_phase: "idle"
                    active_generation: 1
                    router_generation: 1
                    default_behavior_id: "{default_behavior_id}"
                    runnable_behavior_count: 1
                    unavailable_behavior_count: 0
                    last_reconcile_result: "startup"
                    last_reconcile_error: ""
                    last_reconcile_completed_at: "2026-04-14T00:00:00Z"
                    updated_at: "2026-04-14T00:00:00Z"
                }},
                update: {{
                    process_state: "ready"
                    reconcile_phase: "idle"
                    active_generation: 1
                    router_generation: 1
                    default_behavior_id: "{default_behavior_id}"
                    runnable_behavior_count: 1
                    unavailable_behavior_count: 0
                    last_reconcile_result: "startup"
                    last_reconcile_error: ""
                    last_reconcile_completed_at: "2026-04-14T00:00:00Z"
                    updated_at: "2026-04-14T00:00:00Z"
                }}
            ) {{ _docID }}
        }}"#,
            agent_did = escape_graphql_string(agent_did),
            default_behavior_id = escape_graphql_string(default_behavior_id),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!("upsert_AgentRuntime failed: {:?}", response.errors);
    }
    Ok(())
}

async fn seed_operator_documents(core: &ClientCore) -> Result<()> {
    insert_agent_principal(core, "did:defra:amy", "Amy", "amy-default").await?;
    insert_agent_runtime(core, "did:defra:amy", "amy-default").await?;

    core.save_backend(&InferenceBackendRow {
        backend_id: "backend-amy".to_string(),
        name: Some("OpenRouter".to_string()),
        provider_kind: Some("openrouter".to_string()),
        endpoint: Some("https://openrouter.ai/api/v1".to_string()),
        api_key: None,
        api_key_env_var: Some("OPENROUTER_API_KEY".to_string()),
        max_concurrent: Some(2),
        max_queue_depth: Some(100),
        enabled: Some(true),
        models: vec!["openai/gpt-5.4".to_string()],
        last_probe: None,
        probe_status: Some("healthy".to_string()),
    })
    .await?;
    core.save_inference_profile(&InferenceProfileRow {
        profile_id: "profile-amy".to_string(),
        display_name: Some("Amy Profile".to_string()),
        context_window: Some(128000),
        max_output_tokens: Some(4096),
        max_turns: Some(24),
        temperature: Some(0.2),
        stream_batch_ms: Some(50),
        deadline_duration_secs: Some(300),
    })
    .await?;
    core.save_tool_selection(&ToolSelectionRow {
        selection_id: "tools-amy".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        display_name: Some("Amy Tools".to_string()),
        enable_file_tools: Some(true),
        file_tools_mode: Some("workspace-write".to_string()),
        enable_bash: Some(true),
        bash_mode: Some("workspace".to_string()),
        cli_tool_names: vec!["rg".to_string(), "cargo".to_string()],
        enable_meta_tools: Some(true),
        delegate_to: vec!["planner".to_string()],
    })
    .await?;
    core.save_behavior(&AgentBehaviorRow {
        behavior_id: "amy-default".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        display_name: Some("Amy Default".to_string()),
        system_prompt: Some("You are Amy.".to_string()),
        backend_id: Some("backend-amy".to_string()),
        model_name: Some("openai/gpt-5.4".to_string()),
        tool_selection_id: Some("tools-amy".to_string()),
        inference_profile_id: Some("profile-amy".to_string()),
        compaction_strategy: Some("StripThenSummarize".to_string()),
        compaction_threshold: Some(0.7),
        enabled: Some(true),
        created_at: Some("2026-04-14T00:00:00Z".to_string()),
    })
    .await?;
    core.save_scheduled_task(&ScheduledTaskRow {
        task_id: "task-amy-daily".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        behavior_id: Some("amy-default".to_string()),
        name: Some("Daily Amy".to_string()),
        prompt: Some("Check the daily queue.".to_string()),
        interval_secs: Some(300),
        enabled: Some(true),
        next_run_at: Some("2026-04-15T00:00:00Z".to_string()),
        last_run_at: None,
        last_status: Some("ok".to_string()),
        last_error: None,
        run_count: Some(4),
        created_at: None,
        updated_at: None,
    })
    .await?;
    core.refresh_store().await?;
    Ok(())
}

async fn insert_chat_transcript_documents(
    core: &ClientCore,
    session_id: &str,
    agent_did: &str,
    behavior_id: &str,
    response_key: &str,
) -> Result<()> {
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{
            add_AgentMessage(input: {{
                message_key: "msg-assistant-1"
                session_id: "{session_id}"
                sequence: 2
                role: "assistant"
                content: "I checked the queue and opened the trace."
                timestamp: "2026-04-14T00:00:01Z"
            }}) {{ message_key }}
            add_AgentToolCall(input: {{
                tool_call_key: "tool-call-1"
                session_id: "{session_id}"
                message_sequence: 2
                tool_name: "shell"
                tool_call_id: "call-shell-1"
                args: "{{\"cmd\":\"rg audit\"}}"
                status: "completed"
                started_at: "2026-04-14T00:00:02Z"
                completed_at: "2026-04-14T00:00:03Z"
            }}) {{ tool_call_key }}
            add_AgentToolResult(input: {{
                agent_did: "{agent_did}"
                session_id: "{session_id}"
                tool_name: "shell"
                tool_input: "rg audit"
                output_text: "src/app.rs: audit target live"
                truncated: false
                truncation_metadata: ""
                conversation_doc_id: "{session_id}"
                created_at: "2026-04-14T00:00:03Z"
            }}) {{ _docID }}
            add_AgentResponse(input: {{
                response_key: "{response_key}"
                agent_did: "{agent_did}"
                behavior_id: "{behavior_id}"
                session_id: "{session_id}"
                content: "Queue checked."
                reasoning: "I verified the latest request, ran the shell tool, and summarized the result."
                status: "completed"
                error_message: ""
                token_count: 42
                progress_seq: 1
                created_at: "2026-04-14T00:00:04Z"
                completed_at: "2026-04-14T00:00:05Z"
            }}) {{ response_key }}
        }}"#,
            session_id = escape_graphql_string(session_id),
            agent_did = escape_graphql_string(agent_did),
            behavior_id = escape_graphql_string(behavior_id),
            response_key = escape_graphql_string(response_key),
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "insert chat transcript documents failed: {:?}",
            response.errors
        );
    }
    core.refresh_store().await?;
    Ok(())
}

fn build_driver(
    runtime: Arc<Runtime>,
    core: ClientCore,
    log_store: Arc<DesktopLogStore>,
) -> AuditDriver {
    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let app = DesktopApp::from_parts(&cc, runtime, Some(Arc::new(core)), Vec::new(), log_store);
    AuditDriver::new(app, ctx)
}

#[derive(Debug, Clone)]
struct LiveAgentDocs {
    behavior_id: String,
    backend_id: String,
    tool_selection_id: String,
    inference_profile_id: String,
    scheduled_task_id: String,
}

struct LiveDesktopFixture {
    runtime: Arc<Runtime>,
    _tempdir: tempfile::TempDir,
    driver: AuditDriver,
    running_agent: Option<RunningAgent>,
    docs: LiveAgentDocs,
    backend: AgentBackendConfig,
}

impl LiveDesktopFixture {
    fn shutdown(mut self) -> Result<()> {
        if let Some(running_agent) = self.running_agent.take() {
            self.runtime.block_on(running_agent.shutdown())?;
        }
        self.driver.app.shutdown_client();
        Ok(())
    }
}

struct LiveRemoteDeployment {
    label: String,
    peer_id: String,
    addr: String,
    agent_did: String,
    core: ClientCore,
    running_agent: RunningAgent,
    docs: LiveAgentDocs,
}

struct MultiAgentLiveDesktopFixture {
    runtime: Arc<Runtime>,
    _tempdir: tempfile::TempDir,
    driver: AuditDriver,
    deployments: Vec<LiveRemoteDeployment>,
    backend: AgentBackendConfig,
}

impl MultiAgentLiveDesktopFixture {
    fn shutdown(mut self) -> Result<()> {
        for deployment in self.deployments.drain(..) {
            self.runtime.block_on(deployment.running_agent.shutdown())?;
            self.runtime.block_on(deployment.core.shutdown())?;
        }
        self.driver.app.shutdown_client();
        Ok(())
    }
}

fn build_live_desktop_fixture(
    label: &str,
    log_store: Arc<DesktopLogStore>,
) -> Result<LiveDesktopFixture> {
    init_test_tracing();

    let backend = AgentBackendConfig::live_from_env()?;
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;

    let unique_label = format!("{label}-{}", uuid::Uuid::new_v4().simple());
    let running_agent = runtime.block_on(spawn_backed_agent(
        core.node_arc(),
        tempdir
            .path()
            .join("agent")
            .join(format!("{unique_label}.key")),
        &unique_label,
        &backend,
    ))?;
    let docs = runtime.block_on(seed_live_operator_documents(
        &core,
        &running_agent.did,
        &unique_label,
        &backend,
    ))?;
    runtime.block_on(core.refresh_store())?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        log_store,
    );
    app.state.activity = Activity::Chat;

    Ok(LiveDesktopFixture {
        runtime,
        _tempdir: tempdir,
        driver: AuditDriver::new(app, ctx),
        running_agent: Some(running_agent),
        docs,
        backend,
    })
}

fn build_multi_agent_live_desktop_fixture(
    label: &str,
    log_store: Arc<DesktopLogStore>,
) -> Result<MultiAgentLiveDesktopFixture> {
    init_test_tracing();

    let backend = AgentBackendConfig::live_from_env()?;
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        live_multi_server_core_options(),
    ))?;
    let mut deployments = Vec::new();

    for suffix in ["alpha", "bravo"] {
        let remote_core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path().join(format!("remote-{suffix}"))),
            live_multi_server_core_options(),
        ))?;
        let remote_addr = runtime.block_on(configure_live_test_replicators(
            &desktop_core,
            &remote_core,
            suffix,
        ))?;

        let unique_label = format!("{label}-{suffix}-{}", uuid::Uuid::new_v4().simple());
        let running_agent = runtime.block_on(spawn_backed_agent(
            remote_core.node_arc(),
            tempdir
                .path()
                .join(format!("agent-{suffix}"))
                .join(format!("{unique_label}.key")),
            &unique_label,
            &backend,
        ))?;
        let docs = runtime.block_on(seed_live_operator_documents(
            &remote_core,
            &running_agent.did,
            &unique_label,
            &backend,
        ))?;

        let deployment_label = format!("{} Server", title_case_ascii(suffix));
        let added = desktop_core.add_test_peer_status(
            &deployment_label,
            remote_addr.clone(),
            &running_agent.did,
            true,
        );
        wait_for_replicated_live_deployment_docs(
            runtime.as_ref(),
            &desktop_core,
            &deployment_label,
            &running_agent.did,
            &docs,
        )?;
        deployments.push(LiveRemoteDeployment {
            label: deployment_label,
            peer_id: added.peer_id,
            addr: remote_addr,
            agent_did: running_agent.did.clone(),
            core: remote_core,
            running_agent,
            docs,
        });
    }

    wait_for_refreshed_desktop_snapshot(
        runtime.as_ref(),
        &desktop_core,
        "multi-server desktop snapshot after remote setup",
        Duration::from_secs(60),
    )?;
    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(desktop_core)),
        Vec::new(),
        log_store,
    );
    app.state.activity = Activity::Chat;

    Ok(MultiAgentLiveDesktopFixture {
        runtime,
        _tempdir: tempdir,
        driver: AuditDriver::new(app, ctx),
        deployments,
        backend,
    })
}

fn wait_for_replicated_live_deployment_docs(
    runtime: &Runtime,
    desktop_core: &ClientCore,
    deployment_label: &str,
    agent_did: &str,
    docs: &LiveAgentDocs,
) -> Result<()> {
    let mut last_error = None;
    let result = wait_for_value(
        &format!("replicated live deployment docs for {deployment_label}"),
        Duration::from_secs(180),
        || match runtime.block_on(live_deployment_docs_available(
            desktop_core,
            agent_did,
            docs,
        )) {
            Ok(true) => Some(()),
            Ok(false) => None,
            Err(error) => {
                last_error = Some(error.to_string());
                None
            }
        },
    );

    result.with_context(|| {
        last_error.map_or_else(
            || format!("last {deployment_label} replication probe saw missing rows"),
            |error| format!("last {deployment_label} replication probe failed: {error}"),
        )
    })?;
    wait_for_refreshed_desktop_snapshot(
        runtime,
        desktop_core,
        &format!("desktop snapshot containing {deployment_label} docs"),
        Duration::from_secs(60),
    )
}

fn wait_for_refreshed_desktop_snapshot(
    runtime: &Runtime,
    desktop_core: &ClientCore,
    label: &str,
    timeout: Duration,
) -> Result<()> {
    let mut last_error = None;
    wait_for_value(label, timeout, || {
        match runtime.block_on(desktop_core.refresh_store()) {
            Ok(_) => Some(()),
            Err(error) => {
                last_error = Some(error.to_string());
                None
            }
        }
    })
    .with_context(|| {
        last_error.map_or_else(
            || format!("desktop snapshot did not refresh for {label}"),
            |error| format!("last desktop snapshot refresh failed for {label}: {error}"),
        )
    })
}

async fn live_deployment_docs_available(
    desktop_core: &ClientCore,
    agent_did: &str,
    docs: &LiveAgentDocs,
) -> Result<bool> {
    for (root, field, value) in [
        ("AgentPrincipal", "agent_did", agent_did),
        ("AgentBehavior", "behavior_id", docs.behavior_id.as_str()),
        ("InferenceBackend", "backend_id", docs.backend_id.as_str()),
        ("ToolSelection", "selection_id", docs.tool_selection_id.as_str()),
        (
            "InferenceProfile",
            "profile_id",
            docs.inference_profile_id.as_str(),
        ),
    ] {
        if !query_has_row_by_unique_field(desktop_core, root, field, value).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn query_has_row_by_unique_field(
    core: &ClientCore,
    root: &str,
    field: &str,
    value: &str,
) -> Result<bool> {
    let escaped_value = escape_graphql_string(value);
    let query = format!(
        r#"{{
            {root}(
                filter: {{ {field}: {{ _eq: "{escaped_value}" }} }},
                limit: 1
            ) {{
                {field}
            }}
        }}"#
    );
    let response = core.node().execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query {root}.{field}={value} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get(root))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(field))
        .and_then(Value::as_str)
        == Some(value))
}

async fn seed_live_operator_documents(
    core: &ClientCore,
    agent_did: &str,
    agent_name: &str,
    backend: &AgentBackendConfig,
) -> Result<LiveAgentDocs> {
    let behavior_id = default_behavior_id_for_agent(agent_did);
    let backend_id = format!("{agent_name}-backend");
    let tool_selection_id = format!("{behavior_id}:tools");
    let inference_profile_id = format!("{behavior_id}:profile");
    let scheduled_task_id = format!("{behavior_id}:scheduled-task");

    core.save_tool_selection(&ToolSelectionRow {
        selection_id: tool_selection_id.clone(),
        agent_did: Some(agent_did.to_string()),
        display_name: Some("Live Audit Tools".to_string()),
        enable_file_tools: Some(false),
        file_tools_mode: Some("readonly".to_string()),
        enable_bash: Some(false),
        bash_mode: Some("disabled".to_string()),
        cli_tool_names: vec![],
        enable_meta_tools: Some(false),
        delegate_to: vec![],
    })
    .await?;
    core.save_inference_profile(&InferenceProfileRow {
        profile_id: inference_profile_id.clone(),
        display_name: Some("Live Audit Profile".to_string()),
        context_window: Some(131072),
        max_output_tokens: Some(1024),
        max_turns: Some(12),
        temperature: Some(0.0),
        stream_batch_ms: Some(50),
        deadline_duration_secs: Some(300),
    })
    .await?;
    core.save_behavior(&AgentBehaviorRow {
        behavior_id: behavior_id.clone(),
        agent_did: Some(agent_did.to_string()),
        display_name: Some("Live Audit Default".to_string()),
        system_prompt: Some(
            "You are a terse desktop integration test agent. Follow exact reply instructions."
                .to_string(),
        ),
        backend_id: Some(backend_id.clone()),
        model_name: Some(backend.model_name.clone()),
        tool_selection_id: Some(tool_selection_id.clone()),
        inference_profile_id: Some(inference_profile_id.clone()),
        compaction_strategy: Some("StripThenSummarize".to_string()),
        compaction_threshold: Some(0.95),
        enabled: Some(true),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    })
    .await?;
    core.save_scheduled_task(&ScheduledTaskRow {
        task_id: scheduled_task_id.clone(),
        agent_did: Some(agent_did.to_string()),
        behavior_id: Some(behavior_id.clone()),
        name: Some("Live Audit Scheduled Task".to_string()),
        prompt: Some("Summarize the live audit queue.".to_string()),
        interval_secs: Some(3600),
        enabled: Some(true),
        next_run_at: Some("2035-01-01T00:00:00Z".to_string()),
        last_run_at: None,
        last_status: Some("ok".to_string()),
        last_error: None,
        run_count: Some(0),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        updated_at: None,
    })
    .await?;
    core.refresh_store().await?;

    Ok(LiveAgentDocs {
        behavior_id,
        backend_id,
        tool_selection_id,
        inference_profile_id,
        scheduled_task_id,
    })
}

fn live_multi_server_core_options() -> ClientCoreOptions {
    let mut options = ClientCoreOptions::local_only();
    // The live multi-agent harness runs several local Iroh endpoints in one
    // process. Keep default fetch concurrency so CAR/Bitswap cannot overwhelm
    // listeners, but allow enough local push budget for streaming responses.
    options.rate_limit_burst = 5_000;
    options.rate_limit_rate = 500.0;
    options.install_replicators_on_bootstrap = false;
    options
}

async fn configure_live_test_replicators(
    desktop_core: &ClientCore,
    remote_core: &ClientCore,
    label: &str,
) -> Result<String> {
    let desktop_addr =
        wait_for_connectable_iroh_addr(desktop_core, &format!("{label} desktop")).await?;
    let remote_addr = wait_for_connectable_iroh_addr(remote_core, label).await?;
    let desktop_peer_id = desktop_core.local_peer_id().to_string();
    let remote_peer_id = remote_core.local_peer_id().to_string();

    connect_peer_with_retry(
        desktop_core,
        &remote_addr,
        &remote_peer_id,
        &format!("desktop -> {label}"),
    )
    .await?;
    connect_peer_with_retry(
        remote_core,
        &desktop_addr,
        &desktop_peer_id,
        &format!("{label} -> desktop"),
    )
    .await?;

    set_replicator_with_retry(
        remote_core,
        &desktop_addr,
        &format!("{label} -> desktop replicator"),
        subscribed_collection_names_for_test(),
    )
    .await?;
    set_replicator_with_retry(
        desktop_core,
        &remote_addr,
        &format!("desktop -> {label} replicator"),
        desktop_origin_collection_names_for_test(),
    )
    .await?;

    Ok(remote_addr)
}

async fn wait_for_connectable_iroh_addr(core: &ClientCore, label: &str) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let addrs = core.p2p().listen_addresses().await?;
        if let Some(addr) = addrs
            .iter()
            .find(|addr| addr.contains("/p2p/") || addr.starts_with("endpoint"))
        {
            return Ok(addr.clone());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {label} listen address");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn connect_peer_with_retry(
    core: &ClientCore,
    addr: &str,
    peer_id: &str,
    label: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if is_connected_peer(core, peer_id).await? {
            return Ok(());
        }

        match core.p2p().connect_peer(addr).await {
            Ok(()) => {
                wait_for_connected_peer(core, peer_id, label).await?;
                return Ok(());
            }
            Err(error) => {
                if is_connected_peer(core, peer_id).await? {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out connecting {label} to {peer_id}: {error}");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

async fn is_connected_peer(core: &ClientCore, peer_id: &str) -> Result<bool> {
    let peers = core.p2p().connected_peers().await?;
    Ok(peers.iter().any(|peer| peer.contains(peer_id)))
}

async fn wait_for_connected_peer(core: &ClientCore, peer_id: &str, label: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if is_connected_peer(core, peer_id).await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for connected peer {peer_id} on {label}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn set_replicator_with_retry(
    core: &ClientCore,
    addr: &str,
    label: &str,
    collections: Vec<String>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match core
            .p2p()
            .add_replicator(collections.clone(), Some(addr), Vec::new(), None)
            .await
        {
            Ok(()) => return Ok(()),
            Err(error) => {
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out configuring {label}: {error}");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

fn subscribed_collection_names_for_test() -> Vec<String> {
    defra_agent_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .chain(defra_agent_protocol::schemas::ALL_COLLECTION_NAMES.iter())
        .map(|name| (*name).to_string())
        .collect()
}

fn desktop_origin_collection_names_for_test() -> Vec<String> {
    [
        defra_agent_protocol::schemas::INFERENCE_BACKEND_NAME,
        defra_agent_protocol::schemas::AGENT_BEHAVIOR_NAME,
        defra_agent_protocol::schemas::TOOL_SELECTION_NAME,
        defra_agent_protocol::schemas::INFERENCE_PROFILE_NAME,
        defra_agent_protocol::schemas::AGENT_CONVERSATION_NAME,
        defra_agent_protocol::schemas::AGENT_SESSION_NAME,
        defra_agent_protocol::schemas::AGENT_REQUEST_NAME,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn title_case_ascii(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn seed_saved_peer_directory(
    paths: &DesktopPaths,
    label: &str,
    addr: &str,
    agent_did: &str,
) -> Result<()> {
    std::fs::create_dir_all(paths.root())?;
    let payload = serde_json::json!({
        "peers": [{
            "peer_id": "peer-broken",
            "label": label,
            "addr": addr,
            "agent_did": agent_did,
            "created_at": "2026-04-14T00:00:00Z",
            "updated_at": "2026-04-14T00:00:00Z"
        }]
    });
    std::fs::write(
        paths.peer_directory_path(),
        serde_json::to_vec_pretty(&payload)?,
    )?;
    Ok(())
}

struct MockModelEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone)]
enum MockModelMode {
    Text,
    ToolLoop { final_text: String },
}

impl MockModelEndpoint {
    fn start(model_name: &str) -> Result<Self> {
        Self::start_with_mode(model_name, MockModelMode::Text)
    }

    fn start_tool_loop(model_name: &str, final_text: impl Into<String>) -> Result<Self> {
        Self::start_with_mode(
            model_name,
            MockModelMode::ToolLoop {
                final_text: final_text.into(),
            },
        )
    }

    fn start_with_mode(model_name: &str, mode: MockModelMode) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let model_name = model_name.to_string();
        let mode_for_thread = mode.clone();
        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = match read_http_request(&mut stream) {
                            Ok(request) => request,
                            Err(_) => {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                        };
                        let (status, content_type, body) = if request.method == "GET"
                            && (request.path == "/v1/models" || request.path == "/models")
                        {
                            (
                                "200 OK",
                                "application/json",
                                format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#),
                            )
                        } else if request.method == "POST"
                            && (request.path == "/v1/chat/completions"
                                || request.path == "/chat/completions")
                        {
                            let body = match &mode_for_thread {
                                MockModelMode::Text => mock_completion_sse("mock response"),
                                MockModelMode::ToolLoop { final_text } => {
                                    if request_has_tool_result_message(&request.body) {
                                        let text = extract_desktop_tool_token(&request.body)
                                            .unwrap_or_else(|| final_text.clone());
                                        mock_completion_sse(&text)
                                    } else {
                                        mock_tool_call_sse("read_file", r#"{"path":"notes.txt"}"#)
                                    }
                                }
                            };
                            ("200 OK", "text/event-stream", body)
                        } else {
                            (
                                "404 Not Found",
                                "application/json",
                                r#"{"error":"not found"}"#.to_string(),
                            )
                        };
                        let _ = write_http_response(&mut stream, status, content_type, &body);
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            handle: Some(handle),
        })
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

fn request_has_tool_result_message(body: &str) -> bool {
    body.contains(r#""role":"tool""#) || body.contains(r#""role": "tool""#)
}

fn extract_desktop_tool_token(body: &str) -> Option<String> {
    let marker = "DESKTOP_TOOL_TOKEN_";
    let start = body.find(marker)?;
    let token = body[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>();
    (!token.is_empty()).then_some(token)
}

fn mock_tool_call_sse(tool_name: &str, arguments: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call-read-file",
                    "function": {
                        "name": tool_name,
                        "arguments": ""
                    }
                }]
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_2 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": null,
                    "function": {
                        "name": null,
                        "arguments": arguments
                    }
                }]
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_3 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": null,
                "tool_calls": []
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 16,
            "completion_tokens": 4,
            "total_tokens": 20
        }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize mock tool chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize mock tool chunk 2"),
        serde_json::to_string(&chunk_3).expect("serialize mock tool chunk 3"),
    )
}

fn mock_completion_sse(text: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": text
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_2 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": null,
                "tool_calls": []
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 24,
            "completion_tokens": 6,
            "total_tokens": 30
        }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize mock completion chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize mock completion chunk 2"),
    )
}

impl Drop for MockModelEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct RunningAgent {
    did: String,
    tool_token: String,
    shutdown_tx: watch::Sender<bool>,
    run_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl RunningAgent {
    async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        self.run_task.await??;
        Ok(())
    }
}

#[derive(Debug)]
struct HttpRequestData {
    method: String,
    path: String,
    body: String,
}

#[derive(Debug, Clone)]
struct AgentBackendConfig {
    endpoint: String,
    model_name: String,
    provider_kind: BackendProviderKind,
    api_key: Option<String>,
    api_key_env_var: Option<String>,
}

impl AgentBackendConfig {
    fn mock(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            model_name: "default".to_string(),
            provider_kind: BackendProviderKind::OpenAiCompatible,
            api_key: None,
            api_key_env_var: None,
        }
    }

    fn live_from_env() -> Result<Self> {
        let endpoint = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT");
        let model_name = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL")
            .or_else(|| optional_env("DEFRA_AGENT_TEST_OPENROUTER_MODEL"))
            .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
        let provider_kind = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_PROVIDER");
        let api_key = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY");
        let api_key_env_var = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR");

        if endpoint.is_some()
            || provider_kind.is_some()
            || api_key.is_some()
            || api_key_env_var.is_some()
        {
            if let Some(env_var_name) = api_key_env_var.as_deref() {
                std::env::var(env_var_name).with_context(|| {
                    format!(
                        "set {env_var_name} because DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR points at it"
                    )
                })?;
            }

            return Ok(Self {
                endpoint: endpoint.context(
                    "set DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT for the live desktop smoke test",
                )?,
                model_name,
                provider_kind: BackendProviderKind::parse_optional(provider_kind.as_deref())?,
                api_key,
                api_key_env_var,
            });
        }

        if std::env::var("OPENROUTER_API_KEY").is_ok() {
            return Ok(Self {
                endpoint: "https://openrouter.ai/api/v1".to_string(),
                model_name,
                provider_kind: BackendProviderKind::OpenRouter,
                api_key: None,
                api_key_env_var: Some("OPENROUTER_API_KEY".to_string()),
            });
        }

        anyhow::bail!(
            "set DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT or OPENROUTER_API_KEY to run the live desktop smoke test"
        );
    }
}

fn test_runtime() -> Result<Arc<Runtime>> {
    Ok(Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .build()?,
    ))
}

fn shutdown_core(runtime: &Runtime, core: ClientCore) -> Result<()> {
    runtime.block_on(core.shutdown())
}

fn submit_chat_message_and_wait_for_response(
    driver: &mut AuditDriver,
    prompt: &str,
) -> Result<(String, String)> {
    submit_chat_message_and_wait_for_response_after_request(driver, prompt, |_, _| Ok(()))
}

fn submit_chat_message_and_wait_for_response_after_request(
    driver: &mut AuditDriver,
    prompt: &str,
    mut after_request: impl FnMut(&mut AuditDriver, &str) -> Result<()>,
) -> Result<(String, String)> {
    let prior_request_count = driver
        .app
        .client
        .as_ref()
        .map(|client| client.store().snapshot().requests.len())
        .ok_or_else(|| anyhow!("desktop client missing"))?;
    let prior_response_count = driver
        .app
        .client
        .as_ref()
        .map(|client| client.store().snapshot().responses.len())
        .ok_or_else(|| anyhow!("desktop client missing"))?;

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text(prompt);
    driver.click_target(audit::targets::CHAT_SEND);
    assert_eq!(driver.app.state.chat.last_submission_error, None);
    assert!(driver.app.state.chat.composer_text.is_empty());

    let request_id = wait_for_value(
        "focused request id after submission",
        Duration::from_secs(5),
        || {
            driver.app.client.as_ref().and_then(|client| {
                let snapshot = client.store().snapshot();
                (snapshot.requests.len() > prior_request_count)
                    .then(|| client.store().focused_request_id())
                    .flatten()
            })
        },
    )?;
    after_request(driver, &request_id)?;

    let mut next_response_refresh = Instant::now();
    let response_deadline = Instant::now() + Duration::from_secs(180);
    let response_text = loop {
        let client = Arc::clone(
            driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing while waiting for response"))?,
        );
        if Instant::now() >= next_response_refresh {
            driver.app.block_on_runtime(client.refresh_store())?;
            next_response_refresh = Instant::now() + Duration::from_secs(1);
        }

        let snapshot = client.store().snapshot();
        let request = snapshot
            .requests
            .iter()
            .find(|row| row.request_id == request_id);
        let response = snapshot.latest_response_for_request(&request_id);
        if let Some(response) = response {
            if matches!(response.status.as_deref(), Some("complete" | "completed")) {
                if let Some(content) = response.content.as_deref() {
                    if !content.trim().is_empty() {
                        break content.to_string();
                    }
                }
            }

            if matches!(response.status.as_deref(), Some("error" | "failed" | "failure")) {
                anyhow::bail!(
                    "response for request {request_id} reached error status while waiting for content: {}",
                    describe_response_wait_state(request, Some(response), prior_response_count, snapshot.responses.len())
                );
            }
        }

        if let Some(request) = request {
            if matches!(
                request.lifecycle_state.as_deref(),
                Some("failed" | "dead" | "superseded")
            ) {
                anyhow::bail!(
                    "request {request_id} reached terminal lifecycle before response content: {}",
                    describe_response_wait_state(Some(request), response, prior_response_count, snapshot.responses.len())
                );
            }
        }

        if Instant::now() >= response_deadline {
            anyhow::bail!(
                "timed out waiting for response content in client store after submission: {}",
                describe_response_wait_state(request, response, prior_response_count, snapshot.responses.len())
            );
        }

        std::thread::sleep(Duration::from_millis(50));
    };

    let rendered_response_text = response_text.trim();
    wait_for_value(
        "submitted prompt and response in transcript",
        Duration::from_secs(30),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains(prompt))
                .then_some(())
                .and_then(|_| {
                    texts
                        .iter()
                        .any(|text| text.contains(rendered_response_text))
                        .then_some(())
                })
        },
    )?;

    Ok((request_id, response_text))
}

fn describe_response_wait_state(
    request: Option<&defra_agent_protocol::row::AgentRequestRow>,
    response: Option<&defra_agent_protocol::row::AgentResponseRow>,
    prior_response_count: usize,
    current_response_count: usize,
) -> String {
    let request_summary = request.map_or_else(
        || "request=<missing>".to_string(),
        |row| {
            format!(
                "request={{status={}, lifecycle_state={}, agent_did={}, behavior_id={}, backend_id={}, execution_origin={}, failure_reason={}, claimed_at={}, deadline={}}}",
                optional_str(row.status.as_deref()),
                optional_str(row.lifecycle_state.as_deref()),
                optional_str(row.agent_did.as_deref()),
                optional_str(row.behavior_id.as_deref()),
                optional_str(row.backend_id.as_deref()),
                optional_str(row.execution_origin.as_deref()),
                optional_str(row.failure_reason.as_deref()),
                optional_str(row.claimed_at.as_deref()),
                optional_str(row.deadline.as_deref()),
            )
        },
    );
    let response_summary = response.map_or_else(
        || "response=<missing>".to_string(),
        |row| {
            format!(
                "response={{key={}, status={}, agent_did={}, behavior_id={}, error_message={}, content_len={}, progress_seq={}, completed_at={}}}",
                row.response_key,
                optional_str(row.status.as_deref()),
                optional_str(row.agent_did.as_deref()),
                optional_str(row.behavior_id.as_deref()),
                optional_str(row.error_message.as_deref()),
                row.content.as_deref().map(str::len).unwrap_or_default(),
                row.progress_seq.unwrap_or_default(),
                optional_str(row.completed_at.as_deref()),
            )
        },
    );
    format!(
        "{request_summary}; {response_summary}; responses_before_submit={prior_response_count}; responses_now={current_response_count}"
    )
}

fn optional_str(value: Option<&str>) -> &str {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("<empty>")
}

fn assert_operator_filter_round_trip(
    driver: &mut AuditDriver,
    section: OperatorSection,
    query: &str,
    target_id: &str,
    missing_query: &str,
) -> Result<()> {
    driver.click_target(&audit::targets::operator_section(section));
    driver.wait_for_target(
        "operator filter input",
        Duration::from_secs(10),
        audit::targets::OPERATOR_ENTITY_FILTER,
    )?;
    driver.replace_text_in_target(audit::targets::OPERATOR_ENTITY_FILTER, query);
    let filtered_texts = driver.render();
    assert_eq!(driver.app.state.operator.entity_filter, query);
    assert!(
        !filtered_texts
            .iter()
            .any(|text| text.contains("No Matches")),
        "operator filter unexpectedly hid {target_id} in {section:?}"
    );
    assert!(driver.has_target(&audit::targets::operator_entity(target_id)));

    driver.replace_text_in_target(audit::targets::OPERATOR_ENTITY_FILTER, missing_query);
    let no_match_texts = driver.render();
    assert!(
        no_match_texts
            .iter()
            .any(|text| text.contains("No Matches")),
        "operator filter did not render No Matches for {missing_query}"
    );

    driver.replace_text_in_target(audit::targets::OPERATOR_ENTITY_FILTER, "");
    driver.wait_for_target(
        "operator filtered row after clearing filter",
        Duration::from_secs(10),
        &audit::targets::operator_entity(target_id),
    )?;
    Ok(())
}

fn render_once(app: &mut DesktopApp, ctx: &egui::Context) -> Vec<String> {
    render_frame(app, ctx, 0.0, Vec::new())
        .into_iter()
        .map(|run| run.text)
        .collect()
}

fn audit_screen_rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 960.0))
}

fn target_is_interactable(rect: egui::Rect) -> bool {
    let visible = audit_screen_rect().shrink2(egui::vec2(8.0, 8.0));
    visible.intersects(rect) && visible.contains(rect.center())
}

#[derive(Debug, Clone)]
struct TextRun {
    text: String,
}

struct AuditDriver {
    app: DesktopApp,
    ctx: egui::Context,
    time: f64,
    last_texts: Vec<TextRun>,
}

impl AuditDriver {
    fn new(app: DesktopApp, ctx: egui::Context) -> Self {
        Self {
            app,
            ctx,
            time: 0.0,
            last_texts: Vec::new(),
        }
    }

    fn render(&mut self) -> Vec<String> {
        self.run_events(Vec::new())
    }

    fn click_target(&mut self, target: &str) -> Vec<String> {
        self.render();
        let rect = audit::target_rect(&self.ctx, target)
            .unwrap_or_else(|| panic!("unable to find audit target rect: {target}"));
        self.click_pos(rect.center())
    }

    fn click_interactable_target(&mut self, target: &str) -> Result<Vec<String>> {
        self.render();
        let rect = audit::target_interact_rect(&self.ctx, target)
            .ok_or_else(|| anyhow!("unable to find audit target rect: {target}"))?;
        anyhow::ensure!(
            target_is_interactable(rect),
            "audit target is not interactable: {target} at {rect:?}"
        );
        Ok(self.click_pos_compact(rect.center()))
    }

    fn has_target(&mut self, target: &str) -> bool {
        self.render();
        audit::target_rect(&self.ctx, target).is_some()
    }

    fn open_activity(&mut self, activity: Activity) -> Vec<String> {
        if self.app.state.activity != activity {
            let _ = self.click_target(audit::targets::activity(activity));
        }
        if self.app.state.activity != activity {
            self.app.state.activity = activity;
        }
        self.render()
    }

    fn wait_for_target(
        &mut self,
        description: &str,
        timeout: Duration,
        target: &str,
    ) -> Result<Vec<String>> {
        wait_for_value(description, timeout, || {
            let texts = self.render();
            audit::target_rect(&self.ctx, target).map(|_| texts)
        })
    }

    fn type_text(&mut self, text: &str) -> Vec<String> {
        self.run_events(vec![egui::Event::Text(text.to_string())])
    }

    fn press_key(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> Vec<String> {
        self.run_events(vec![
            egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers,
            },
        ])
    }

    fn replace_text_in_target(&mut self, target: &str, text: &str) -> Vec<String> {
        self.click_target(target);
        self.press_key(egui::Key::A, egui::Modifiers::COMMAND);
        self.press_key(egui::Key::Backspace, egui::Modifiers::NONE);
        self.type_text(text)
    }

    fn click_pos(&mut self, pos: egui::Pos2) -> Vec<String> {
        self.run_events(vec![egui::Event::PointerMoved(pos)]);
        self.run_events(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        self.run_events(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ])
    }

    fn click_pos_compact(&mut self, pos: egui::Pos2) -> Vec<String> {
        self.run_events(vec![egui::Event::PointerMoved(pos)]);
        self.run_events(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ])
    }

    fn scroll_pos(&mut self, pos: egui::Pos2, delta_y: f32) -> Vec<String> {
        self.run_events(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, delta_y),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            },
        ])
    }

    fn scroll_right_rail(&mut self, delta_y: f32) -> Vec<String> {
        self.scroll_pos(egui::pos2(1400.0, 480.0), delta_y)
    }

    fn scroll_right_rail_until_target(
        &mut self,
        description: &str,
        target: &str,
    ) -> Result<Vec<String>> {
        wait_for_value(description, Duration::from_secs(3), || {
            let texts = self.render();
            if audit::target_interact_rect(&self.ctx, target).is_some_and(target_is_interactable) {
                Some(texts)
            } else {
                self.scroll_right_rail(-280.0);
                None
            }
        })
    }

    fn run_events(&mut self, events: Vec<egui::Event>) -> Vec<String> {
        self.last_texts = render_frame(&mut self.app, &self.ctx, self.time, events);
        self.time += 1.0 / 60.0;
        self.last_texts.iter().map(|run| run.text.clone()).collect()
    }
}

fn render_frame(
    app: &mut DesktopApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<TextRun> {
    let mut frame = eframe::Frame::_new_kittest();
    app.logic(ctx, &mut frame);

    audit::begin_frame(ctx);
    let output = ctx.run_ui(test_raw_input(time, events), |ui| app.ui(ui, &mut frame));

    collect_text_runs(&output.shapes)
}

fn test_raw_input(time: f64, events: Vec<egui::Event>) -> egui::RawInput {
    let modifiers = events
        .iter()
        .rev()
        .find_map(|event| match event {
            egui::Event::Key { modifiers, .. }
            | egui::Event::PointerButton { modifiers, .. }
            | egui::Event::MouseWheel { modifiers, .. } => Some(*modifiers),
            _ => None,
        })
        .unwrap_or_default();
    egui::RawInput {
        screen_rect: Some(audit_screen_rect()),
        time: Some(time),
        modifiers,
        events,
        ..Default::default()
    }
}

fn collect_text_runs(shapes: &[egui::epaint::ClippedShape]) -> Vec<TextRun> {
    let mut texts = Vec::new();
    for shape in shapes {
        collect_shape_text(&shape.shape, &mut texts);
    }
    texts
}

fn collect_shape_text(shape: &egui::epaint::Shape, texts: &mut Vec<TextRun>) {
    match shape {
        egui::epaint::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_shape_text(shape, texts);
            }
        }
        egui::epaint::Shape::Text(text_shape) => {
            let text = text_shape.galley.text().trim();
            if !text.is_empty() {
                texts.push(TextRun {
                    text: text.to_string(),
                });
            }
        }
        _ => {}
    }
}

async fn spawn_backed_agent(
    node: Arc<EmbeddedNode>,
    key_path: impl Into<std::path::PathBuf>,
    name: &str,
    backend: &AgentBackendConfig,
) -> Result<RunningAgent> {
    let key_path = key_path.into();
    let tool_root = key_path
        .parent()
        .map(|parent| parent.join("tool-root"))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("defra-agent-tools-{name}")));
    std::fs::create_dir_all(&tool_root)
        .with_context(|| format!("creating live tool root {}", tool_root.display()))?;
    let tool_token = format!("DESKTOP_TOOL_TOKEN_{}", uuid::Uuid::new_v4().simple());
    std::fs::write(tool_root.join("notes.txt"), format!("{tool_token}\n")).with_context(|| {
        format!(
            "writing live tool fixture {}",
            tool_root.join("notes.txt").display()
        )
    })?;

    let identity = Arc::new(SimpleIdentity::new(name, key_path, None));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        &format!("{name}-backend"),
        backend,
    )
    .await?;
    let did = identity.did().to_string();
    let agent = DefraAgent::from_default_behavior_documents(
        Arc::clone(&node),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(tool_root),
            ..Default::default()
        },
    )
    .await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_process_state(node.as_ref(), &did, "ready").await?;
    Ok(RunningAgent {
        did,
        tool_token,
        shutdown_tx,
        run_task,
    })
}

async fn bind_default_behavior_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    backend: &AgentBackendConfig,
) -> Result<()> {
    let bootstrap = ensure_agent_principal(node, agent_did).await?;
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(&backend.endpoint);
    let escaped_provider_kind = escape_graphql_string(backend.provider_kind.as_str());
    let escaped_model_name = escape_graphql_string(&backend.model_name);
    let api_key_field = graphql_optional_string_field("api_key", backend.api_key.as_deref());
    let api_key_env_var_field =
        graphql_optional_string_field("api_key_env_var", backend.api_key_env_var.as_deref());
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "{escaped_provider_kind}",
                    endpoint: "{escaped_endpoint}",
                    {api_key_field}
                    {api_key_env_var_field}
                    max_concurrent: 1,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "{escaped_provider_kind}",
                    endpoint: "{escaped_endpoint}",
                    {api_key_field}
                    {api_key_env_var_field}
                    max_concurrent: 1,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("upsert inference backend failed: {:?}", response.errors);
    }

    let mut default_behavior =
        load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
            .await?
            .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    default_behavior.model_name = Some(backend.model_name.clone());
    upsert_agent_behavior(node, &default_behavior).await?;
    Ok(())
}

async fn wait_for_runtime_process_state(
    node: &EmbeddedNode,
    agent_did: &str,
    expected_process_state: &str,
) -> Result<()> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let query = format!(
            r#"{{
                AgentRuntime(
                    filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                    limit: 1
                ) {{
                    process_state
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("AgentRuntime query failed: {:?}", response.errors);
        }
        let process_state = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRuntime"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("process_state"))
            .and_then(Value::as_str);
        if process_state == Some(expected_process_state) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for AgentRuntime {agent_did} to reach process_state={expected_process_state}; last={process_state:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn init_test_tracing() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = tracing_subscriber::registry()
            .with(EnvFilter::new(
                "warn,\
                 defra_agent_desktop=info,\
                 defra_agent=info,\
                 defra_node=info,\
                 p2p=info,\
                 iroh=warn,\
                 reqwest=warn,\
                 hyper=warn,\
                 h2=warn",
            ))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .compact()
                    .without_time(),
            )
            .with(global_log_layer())
            .try_init();
    });
}

fn live_desktop_test_guard() -> MutexGuard<'static, ()> {
    static LIVE_DESKTOP_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LIVE_DESKTOP_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("live desktop test lock poisoned")
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn graphql_optional_string_field(name: &str, value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(r#"{name}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_default()
}

fn assert_logs_filter_has_results(texts: &[String]) {
    assert!(
        !texts.iter().any(|text| text.contains("No Matching Events")),
        "logs filter unexpectedly rendered empty state"
    );
}

fn wait_for_value<T>(
    label: &str,
    timeout: Duration,
    mut loader: impl FnMut() -> Option<T>,
) -> Result<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = loader() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {label}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequestData> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut buffer = Vec::new();
    let mut temp = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut temp)?;
        if read == 0 {
            anyhow::bail!("connection closed before headers");
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let mut content_length = 0_usize;
    for line in lines.clone() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or_default();
            }
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("missing request method"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| anyhow!("missing request path"))?
        .to_string();
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut temp)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&temp[..read]);
    }
    let body = String::from_utf8_lossy(&buffer[header_end..buffer.len().min(header_end + content_length)])
        .to_string();

    Ok(HttpRequestData { method, path, body })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}
