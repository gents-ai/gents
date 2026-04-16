use super::*;

fn refreshed_runtime_generation(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    agent_did: &str,
) -> Option<i64> {
    runtime.block_on(core.refresh_store()).ok()?;
    core.store()
        .snapshot()
        .latest_runtime(agent_did)
        .and_then(|row| row.router_generation.or(row.active_generation))
}

fn compact_field(value: Option<&str>) -> String {
    match value {
        Some(value) if value.len() > 96 => format!("{}...", &value[..96]),
        Some(value) => value.to_string(),
        None => "<none>".to_string(),
    }
}

fn describe_live_config_state(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    agent_did: &str,
    docs: &LiveAgentDocs,
    switch_backend_id: &str,
    switch_profile_id: &str,
) -> String {
    let refresh = runtime
        .block_on(core.refresh_store())
        .map(|_| "ok".to_string())
        .unwrap_or_else(|error| format!("error={error:#}"));
    let snapshot = core.store().snapshot();
    let behavior = snapshot
        .behaviors
        .iter()
        .find(|row| row.behavior_id == docs.behavior_id)
        .map(|row| {
            format!(
                "behavior(agent={:?}, backend={:?}, model={:?}, tool_selection={:?}, profile={:?}, enabled={:?}, prompt={})",
                row.agent_did,
                row.backend_id,
                row.model_name,
                row.tool_selection_id,
                row.inference_profile_id,
                row.enabled,
                compact_field(row.system_prompt.as_deref())
            )
        })
        .unwrap_or_else(|| "behavior=<missing>".to_string());
    let original_backend = snapshot
        .inference_backends
        .iter()
        .find(|row| row.backend_id == docs.backend_id)
        .map(|row| {
            format!(
                "original_backend(enabled={:?}, probe={:?}, endpoint={}, models={:?})",
                row.enabled,
                row.probe_status.as_deref(),
                compact_field(row.endpoint.as_deref()),
                row.models
            )
        })
        .unwrap_or_else(|| "original_backend=<missing>".to_string());
    let switch_backend = snapshot
        .inference_backends
        .iter()
        .find(|row| row.backend_id == switch_backend_id)
        .map(|row| {
            format!(
                "switch_backend(enabled={:?}, probe={:?}, endpoint={}, models={:?})",
                row.enabled,
                row.probe_status.as_deref(),
                compact_field(row.endpoint.as_deref()),
                row.models
            )
        })
        .unwrap_or_else(|| "switch_backend=<missing>".to_string());
    let tool_selection = snapshot
        .tool_selections
        .iter()
        .find(|row| row.selection_id == docs.tool_selection_id)
        .map(|row| {
            format!(
                "tools(agent={:?}, enable_file={:?}, file_mode={:?}, enable_bash={:?}, bash_mode={:?}, cli={:?}, meta={:?})",
                row.agent_did,
                row.enable_file_tools,
                row.file_tools_mode,
                row.enable_bash,
                row.bash_mode,
                row.cli_tool_names,
                row.enable_meta_tools
            )
        })
        .unwrap_or_else(|| "tools=<missing>".to_string());
    let switch_profile = snapshot
        .inference_profiles
        .iter()
        .find(|row| row.profile_id == switch_profile_id)
        .map(|row| {
            format!(
                "switch_profile(max_output={:?}, max_turns={:?}, temp={:?})",
                row.max_output_tokens, row.max_turns, row.temperature
            )
        })
        .unwrap_or_else(|| "switch_profile=<missing>".to_string());
    let runtime_row = snapshot
        .latest_runtime(agent_did)
        .map(|row| {
            format!(
                "runtime(process={:?}, phase={:?}, active={:?}, router={:?}, default={:?}, runnable={:?}, unavailable={:?}, result={:?}, error={})",
                row.process_state,
                row.reconcile_phase,
                row.active_generation,
                row.router_generation,
                row.default_behavior_id,
                row.runnable_behavior_count,
                row.unavailable_behavior_count,
                row.last_reconcile_result,
                compact_field(row.last_reconcile_error.as_deref())
            )
        })
        .unwrap_or_else(|| "runtime=<missing>".to_string());

    format!(
        "{label}: refresh={refresh}; {runtime_row}; {behavior}; {original_backend}; {switch_backend}; {tool_selection}; {switch_profile}"
    )
}

fn wait_for_stable_runtime_ready(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    agent_did: &str,
    stable_for: Duration,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut stable_since = None;
    let mut stable_generation = None;
    let mut last_state = "runtime=<missing>".to_string();

    loop {
        runtime.block_on(core.refresh_store())?;
        let snapshot = core.store().snapshot();
        let runtime_row = snapshot.latest_runtime(agent_did);
        let ready = runtime_row.is_some_and(|row| {
            let generation = row.router_generation.or(row.active_generation);
            last_state = format!(
                "generation={generation:?} runnable={:?} unavailable={:?} result={:?} error={}",
                row.runnable_behavior_count,
                row.unavailable_behavior_count,
                row.last_reconcile_result,
                compact_field(row.last_reconcile_error.as_deref())
            );
            generation.is_some()
                && row.runnable_behavior_count == Some(1)
                && row.unavailable_behavior_count == Some(0)
                && row
                    .last_reconcile_error
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
        });
        let generation =
            runtime_row.and_then(|row| row.router_generation.or(row.active_generation));

        if ready {
            match (stable_generation, generation) {
                (Some(stable), Some(current)) if stable == current => {}
                (_, Some(current)) => {
                    stable_generation = Some(current);
                    stable_since = Some(Instant::now());
                }
                _ => {
                    stable_generation = None;
                    stable_since = None;
                }
            }
            if stable_since.is_some_and(|since| since.elapsed() >= stable_for) {
                return Ok(());
            }
        } else {
            stable_generation = None;
            stable_since = None;
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for stable runtime ready for {label}; last={last_state}"
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn assert_live_submission_rows(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    deployment: &LiveDeploymentCase<'_>,
    submission: &LiveSubmissionCase,
    expected_backend_id: Option<&str>,
) -> Result<()> {
    wait_for_value(
        &format!("{label} submission rows for {}", deployment.label),
        Duration::from_secs(30),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let request = snapshot
                .requests
                .iter()
                .find(|row| row.request_id == submission.request_id)?;
            let request_ok = request.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                && request.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                && request.session_id.as_deref() == Some(submission.session_id.as_str())
                && expected_backend_id
                    .is_none_or(|backend_id| request.backend_id.as_deref() == Some(backend_id))
                && request.content.as_deref() == Some(submission.prompt.as_str())
                && request
                    .failure_reason
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                && !matches!(
                    request.lifecycle_state.as_deref(),
                    Some("failed" | "dead" | "superseded")
                );

            let response = snapshot.latest_response_for_request(&submission.request_id)?;
            let response_ok = response.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                && response.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                && response.session_id.as_deref() == Some(submission.session_id.as_str())
                && matches!(response.status.as_deref(), Some("complete" | "completed"))
                && response
                    .error_message
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                && response
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains(submission.response.trim()));

            let conversation_ok = snapshot
                .conversations
                .iter()
                .find(|row| row.session_id == submission.session_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                        && row.latest_request_id.as_deref() == Some(submission.request_id.as_str())
                });
            let session_ok = snapshot
                .sessions
                .iter()
                .find(|row| row.session_id == submission.session_id)
                .is_some_and(|row| {
                    row.behavior_id.as_deref() == Some(deployment.docs.behavior_id.as_str())
                });

            (request_ok && response_ok && conversation_ok && session_ok).then_some(())
        },
    )
}

fn assert_live_deployment_default_config(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    deployment: &LiveDeploymentCase<'_>,
    expected_model_name: &str,
) -> Result<()> {
    wait_for_value(
        &format!("{label} default config remains isolated"),
        Duration::from_secs(30),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let behavior_ok = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.backend_id.as_deref() == Some(deployment.docs.backend_id.as_str())
                        && row.inference_profile_id.as_deref()
                            == Some(deployment.docs.inference_profile_id.as_str())
                        && row.tool_selection_id.as_deref()
                            == Some(deployment.docs.tool_selection_id.as_str())
                        && row.model_name.as_deref() == Some(expected_model_name)
                        && row.enabled == Some(true)
                });
            let tools_ok = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == deployment.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.enable_file_tools == Some(false)
                        && row.enable_bash == Some(false)
                        && row.cli_tool_names.is_empty()
                        && row.delegate_to.is_empty()
                });
            let profile_ok = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == deployment.docs.inference_profile_id)
                .is_some_and(|row| {
                    row.max_output_tokens == Some(1024)
                        && row.max_turns == Some(12)
                        && row.temperature == Some(0.0)
                });
            (behavior_ok && tools_ok && profile_ok).then_some(())
        },
    )
}

#[test]
fn desktop_app_p2p_replicates_chat_request_path_to_remote_core() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    init_test_tracing();

    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        live_multi_server_core_options(),
    ))?;
    let remote_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote")),
        live_multi_server_core_options(),
    ))?;

    let remote_addr = runtime.block_on(async {
        let remote_addr =
            wait_for_connectable_iroh_addr(&remote_core, "request-path remote").await?;
        connect_peer_with_retry(
            &desktop_core,
            &remote_addr,
            remote_core.local_peer_id(),
            "request-path desktop -> remote",
        )
        .await?;
        set_replicator_with_retry(
            &desktop_core,
            &remote_addr,
            "request-path desktop -> remote replicator",
            vec![
                defra_agent_protocol::schemas::AGENT_CONVERSATION_NAME.to_string(),
                defra_agent_protocol::schemas::AGENT_SESSION_NAME.to_string(),
                defra_agent_protocol::schemas::AGENT_REQUEST_NAME.to_string(),
            ],
        )
        .await?;
        Ok::<_, anyhow::Error>(remote_addr)
    })?;

    let agent_did = format!("did:defra:p2p-repro-{}", uuid::Uuid::new_v4().simple());
    let conversation = runtime.block_on(desktop_core.create_conversation(&agent_did, None))?;
    let request = runtime.block_on(desktop_core.submit_request(
        &conversation.session_id,
        &agent_did,
        "replicate this request to the remote core",
        None,
    ))?;

    wait_for_value(
        "remote replicated AgentConversation",
        Duration::from_secs(60),
        || {
            runtime
                .block_on(query_has_row_by_unique_field(
                    &remote_core,
                    "AgentConversation",
                    "session_id",
                    &conversation.session_id,
                ))
                .ok()
                .filter(|has_row| *has_row)
                .map(|_| ())
        },
    )
    .with_context(|| format!("remote addr was {remote_addr}"))?;
    wait_for_value(
        "remote replicated AgentSession",
        Duration::from_secs(60),
        || {
            runtime
                .block_on(query_has_row_by_unique_field(
                    &remote_core,
                    "AgentSession",
                    "session_id",
                    &conversation.session_id,
                ))
                .ok()
                .filter(|has_row| *has_row)
                .map(|_| ())
        },
    )?;
    wait_for_value(
        "remote replicated AgentRequest",
        Duration::from_secs(60),
        || {
            runtime
                .block_on(query_has_row_by_unique_field(
                    &remote_core,
                    "AgentRequest",
                    "request_id",
                    &request.request_id,
                ))
                .ok()
                .filter(|has_row| *has_row)
                .map(|_| ())
        },
    )?;

    runtime.block_on(remote_core.shutdown())?;
    runtime.block_on(desktop_core.shutdown())?;
    Ok(())
}

#[test]
fn desktop_app_p2p_replicates_config_docs_to_multiple_remote_cores() -> Result<()> {
    init_test_tracing();

    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        live_multi_server_core_options(),
    ))?;
    let alpha_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote-alpha")),
        live_multi_server_core_options(),
    ))?;
    let bravo_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote-bravo")),
        live_multi_server_core_options(),
    ))?;

    runtime.block_on(configure_live_test_replicators(
        &desktop_core,
        &alpha_core,
        "config alpha",
    ))?;
    runtime.block_on(configure_live_test_replicators(
        &desktop_core,
        &bravo_core,
        "config bravo",
    ))?;

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let alpha_agent_did = format!("did:defra:p2p-config-alpha-{suffix}");
    let bravo_agent_did = format!("did:defra:p2p-config-bravo-{suffix}");
    let alpha_docs =
        seed_desktop_origin_config_docs(&runtime, &desktop_core, "alpha", &alpha_agent_did)?;
    let bravo_docs =
        seed_desktop_origin_config_docs(&runtime, &desktop_core, "bravo", &bravo_agent_did)?;

    wait_for_remote_config_docs(
        &runtime,
        &alpha_core,
        "alpha remote owner config docs",
        &alpha_docs,
    )?;
    wait_for_remote_config_docs(
        &runtime,
        &bravo_core,
        "bravo remote owner config docs",
        &bravo_docs,
    )?;

    runtime.block_on(desktop_core.shutdown())?;
    runtime.block_on(alpha_core.shutdown())?;
    runtime.block_on(bravo_core.shutdown())?;
    Ok(())
}

fn seed_desktop_origin_config_docs(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    agent_did: &str,
) -> Result<LiveAgentDocs> {
    let behavior_id = format!("{agent_did}:default");
    let backend_id = format!("{label}-desktop-origin-backend");
    let tool_selection_id = format!("{behavior_id}:tools");
    let inference_profile_id = format!("{behavior_id}:profile");
    let scheduled_task_id = format!("{behavior_id}:scheduled-task");

    runtime.block_on(async {
        core.save_backend(&InferenceBackendRow {
            backend_id: backend_id.clone(),
            name: Some(format!("{label} Desktop Origin Backend")),
            provider_kind: Some("openai-compatible".to_string()),
            endpoint: Some("http://127.0.0.1:65535/v1".to_string()),
            api_key: None,
            api_key_env_var: None,
            max_concurrent: Some(1),
            max_queue_depth: Some(10),
            enabled: Some(true),
            models: vec!["local-test-model".to_string()],
            last_probe: None,
            probe_status: Some("healthy".to_string()),
        })
        .await?;
        core.save_inference_profile(&InferenceProfileRow {
            profile_id: inference_profile_id.clone(),
            display_name: Some(format!("{label} Desktop Origin Profile")),
            context_window: Some(8192),
            max_output_tokens: Some(256),
            max_turns: Some(8),
            temperature: Some(0.0),
            stream_batch_ms: Some(50),
            deadline_duration_secs: Some(60),
        })
        .await?;
        core.save_tool_selection(&ToolSelectionRow {
            selection_id: tool_selection_id.clone(),
            agent_did: Some(agent_did.to_string()),
            display_name: Some(format!("{label} Desktop Origin Tools")),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".to_string()),
            enable_bash: Some(false),
            bash_mode: Some("Off".to_string()),
            cli_tool_names: vec![],
            enable_meta_tools: Some(false),
            delegate_to: vec![],
        })
        .await?;
        core.save_behavior(&AgentBehaviorRow {
            behavior_id: behavior_id.clone(),
            agent_did: Some(agent_did.to_string()),
            display_name: Some(format!("{label} Desktop Origin Behavior")),
            system_prompt: Some(format!("{label} desktop-origin config replication probe")),
            backend_id: Some(backend_id.clone()),
            model_name: Some("local-test-model".to_string()),
            tool_selection_id: Some(tool_selection_id.clone()),
            inference_profile_id: Some(inference_profile_id.clone()),
            compaction_strategy: Some("StripThenSummarize".to_string()),
            compaction_threshold: Some(0.9),
            enabled: Some(true),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
        })
        .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(LiveAgentDocs {
        behavior_id,
        backend_id,
        tool_selection_id,
        inference_profile_id,
        scheduled_task_id,
    })
}

fn wait_for_remote_config_docs(
    runtime: &tokio::runtime::Runtime,
    remote_core: &ClientCore,
    label: &str,
    docs: &LiveAgentDocs,
) -> Result<()> {
    wait_for_value(label, Duration::from_secs(60), || {
        let has_rows = runtime
            .block_on(async {
                Ok::<_, anyhow::Error>(
                    query_has_row_by_unique_field(
                        remote_core,
                        "InferenceBackend",
                        "backend_id",
                        &docs.backend_id,
                    )
                    .await?
                        && query_has_row_by_unique_field(
                            remote_core,
                            "InferenceProfile",
                            "profile_id",
                            &docs.inference_profile_id,
                        )
                        .await?
                        && query_has_row_by_unique_field(
                            remote_core,
                            "ToolSelection",
                            "selection_id",
                            &docs.tool_selection_id,
                        )
                        .await?
                        && query_has_row_by_unique_field(
                            remote_core,
                            "AgentBehavior",
                            "behavior_id",
                            &docs.behavior_id,
                        )
                        .await?,
                )
            })
            .ok()?;
        has_rows.then_some(())
    })
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_inference_smoke() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    init_test_tracing();

    let backend = AgentBackendConfig::live_from_env()?;
    let live_backend_id = "audit-live-remote-backend".to_string();
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("peer")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer_addr = peer
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("live peer missing listen address"))?;
    let baseline_events = global_log_store().snapshot().total_events;
    let running_agent = runtime.block_on(spawn_backed_agent(
        core.node_arc(),
        tempdir.path().join("agent").join("audit-live.key"),
        "audit-live-remote",
        &backend,
    ))?;
    let live_agent_did = running_agent.did.clone();
    runtime.block_on(core.refresh_store())?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        global_log_store(),
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    let prompt = format!(
        "Reply with exactly READY and nothing else. audit {}",
        uuid::Uuid::new_v4()
    );
    let prompt_snippet = "Reply with exactly READY";

    let first_session_id = ensure_chat_session_selected(
        &mut driver,
        "live first conversation selected",
        Duration::from_secs(10),
    )?;
    wait_for_value(
        "live transcript empty state",
        Duration::from_secs(5),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("Transcript Empty"))
                .then_some(())
        },
    )?;

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text(&prompt);
    driver.render();
    driver.click_target(audit::targets::CHAT_SEND);
    assert_eq!(driver.app.state.chat.last_submission_error, None);

    let request_id = wait_for_value("live focused request id", Duration::from_secs(10), || {
        driver
            .app
            .client
            .as_ref()
            .and_then(|client| client.store().focused_request_id())
    })?;
    let response_content = wait_for_value(
        "live response row in client store",
        Duration::from_secs(90),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .latest_response_for_request(&request_id)
                    .and_then(|row| row.content.clone())
                    .filter(|content| !content.trim().is_empty())
            })
        },
    )?;
    assert!(!response_content.trim().is_empty());
    let (
        request_lifecycle_state,
        response_status,
        runtime_process_state,
        runtime_default_behavior_id,
        runtime_last_result,
        runtime_runnable_behaviors,
        runtime_scheduled_task_count,
        _initial_live_store_row_count,
    ) = wait_for_value(
        "live operator rows available",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                let snapshot = client.store().snapshot();
                let request = snapshot
                    .requests
                    .iter()
                    .find(|row| row.request_id == request_id)?;
                let response = snapshot.latest_response_for_request(&request_id)?;
                let runtime_row = snapshot.latest_runtime(&running_agent.did)?;
                let backend_row = snapshot
                    .inference_backends
                    .iter()
                    .find(|row| row.backend_id == live_backend_id)?;

                Some((
                    request
                        .lifecycle_state
                        .clone()
                        .unwrap_or_else(|| "unset".to_string()),
                    response
                        .status
                        .clone()
                        .unwrap_or_else(|| "unset".to_string()),
                    runtime_row
                        .process_state
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    runtime_row
                        .default_behavior_id
                        .clone()
                        .unwrap_or_else(|| "unbound".to_string()),
                    runtime_row
                        .last_reconcile_result
                        .clone()
                        .unwrap_or_else(|| "pending".to_string()),
                    runtime_row
                        .runnable_behavior_count
                        .unwrap_or_default()
                        .to_string(),
                    snapshot
                        .scheduled_tasks
                        .iter()
                        .filter(|row| row.agent_did.as_deref() == Some(running_agent.did.as_str()))
                        .count()
                        .to_string(),
                    snapshot.row_count().to_string(),
                ))
                .filter(|_| {
                    backend_row.provider_kind.as_deref() == Some(backend.provider_kind.as_str())
                        && backend_row.endpoint.as_deref() == Some(backend.endpoint.as_str())
                        && backend_row
                            .models
                            .iter()
                            .any(|model| model == &backend.model_name)
                })
            })
        },
    )?;

    let chat_texts = wait_for_value(
        "live response in transcript",
        Duration::from_secs(30),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains(prompt_snippet))
                .then_some(texts)
        },
    )?;
    assert!(chat_texts
        .iter()
        .any(|text| text.contains(response_content.trim())));

    let second_prompt = format!(
        "Reply with exactly SECOND_READY and nothing else. audit {}",
        uuid::Uuid::new_v4()
    );
    let second_session = {
        let client = Arc::clone(
            driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        driver
            .app
            .runtime
            .block_on(client.create_conversation(&live_agent_did, None))?
    };
    let second_session_target = audit::targets::chat_conversation(&second_session.session_id);
    driver.wait_for_target(
        "live second conversation row",
        Duration::from_secs(10),
        &second_session_target,
    )?;
    driver.click_target(&second_session_target);
    assert_eq!(
        driver.app.state.chat.selected_session_id.as_deref(),
        Some(second_session.session_id.as_str())
    );
    let (_, second_response_content) =
        submit_chat_message_and_wait_for_observed_response(&mut driver, &second_prompt)?;

    let multi_turn_prompt = format!(
        "This is the second turn in the same conversation. Reply with exactly MULTI_TURN_READY and nothing else. audit {}",
        uuid::Uuid::new_v4()
    );
    let (_, multi_turn_response_content) =
        submit_chat_message_and_wait_for_observed_response(&mut driver, &multi_turn_prompt)?;
    let second_session_request_count = driver
        .app
        .client
        .as_ref()
        .map(|client| {
            client
                .store()
                .snapshot()
                .requests
                .iter()
                .filter(|row| row.session_id.as_deref() == Some(second_session.session_id.as_str()))
                .count()
        })
        .ok_or_else(|| anyhow!("desktop client missing"))?;
    assert_eq!(second_session_request_count, 2);

    let first_session_target = audit::targets::chat_conversation(&first_session_id);
    driver.wait_for_target(
        "live first conversation row",
        Duration::from_secs(10),
        &first_session_target,
    )?;
    let first_conversation_texts = driver.click_target(&first_session_target);
    assert_eq!(
        driver.app.state.chat.selected_session_id.as_deref(),
        Some(first_session_id.as_str())
    );
    assert!(first_conversation_texts
        .iter()
        .any(|text| text.contains(prompt_snippet)));
    assert!(first_conversation_texts
        .iter()
        .any(|text| text.contains(response_content.trim())));

    let second_conversation_texts = driver.click_target(&second_session_target);
    assert_eq!(
        driver.app.state.chat.selected_session_id.as_deref(),
        Some(second_session.session_id.as_str())
    );
    assert!(second_conversation_texts
        .iter()
        .any(|text| text.contains(&second_prompt)));
    assert!(second_conversation_texts
        .iter()
        .any(|text| text.contains(&multi_turn_prompt)));
    assert!(second_conversation_texts
        .iter()
        .any(|text| text.contains(second_response_content.trim())));
    assert!(second_conversation_texts
        .iter()
        .any(|text| text.contains(multi_turn_response_content.trim())));

    driver.open_activity(Activity::Operator);
    driver.click_target(&audit::targets::operator_section(
        crate::state::OperatorSection::Runtime,
    ));
    let runtime_texts = wait_for_value(
        "operator runtime inspector",
        Duration::from_secs(10),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("Runtime Inspector"))
                .then_some(texts)
        },
    )?;
    assert!(runtime_texts
        .iter()
        .any(|text| text.contains(&runtime_process_state)));
    assert!(runtime_texts
        .iter()
        .any(|text| text.contains(&runtime_default_behavior_id)));
    assert!(runtime_texts
        .iter()
        .any(|text| text.contains(&runtime_last_result)));
    assert!(runtime_texts
        .iter()
        .any(|text| text.contains(&runtime_runnable_behaviors)));
    assert!(runtime_texts
        .iter()
        .any(|text| text.contains(&runtime_scheduled_task_count)));

    driver.click_target(&audit::targets::operator_section(
        crate::state::OperatorSection::RequestTimeline,
    ));
    let operator_texts = wait_for_value(
        "live request row in operator timeline",
        Duration::from_secs(10),
        || {
            driver
                .wait_for_target(
                    "operator request row",
                    Duration::from_millis(250),
                    &audit::targets::operator_entity(&request_id),
                )
                .ok()?;
            let texts = driver.click_target(&audit::targets::operator_entity(&request_id));
            texts
                .iter()
                .any(|text| text.contains("Request Detail"))
                .then_some(texts)
        },
    )?;
    assert_eq!(
        driver.app.state.operator.selected_entity_id.as_deref(),
        Some(request_id.as_str())
    );
    assert!(operator_texts
        .iter()
        .any(|text| text.contains(prompt_snippet)));
    assert!(operator_texts
        .iter()
        .any(|text| text.contains(&request_lifecycle_state)));
    assert!(operator_texts
        .iter()
        .any(|text| text.contains(&response_status)));
    assert!(operator_texts
        .iter()
        .any(|text| text.contains(response_content.trim())));

    driver.click_target(&audit::targets::operator_section(
        crate::state::OperatorSection::Backends,
    ));
    driver.wait_for_target(
        "live backend entity",
        Duration::from_secs(10),
        &audit::targets::operator_entity(&live_backend_id),
    )?;
    let backend_texts = driver.click_target(&audit::targets::operator_entity(&live_backend_id));
    assert_eq!(
        driver.app.state.operator.selected_entity_id.as_deref(),
        Some(live_backend_id.as_str())
    );
    assert!(backend_texts
        .iter()
        .any(|text| text.contains("Provider Kind")));
    assert!(backend_texts
        .iter()
        .any(|text| text.contains(backend.endpoint.as_str())));
    match driver.app.state.operator.draft.as_ref() {
        Some(OperatorDraft::Backend(draft)) => {
            assert_eq!(draft.backend_id, live_backend_id);
            assert_eq!(draft.provider_kind, backend.provider_kind.as_str());
            assert_eq!(draft.endpoint, backend.endpoint);
            assert_eq!(draft.models, backend.model_name);
            assert!(draft.enabled);
            assert_eq!(draft.max_queue_depth, "100");
        }
        other => panic!("expected backend draft in live smoke, got {other:?}"),
    }

    driver.open_activity(Activity::Peers);
    let peers_texts = driver.wait_for_target(
        "peers add-deployment form",
        Duration::from_secs(10),
        audit::targets::PEERS_ADD_LABEL,
    )?;
    assert!(peers_texts
        .iter()
        .any(|text| text.contains("Add Your First Deployment")));
    driver.click_target(audit::targets::PEERS_ONBOARDING_COPY_DID);
    assert_eq!(
        driver.app.state.peers.last_action_message.as_deref(),
        Some("Copied desktop DID to clipboard.")
    );
    driver.click_target(audit::targets::PEERS_ADD_LABEL);
    driver.type_text("Scratch Remote");
    driver.click_target(audit::targets::PEERS_ADD_ADDR);
    driver.type_text("iroh://scratch-address");
    driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
    driver.type_text("did:defra:scratch");
    driver.click_target(audit::targets::PEERS_CLEAR);
    assert!(driver.app.state.peers.add_label.is_empty());
    assert!(driver.app.state.peers.add_addr.is_empty());
    assert!(driver.app.state.peers.add_agent_did.is_empty());
    assert_eq!(driver.app.state.peers.last_action_message, None);
    driver.click_target(audit::targets::PEERS_ADD_LABEL);
    driver.type_text("Live Remote");
    driver.click_target(audit::targets::PEERS_ADD_ADDR);
    driver.type_text(&peer_addr);
    driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
    driver.type_text("did:defra:peer-live");
    driver.click_target(audit::targets::PEERS_SAVE);
    let live_peer_id = wait_for_value("live peer added", Duration::from_secs(10), || {
        driver.app.client.as_ref().and_then(|client| {
            let records = driver.app.runtime.block_on(client.peer_records());
            records
                .iter()
                .find(|record| record.label == "Live Remote")
                .map(|record| record.peer_id.clone())
        })
    })?;
    let live_peer_target = audit::targets::peers_peer(&live_peer_id);
    driver.wait_for_target(
        "live peer row after save",
        Duration::from_secs(10),
        &live_peer_target,
    )?;
    driver.click_interactable_target(audit::targets::PEERS_MAIN_COPY_DID)?;
    assert_eq!(
        driver.app.state.peers.last_action_message.as_deref(),
        Some("Copied desktop DID to clipboard.")
    );
    driver.click_target(&live_peer_target);
    assert_eq!(
        driver.app.state.peers.selected_peer_id.as_deref(),
        Some(live_peer_id.as_str())
    );
    driver
        .scroll_right_rail_until_target("peers remove saved peer", audit::targets::PEERS_REMOVE)?;
    driver.click_interactable_target(audit::targets::PEERS_REMOVE)?;
    wait_for_value("live peer removed", Duration::from_secs(10), || {
        driver
            .app
            .client
            .as_ref()
            .filter(|client| client.configured_peer_count() == 0)
            .map(|_| ())
    })?;

    global_log_store().record_manual(
        chrono::Utc::now(),
        tracing::Level::INFO,
        "defra_agent_desktop::replication",
        "live audit replication marker",
        [("request_id", request_id.clone())],
    );
    global_log_store().record_manual(
        chrono::Utc::now(),
        tracing::Level::WARN,
        "defra_agent_desktop::peer",
        "live audit warning marker",
        [("peer_id", "peer-live".to_string())],
    );

    let live_store_row_count = driver
        .app
        .client
        .as_ref()
        .map(|client| client.store().snapshot().row_count().to_string())
        .ok_or_else(|| anyhow!("desktop client missing"))?;
    let logs_texts = driver.open_activity(Activity::Logs);
    assert!(logs_texts.iter().any(|text| text.contains("Live Logs")));
    assert!(logs_texts.iter().any(|text| text.contains("approx store")));
    assert!(logs_texts
        .iter()
        .any(|text| text.contains(&format!("/ {live_store_row_count} rows"))));
    assert!(logs_texts
        .iter()
        .any(|text| text.contains("peers               0/0 connected")));
    assert!(logs_texts
        .iter()
        .any(|text| text.contains("latest warning")));
    assert!(logs_texts
        .iter()
        .any(|text| text.contains("live audit replication marker")));
    assert!(logs_texts
        .iter()
        .any(|text| text.contains("live audit warning marker")));
    driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
        crate::telemetry::DesktopLogCategory::Warnings,
    )));
    let warning_texts = driver.render();
    assert!(warning_texts
        .iter()
        .any(|text| text.contains("live audit warning marker")));
    driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
        crate::telemetry::DesktopLogCategory::Replication,
    )));
    let replication_texts = driver.render();
    assert!(replication_texts
        .iter()
        .any(|text| text.contains("live audit replication marker")));
    assert!(global_log_store().snapshot().total_events > baseline_events);

    let returned_chat_texts = driver.open_activity(Activity::Chat);
    assert_eq!(
        driver.app.state.chat.selected_session_id.as_deref(),
        Some(second_session.session_id.as_str())
    );
    assert!(returned_chat_texts
        .iter()
        .any(|text| text.contains(&second_prompt)));
    assert!(returned_chat_texts
        .iter()
        .any(|text| text.contains(&multi_turn_prompt)));
    assert!(returned_chat_texts
        .iter()
        .any(|text| text.contains(second_response_content.trim())));
    assert!(returned_chat_texts
        .iter()
        .any(|text| text.contains(multi_turn_response_content.trim())));

    runtime.block_on(running_agent.shutdown())?;
    driver.app.shutdown_client();
    shutdown_core(runtime.as_ref(), peer)?;
    Ok(())
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_multi_agent_server_switching_and_config_inference() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture =
        build_multi_agent_live_desktop_fixture("audit-live-multi-server", global_log_store())?;
    assert_eq!(fixture.deployments.len(), 2);

    let alpha = live_deployment_case(&fixture.deployments[0]);
    let bravo = live_deployment_case(&fixture.deployments[1]);
    let backend = fixture.backend.clone();
    let alpha_switch_backend_id = format!("{}:switch-backend", alpha.docs.behavior_id);
    let alpha_switch_profile_id = format!("{}:switch-profile", alpha.docs.behavior_id);
    let alpha_tool_prompt = "When the user asks you to read a local file, you must call the read_file tool instead of guessing. The token is not available in the conversation. If they ask for a token from a file, call read_file first and then respond with only that token.";
    let desktop_client = Arc::clone(
        fixture
            .driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );

    {
        fixture.runtime.block_on(async {
            desktop_client
                .save_backend(&InferenceBackendRow {
                    backend_id: alpha_switch_backend_id.clone(),
                    name: Some("Alpha Switch Backend".to_string()),
                    provider_kind: Some(backend.provider_kind.as_str().to_string()),
                    endpoint: Some(backend.endpoint.clone()),
                    api_key: backend.api_key.clone(),
                    api_key_env_var: backend.api_key_env_var.clone(),
                    max_concurrent: Some(1),
                    max_queue_depth: Some(100),
                    enabled: Some(true),
                    models: vec![backend.model_name.clone()],
                    last_probe: None,
                    probe_status: Some("healthy".to_string()),
                })
                .await?;
            desktop_client
                .save_inference_profile(&InferenceProfileRow {
                    profile_id: alpha_switch_profile_id.clone(),
                    display_name: Some("Alpha Switch Profile".to_string()),
                    context_window: Some(65536),
                    max_output_tokens: Some(2048),
                    max_turns: Some(16),
                    temperature: Some(0.0),
                    stream_batch_ms: Some(40),
                    deadline_duration_secs: Some(240),
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;
    }

    wait_for_value(
        "alpha switch backend saved in live desktop store",
        Duration::from_secs(20),
        || {
            fixture
                .runtime
                .block_on(desktop_client.refresh_store())
                .ok()?;
            let snapshot = desktop_client.store().snapshot();
            let has_backend = snapshot
                .inference_backends
                .iter()
                .any(|row| row.backend_id == alpha_switch_backend_id);
            let has_profile = snapshot
                .inference_profiles
                .iter()
                .any(|row| row.profile_id == alpha_switch_profile_id);
            (has_backend && has_profile).then_some(())
        },
    )?;

    let alpha_initial_generation = refreshed_runtime_generation(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        &alpha.agent_did,
    )
    .unwrap_or_default();
    let alpha_remote_initial_generation = refreshed_runtime_generation(
        fixture.runtime.as_ref(),
        alpha.remote_core,
        &alpha.agent_did,
    )
    .unwrap_or_default();

    let (alpha_submission, bravo_submission);
    {
        let driver = &mut fixture.driver;
        alpha_submission = submit_live_prompt_for_deployment(driver, &alpha, "ALPHA_SERVER_READY")?;
        bravo_submission = submit_live_prompt_for_deployment(driver, &bravo, "BRAVO_SERVER_READY")?;
    }
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop alpha initial",
        &alpha,
        &alpha_submission,
        None,
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        alpha.remote_core,
        "remote alpha initial",
        &alpha,
        &alpha_submission,
        None,
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop bravo initial",
        &bravo,
        &bravo_submission,
        None,
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        bravo.remote_core,
        "remote bravo initial",
        &bravo,
        &bravo_submission,
        None,
    )?;

    {
        let driver = &mut fixture.driver;
        driver.open_activity(Activity::Chat);
        driver.click_target(&audit::targets::chat_deployment(&alpha.peer_id));
        driver.click_target(&audit::targets::chat_agent(&alpha.agent_did));
        assert_chat_context(driver, &alpha, None);
        let alpha_texts = driver.click_target(&audit::targets::chat_conversation(
            &alpha_submission.session_id,
        ));
        assert_chat_context(driver, &alpha, Some(alpha_submission.session_id.as_str()));
        assert!(alpha_texts
            .iter()
            .any(|text| text.contains(alpha_submission.prompt.as_str())));
        assert!(alpha_texts
            .iter()
            .any(|text| text.contains(alpha_submission.response.trim())));
        assert!(
            !alpha_texts
                .iter()
                .any(|text| text.contains(bravo_submission.prompt.as_str())),
            "alpha transcript leaked bravo prompt after switching deployments"
        );

        driver.click_target(&audit::targets::chat_deployment(&bravo.peer_id));
        driver.click_target(&audit::targets::chat_agent(&bravo.agent_did));
        assert_chat_context(driver, &bravo, None);
        let bravo_texts = driver.click_target(&audit::targets::chat_conversation(
            &bravo_submission.session_id,
        ));
        assert_chat_context(driver, &bravo, Some(bravo_submission.session_id.as_str()));
        assert!(bravo_texts
            .iter()
            .any(|text| text.contains(bravo_submission.prompt.as_str())));
        assert!(bravo_texts
            .iter()
            .any(|text| text.contains(bravo_submission.response.trim())));
        assert!(
            !bravo_texts
                .iter()
                .any(|text| text.contains(alpha_submission.prompt.as_str())),
            "bravo transcript leaked alpha prompt after switching deployments"
        );

        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_deployment(&alpha.peer_id));
        driver.click_target(&audit::targets::operator_agent(&alpha.agent_did));
        driver.click_target(&audit::targets::operator_section(
            OperatorSection::Behaviors,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::Behaviors, None);
        driver.wait_for_target(
            "alpha behavior row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.behavior_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&bravo.docs.behavior_id)));
        driver.click_target(&audit::targets::operator_entity(&alpha.docs.behavior_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::Behaviors,
            Some(alpha.docs.behavior_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => {
                assert_eq!(draft.behavior_id, alpha.docs.behavior_id);
                assert_eq!(draft.agent_did, alpha.agent_did);
                assert_eq!(draft.backend_id, alpha.docs.backend_id);
                assert_eq!(draft.tool_selection_id, alpha.docs.tool_selection_id);
                assert_eq!(draft.inference_profile_id, alpha.docs.inference_profile_id);
            }
            other => panic!("expected alpha behavior draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(OperatorSection::Backends));
        assert_operator_context(driver, &alpha, OperatorSection::Backends, None);
        driver.wait_for_target(
            "alpha backend row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.backend_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&bravo.docs.backend_id)));
        driver.click_target(&audit::targets::operator_entity(&alpha.docs.backend_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::Backends,
            Some(alpha.docs.backend_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Backend(draft)) => {
                assert_eq!(draft.backend_id, alpha.docs.backend_id);
                assert_eq!(draft.provider_kind, backend.provider_kind.as_str());
                assert_eq!(draft.endpoint, backend.endpoint);
                assert!(draft.models.contains(backend.model_name.as_str()));
            }
            other => panic!("expected alpha backend draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::InferenceProfiles,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::InferenceProfiles, None);
        driver.wait_for_target(
            "alpha inference profile row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.inference_profile_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &bravo.docs.inference_profile_id
        )));
        driver.click_target(&audit::targets::operator_entity(
            &alpha.docs.inference_profile_id,
        ));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::InferenceProfiles,
            Some(alpha.docs.inference_profile_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::InferenceProfile(draft)) => {
                assert_eq!(draft.profile_id, alpha.docs.inference_profile_id);
                assert_eq!(draft.max_output_tokens, "1024");
                assert_eq!(draft.max_turns, "12");
            }
            other => panic!("expected alpha inference profile draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::RequestTimeline,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::RequestTimeline, None);
        driver.wait_for_target(
            "alpha request row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha_submission.request_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &bravo_submission.request_id
        )));
        let alpha_timeline_texts = driver.click_target(&audit::targets::operator_entity(
            &alpha_submission.request_id,
        ));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::RequestTimeline,
            Some(alpha_submission.request_id.as_str()),
        );
        assert!(alpha_timeline_texts
            .iter()
            .any(|text| text.contains(alpha_submission.prompt.as_str())));
        assert!(alpha_timeline_texts
            .iter()
            .any(|text| text.contains(alpha_submission.response.trim())));

        driver.click_target(&audit::targets::operator_deployment(&bravo.peer_id));
        driver.click_target(&audit::targets::operator_agent(&bravo.agent_did));
        driver.click_target(&audit::targets::operator_section(
            OperatorSection::Behaviors,
        ));
        assert_operator_context(driver, &bravo, OperatorSection::Behaviors, None);
        driver.wait_for_target(
            "bravo behavior row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo.docs.behavior_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&alpha.docs.behavior_id)));
        driver.click_target(&audit::targets::operator_entity(&bravo.docs.behavior_id));
        assert_operator_context(
            driver,
            &bravo,
            OperatorSection::Behaviors,
            Some(bravo.docs.behavior_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => {
                assert_eq!(draft.behavior_id, bravo.docs.behavior_id);
                assert_eq!(draft.agent_did, bravo.agent_did);
                assert_eq!(draft.backend_id, bravo.docs.backend_id);
                assert_eq!(draft.tool_selection_id, bravo.docs.tool_selection_id);
                assert_eq!(draft.inference_profile_id, bravo.docs.inference_profile_id);
            }
            other => panic!("expected bravo behavior draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(OperatorSection::Backends));
        assert_operator_context(driver, &bravo, OperatorSection::Backends, None);
        driver.wait_for_target(
            "bravo backend row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo.docs.backend_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&alpha.docs.backend_id)));
        driver.click_target(&audit::targets::operator_entity(&bravo.docs.backend_id));
        assert_operator_context(
            driver,
            &bravo,
            OperatorSection::Backends,
            Some(bravo.docs.backend_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Backend(draft)) => {
                assert_eq!(draft.backend_id, bravo.docs.backend_id);
                assert_eq!(draft.provider_kind, backend.provider_kind.as_str());
                assert_eq!(draft.endpoint, backend.endpoint);
                assert!(draft.models.contains(backend.model_name.as_str()));
            }
            other => panic!("expected bravo backend draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::InferenceProfiles,
        ));
        assert_operator_context(driver, &bravo, OperatorSection::InferenceProfiles, None);
        driver.wait_for_target(
            "bravo inference profile row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo.docs.inference_profile_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &alpha.docs.inference_profile_id
        )));
        driver.click_target(&audit::targets::operator_entity(
            &bravo.docs.inference_profile_id,
        ));
        assert_operator_context(
            driver,
            &bravo,
            OperatorSection::InferenceProfiles,
            Some(bravo.docs.inference_profile_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::InferenceProfile(draft)) => {
                assert_eq!(draft.profile_id, bravo.docs.inference_profile_id);
                assert_eq!(draft.max_output_tokens, "1024");
                assert_eq!(draft.max_turns, "12");
            }
            other => panic!("expected bravo inference profile draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::RequestTimeline,
        ));
        assert_operator_context(driver, &bravo, OperatorSection::RequestTimeline, None);
        driver.wait_for_target(
            "bravo request row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo_submission.request_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &alpha_submission.request_id
        )));
        let bravo_timeline_texts = driver.click_target(&audit::targets::operator_entity(
            &bravo_submission.request_id,
        ));
        assert_operator_context(
            driver,
            &bravo,
            OperatorSection::RequestTimeline,
            Some(bravo_submission.request_id.as_str()),
        );
        assert!(bravo_timeline_texts
            .iter()
            .any(|text| text.contains(bravo_submission.prompt.as_str())));
        assert!(bravo_timeline_texts
            .iter()
            .any(|text| text.contains(bravo_submission.response.trim())));

        driver.click_target(&audit::targets::operator_deployment(&alpha.peer_id));
        driver.click_target(&audit::targets::operator_section(
            OperatorSection::Behaviors,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::Behaviors, None);
        driver.wait_for_target(
            "alpha behavior row before config edit",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.behavior_id),
        )?;
        driver.click_target(&audit::targets::operator_entity(&alpha.docs.behavior_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::Behaviors,
            Some(alpha.docs.behavior_id.as_str()),
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("System Prompt"),
            alpha_tool_prompt,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Backend ID"),
            &alpha_switch_backend_id,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Model Name"),
            backend.model_name.as_str(),
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Inference Profile ID"),
            &alpha_switch_profile_id,
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => {
                assert_eq!(draft.behavior_id, alpha.docs.behavior_id);
                assert_eq!(draft.backend_id, alpha_switch_backend_id);
                assert_eq!(draft.inference_profile_id, alpha_switch_profile_id);
                assert_eq!(draft.system_prompt, alpha_tool_prompt);
            }
            other => panic!("expected edited alpha behavior draft, got {other:?}"),
        }
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "alpha behavior config edit persisted on desktop",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .behaviors
                        .iter()
                        .find(|row| row.behavior_id == alpha.docs.behavior_id)
                        .filter(|row| {
                            row.agent_did.as_deref() == Some(alpha.agent_did.as_str())
                                && row.backend_id.as_deref()
                                    == Some(alpha_switch_backend_id.as_str())
                                && row.inference_profile_id.as_deref()
                                    == Some(alpha_switch_profile_id.as_str())
                                && row.system_prompt.as_deref() == Some(alpha_tool_prompt)
                        })
                        .map(|row| row.behavior_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::ToolSelections,
        ));
        driver.wait_for_target(
            "alpha tool selection after config edit",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.tool_selection_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &bravo.docs.tool_selection_id
        )));
        driver.click_target(&audit::targets::operator_entity(
            &alpha.docs.tool_selection_id,
        ));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::ToolSelections,
            Some(alpha.docs.tool_selection_id.as_str()),
        );
        driver.click_target(&audit::targets::operator_toggle("Enable File Tools"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("File Tools Mode"),
            "ReadOnly",
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::ToolSelection(draft)) => {
                assert_eq!(draft.selection_id, alpha.docs.tool_selection_id);
                assert_eq!(draft.agent_did, alpha.agent_did);
                assert!(draft.enable_file_tools);
                assert_eq!(draft.file_tools_mode, "ReadOnly");
                assert!(!draft.enable_bash);
            }
            other => panic!("expected edited alpha tool selection draft, got {other:?}"),
        }
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "alpha tool selection edit persisted on desktop",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .tool_selections
                        .iter()
                        .find(|row| row.selection_id == alpha.docs.tool_selection_id)
                        .filter(|row| {
                            row.agent_did.as_deref() == Some(alpha.agent_did.as_str())
                                && row.enable_file_tools == Some(true)
                                && row.file_tools_mode.as_deref() == Some("ReadOnly")
                        })
                        .map(|row| row.selection_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(OperatorSection::Backends));
        driver.wait_for_target(
            "alpha switched backend row after behavior binding edit",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha_switch_backend_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&alpha.docs.backend_id)));
        assert!(!driver.has_target(&audit::targets::operator_entity(&bravo.docs.backend_id)));
        driver.click_target(&audit::targets::operator_entity(&alpha_switch_backend_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::Backends,
            Some(alpha_switch_backend_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Backend(draft)) => {
                assert_eq!(draft.backend_id, alpha_switch_backend_id);
                assert_eq!(draft.endpoint, backend.endpoint);
                assert!(draft.models.contains(backend.model_name.as_str()));
                assert_eq!(draft.probe_status, "healthy");
            }
            other => panic!("expected alpha switched backend draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::InferenceProfiles,
        ));
        driver.wait_for_target(
            "alpha switched inference profile row after behavior binding edit",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha_switch_profile_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &alpha.docs.inference_profile_id
        )));
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &bravo.docs.inference_profile_id
        )));
        driver.click_target(&audit::targets::operator_entity(&alpha_switch_profile_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::InferenceProfiles,
            Some(alpha_switch_profile_id.as_str()),
        );
        driver.replace_text_in_target(&audit::targets::operator_field("Max Output Tokens"), "1536");
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::InferenceProfile(draft)) => {
                assert_eq!(draft.profile_id, alpha_switch_profile_id);
                assert_eq!(draft.max_output_tokens, "1536");
                assert_eq!(draft.max_turns, "16");
                assert_eq!(draft.temperature, "0");
            }
            other => panic!("expected edited alpha switched profile draft, got {other:?}"),
        }
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "alpha inference profile edit persisted on desktop",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_profiles
                        .iter()
                        .find(|row| row.profile_id == alpha_switch_profile_id)
                        .filter(|row| row.max_output_tokens == Some(1536))
                        .map(|row| row.profile_id.clone())
                })
            },
        )?;
    }

    wait_for_value(
        "alpha behavior/tool config and generation after UI edits",
        Duration::from_secs(120),
        || {
            fixture
                .runtime
                .block_on(desktop_client.refresh_store())
                .ok()?;
            let snapshot = desktop_client.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == alpha.docs.behavior_id)
                .is_some_and(|row| {
                    row.backend_id.as_deref() == Some(alpha_switch_backend_id.as_str())
                        && row.inference_profile_id.as_deref()
                            == Some(alpha_switch_profile_id.as_str())
                        && row.system_prompt.as_deref() == Some(alpha_tool_prompt)
                });
            let tools_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == alpha.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            let profile_ready = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == alpha_switch_profile_id)
                .is_some_and(|row| row.max_output_tokens == Some(1536));
            let runtime_ready = snapshot
                .latest_runtime(&alpha.agent_did)
                .is_some_and(|row| {
                    row.router_generation
                        .or(row.active_generation)
                        .is_some_and(|generation| generation > alpha_initial_generation)
                        && row.runnable_behavior_count == Some(1)
                        && row.unavailable_behavior_count == Some(0)
                        && row
                            .last_reconcile_error
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                });
            (behavior_ready && tools_ready && profile_ready && runtime_ready).then_some(())
        },
    )
    .with_context(|| {
        format!(
            "desktop state: {}\nremote state: {}",
            describe_live_config_state(
                fixture.runtime.as_ref(),
                desktop_client.as_ref(),
                "desktop",
                &alpha.agent_did,
                &alpha.docs,
                &alpha_switch_backend_id,
                &alpha_switch_profile_id,
            ),
            describe_live_config_state(
                fixture.runtime.as_ref(),
                alpha.remote_core,
                "alpha remote",
                &alpha.agent_did,
                &alpha.docs,
                &alpha_switch_backend_id,
                &alpha_switch_profile_id,
            )
        )
    })?;
    wait_for_value(
        "alpha behavior/tool config replicated to remote runtime",
        Duration::from_secs(120),
        || {
            fixture
                .runtime
                .block_on(alpha.remote_core.refresh_store())
                .ok()?;
            let snapshot = alpha.remote_core.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == alpha.docs.behavior_id)
                .is_some_and(|row| {
                    row.backend_id.as_deref() == Some(alpha_switch_backend_id.as_str())
                        && row.inference_profile_id.as_deref()
                            == Some(alpha_switch_profile_id.as_str())
                        && row.system_prompt.as_deref() == Some(alpha_tool_prompt)
                });
            let backend_ready = snapshot
                .inference_backends
                .iter()
                .find(|row| row.backend_id == alpha_switch_backend_id)
                .is_some_and(|row| {
                    row.endpoint.as_deref() == Some(backend.endpoint.as_str())
                        && row.models.iter().any(|model| model == &backend.model_name)
                });
            let tools_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == alpha.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            let profile_ready = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == alpha_switch_profile_id)
                .is_some_and(|row| row.max_output_tokens == Some(1536));
            let runtime_ready = snapshot
                .latest_runtime(&alpha.agent_did)
                .is_some_and(|row| {
                    row.router_generation
                        .or(row.active_generation)
                        .is_some_and(|generation| generation > alpha_remote_initial_generation)
                        && row.runnable_behavior_count == Some(1)
                        && row.unavailable_behavior_count == Some(0)
                        && row
                            .last_reconcile_error
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                });
            (behavior_ready && backend_ready && tools_ready && profile_ready && runtime_ready)
                .then_some(())
        },
    )
    .with_context(|| {
        format!(
            "desktop state: {}\nremote state: {}",
            describe_live_config_state(
                fixture.runtime.as_ref(),
                desktop_client.as_ref(),
                "desktop",
                &alpha.agent_did,
                &alpha.docs,
                &alpha_switch_backend_id,
                &alpha_switch_profile_id,
            ),
            describe_live_config_state(
                fixture.runtime.as_ref(),
                alpha.remote_core,
                "alpha remote",
                &alpha.agent_did,
                &alpha.docs,
                &alpha_switch_backend_id,
                &alpha_switch_profile_id,
            )
        )
    })?;
    wait_for_stable_runtime_ready(
        fixture.runtime.as_ref(),
        alpha.remote_core,
        "alpha after remote config replication",
        &alpha.agent_did,
        Duration::from_secs(10),
        Duration::from_secs(90),
    )?;
    assert_live_deployment_default_config(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop bravo",
        &bravo,
        backend.model_name.as_str(),
    )?;
    assert_live_deployment_default_config(
        fixture.runtime.as_ref(),
        bravo.remote_core,
        "remote bravo",
        &bravo,
        backend.model_name.as_str(),
    )?;

    let post_config_submission;
    {
        let driver = &mut fixture.driver;
        post_config_submission =
            submit_live_prompt_for_deployment(driver, &alpha, "ALPHA_CONFIG_READY")?;
        wait_for_value(
            "post-config request used switched backend",
            Duration::from_secs(30),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .requests
                        .iter()
                        .find(|row| row.request_id == post_config_submission.request_id)
                        .filter(|row| {
                            row.agent_did.as_deref() == Some(alpha.agent_did.as_str())
                                && row.behavior_id.as_deref()
                                    == Some(alpha.docs.behavior_id.as_str())
                                && row.backend_id.as_deref()
                                    == Some(alpha_switch_backend_id.as_str())
                        })
                        .map(|row| row.request_id.clone())
                })
            },
        )?;
    }
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop alpha post-config",
        &alpha,
        &post_config_submission,
        Some(alpha_switch_backend_id.as_str()),
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        alpha.remote_core,
        "remote alpha post-config",
        &alpha,
        &post_config_submission,
        Some(alpha_switch_backend_id.as_str()),
    )?;

    {
        let driver = &mut fixture.driver;
        driver.open_activity(Activity::Chat);
        driver.click_target(&audit::targets::chat_deployment(&bravo.peer_id));
        driver.click_target(&audit::targets::chat_agent(&bravo.agent_did));
        assert_chat_context(driver, &bravo, None);
        let bravo_texts = driver.click_target(&audit::targets::chat_conversation(
            &bravo_submission.session_id,
        ));
        assert_chat_context(driver, &bravo, Some(bravo_submission.session_id.as_str()));
        assert!(bravo_texts
            .iter()
            .any(|text| text.contains(bravo_submission.prompt.as_str())));
        assert!(
            !bravo_texts
                .iter()
                .any(|text| text.contains(post_config_submission.prompt.as_str())),
            "bravo transcript leaked alpha post-config prompt after switching deployments"
        );

        driver.click_target(&audit::targets::chat_deployment(&alpha.peer_id));
        driver.click_target(&audit::targets::chat_agent(&alpha.agent_did));
        assert_chat_context(driver, &alpha, None);
        let alpha_post_config_texts = driver.click_target(&audit::targets::chat_conversation(
            &post_config_submission.session_id,
        ));
        assert_chat_context(
            driver,
            &alpha,
            Some(post_config_submission.session_id.as_str()),
        );
        assert!(alpha_post_config_texts
            .iter()
            .any(|text| text.contains(post_config_submission.prompt.as_str())));
        assert!(alpha_post_config_texts
            .iter()
            .any(|text| text.contains(post_config_submission.response.trim())));
    }

    fixture.shutdown()
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_operator_config_round_trips() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-operator", global_log_store())?;
    let docs = fixture.docs.clone();
    let backend = fixture.backend.clone();
    let agent_did = fixture
        .running_agent
        .as_ref()
        .ok_or_else(|| anyhow!("live fixture missing running agent"))?
        .did
        .clone();
    let shadow_backend_id = format!("{}:binding-backend", docs.behavior_id);
    let shadow_model_name = format!("{}:binding-model", backend.model_name);
    let shadow_tool_selection_id = format!("{}:binding-tools", docs.behavior_id);
    let shadow_inference_profile_id = format!("{}:binding-profile", docs.behavior_id);

    {
        let client = Arc::clone(
            fixture
                .driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        fixture.runtime.block_on(async {
            client
                .save_backend(&InferenceBackendRow {
                    backend_id: shadow_backend_id.clone(),
                    name: Some("Live Binding Backend".to_string()),
                    provider_kind: Some(backend.provider_kind.as_str().to_string()),
                    endpoint: Some(backend.endpoint.clone()),
                    api_key: backend.api_key.clone(),
                    api_key_env_var: backend.api_key_env_var.clone(),
                    max_concurrent: Some(1),
                    max_queue_depth: Some(100),
                    enabled: Some(true),
                    models: vec![shadow_model_name.clone()],
                    last_probe: None,
                    probe_status: Some("healthy".to_string()),
                })
                .await?;
            client
                .save_tool_selection(&ToolSelectionRow {
                    selection_id: shadow_tool_selection_id.clone(),
                    agent_did: Some(agent_did.clone()),
                    display_name: Some("Live Binding Tools".to_string()),
                    enable_file_tools: Some(false),
                    file_tools_mode: Some("readonly".to_string()),
                    enable_bash: Some(false),
                    bash_mode: Some("disabled".to_string()),
                    cli_tool_names: vec![],
                    enable_meta_tools: Some(false),
                    delegate_to: vec![],
                })
                .await?;
            client
                .save_inference_profile(&InferenceProfileRow {
                    profile_id: shadow_inference_profile_id.clone(),
                    display_name: Some("Live Binding Profile".to_string()),
                    context_window: Some(32768),
                    max_output_tokens: Some(512),
                    max_turns: Some(8),
                    temperature: Some(0.0),
                    stream_batch_ms: Some(40),
                    deadline_duration_secs: Some(120),
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;
    }

    {
        let driver = &mut fixture.driver;
        let _session_id = ensure_chat_session_selected(
            driver,
            "live operator fixture chat ready",
            Duration::from_secs(10),
        )?;
        let (request_id, response_text) = submit_chat_message_and_wait_for_observed_response(
            driver,
            "Reply with exactly CONFIG_READY for the operator audit",
        )?;
        assert!(!response_text.trim().is_empty());

        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Runtime,
        ));
        let runtime_texts = driver.render();
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains("Runtime Inspector")));
        assert!(runtime_texts.iter().any(|text| text.contains(&agent_did)));
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains(&docs.behavior_id)));

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::RequestTimeline,
        ));
        driver.wait_for_target(
            "live operator request timeline row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&request_id),
        )?;
        let timeline_texts = driver.click_target(&audit::targets::operator_entity(&request_id));
        assert!(timeline_texts
            .iter()
            .any(|text| text.contains("CONFIG_READY")));
        assert!(timeline_texts
            .iter()
            .any(|text| text.contains(response_text.trim())));

        assert_operator_filter_round_trip(
            driver,
            OperatorSection::Behaviors,
            "Live Audit Default",
            &docs.behavior_id,
            "definitely-missing-live-behavior",
        )?;
        driver.click_target(&audit::targets::operator_entity(&docs.behavior_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Discarded Live Behavior Draft",
        );
        driver.click_target(audit::targets::OPERATOR_DISCARD);
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => {
                assert_eq!(draft.display_name, "Live Audit Default");
            }
            other => panic!("expected behavior draft after discard, got {other:?}"),
        }
        assert!(driver
            .app
            .client
            .as_ref()
            .and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .behaviors
                    .iter()
                    .find(|row| row.behavior_id == docs.behavior_id)
                    .and_then(|row| row.display_name.clone())
            })
            .is_some_and(|display_name| display_name == "Live Audit Default"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Live Audit Behavior Reviewed",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("System Prompt"),
            "You are a live audited desktop operator. Return concise answers.",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Backend ID"),
            &shadow_backend_id,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Model Name"),
            &shadow_model_name,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Tool Selection ID"),
            &shadow_tool_selection_id,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Inference Profile ID"),
            &shadow_inference_profile_id,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Compaction Strategy"),
            "StripThenSummarize",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Compaction Threshold"),
            "0.88",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live behavior edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .behaviors
                    .iter()
                    .find(|row| row.behavior_id == docs.behavior_id)
                    .filter(|row| {
                        row.display_name.as_deref()
                            == Some("Live Audit Behavior Reviewed")
                            && row.system_prompt.as_deref()
                                == Some(
                                    "You are a live audited desktop operator. Return concise answers.",
                                )
                            && row.backend_id.as_deref() == Some(shadow_backend_id.as_str())
                            && row.model_name.as_deref() == Some(shadow_model_name.as_str())
                            && row.tool_selection_id.as_deref()
                                == Some(shadow_tool_selection_id.as_str())
                            && row.inference_profile_id.as_deref()
                                == Some(shadow_inference_profile_id.as_str())
                            && row.compaction_strategy.as_deref() == Some("StripThenSummarize")
                            && row.compaction_threshold == Some(0.88)
                    })
                    .map(|row| row.behavior_id.clone())
            })
            },
        )?;

        assert_operator_filter_round_trip(
            driver,
            OperatorSection::Backends,
            &shadow_backend_id,
            &shadow_backend_id,
            "definitely-missing-live-backend",
        )?;
        driver.click_target(&audit::targets::operator_entity(&shadow_backend_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Name"),
            "Live Backend Reviewed",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Provider Kind"),
            backend.provider_kind.as_str(),
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Endpoint"),
            backend.endpoint.as_str(),
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("API Key"),
            "desktop-audit-placeholder-key",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("API Key Env Var"),
            "DEFRA_AGENT_DESKTOP_AUDIT_API_KEY",
        );
        driver.replace_text_in_target(&audit::targets::operator_field("Max Concurrent"), "2");
        driver.replace_text_in_target(&audit::targets::operator_field("Max Queue Depth"), "200");
        driver.replace_text_in_target(&audit::targets::operator_field("Probe Status"), "reviewed");
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Models"),
            &format!("{shadow_model_name}, audit-shadow-model"),
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live backend edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_backends
                        .iter()
                        .find(|row| row.backend_id == shadow_backend_id)
                        .filter(|row| {
                            row.name.as_deref() == Some("Live Backend Reviewed")
                                && row.provider_kind.as_deref()
                                    == Some(backend.provider_kind.as_str())
                                && row.endpoint.as_deref() == Some(backend.endpoint.as_str())
                                && row.api_key.as_deref() == Some("desktop-audit-placeholder-key")
                                && row.api_key_env_var.as_deref()
                                    == Some("DEFRA_AGENT_DESKTOP_AUDIT_API_KEY")
                                && row.max_concurrent == Some(2)
                                && row.max_queue_depth == Some(200)
                                && row.probe_status.as_deref() == Some("reviewed")
                                && row.enabled == Some(false)
                                && row.models.iter().any(|model| model == &shadow_model_name)
                                && row.models.iter().any(|model| model == "audit-shadow-model")
                        })
                        .map(|row| row.backend_id.clone())
                })
            },
        )?;

        assert_operator_filter_round_trip(
            driver,
            OperatorSection::ToolSelections,
            "Live Audit Tools",
            &docs.tool_selection_id,
            "definitely-missing-live-tools",
        )?;
        driver.click_target(&audit::targets::operator_entity(&docs.tool_selection_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Live Tooling Reviewed",
        );
        driver.click_target(&audit::targets::operator_toggle("Enable File Tools"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("File Tools Mode"),
            "readonly",
        );
        driver.click_target(&audit::targets::operator_toggle("Enable Bash"));
        driver.replace_text_in_target(&audit::targets::operator_field("Bash Mode"), "workspace");
        driver.replace_text_in_target(
            &audit::targets::operator_field("CLI Tool Names"),
            "rg\ncargo",
        );
        driver.click_target(&audit::targets::operator_toggle("Enable Meta Tools"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Delegate To"),
            "planner\nreviewer",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live tool selection edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .tool_selections
                        .iter()
                        .find(|row| row.selection_id == docs.tool_selection_id)
                        .filter(|row| {
                            row.display_name.as_deref() == Some("Live Tooling Reviewed")
                                && row.enable_file_tools == Some(true)
                                && row.file_tools_mode.as_deref() == Some("readonly")
                                && row.enable_bash == Some(true)
                                && row.bash_mode.as_deref() == Some("workspace")
                                && row.cli_tool_names == vec!["rg".to_string(), "cargo".to_string()]
                                && row.enable_meta_tools == Some(true)
                                && row.delegate_to
                                    == vec!["planner".to_string(), "reviewer".to_string()]
                        })
                        .map(|row| row.selection_id.clone())
                })
            },
        )?;

        assert_operator_filter_round_trip(
            driver,
            OperatorSection::InferenceProfiles,
            "Live Binding Profile",
            &shadow_inference_profile_id,
            "definitely-missing-live-profile",
        )?;
        driver.click_target(&audit::targets::operator_entity(
            &shadow_inference_profile_id,
        ));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Live Profile Reviewed",
        );
        driver.replace_text_in_target(&audit::targets::operator_field("Context Window"), "65536");
        driver.replace_text_in_target(&audit::targets::operator_field("Max Output Tokens"), "2048");
        driver.replace_text_in_target(&audit::targets::operator_field("Max Turns"), "14");
        driver.replace_text_in_target(&audit::targets::operator_field("Temperature"), "0.1");
        driver.replace_text_in_target(&audit::targets::operator_field("Stream Batch Ms"), "80");
        driver.replace_text_in_target(
            &audit::targets::operator_field("Deadline Duration Secs"),
            "180",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live inference profile edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_profiles
                        .iter()
                        .find(|row| row.profile_id == shadow_inference_profile_id)
                        .filter(|row| {
                            row.display_name.as_deref() == Some("Live Profile Reviewed")
                                && row.context_window == Some(65536)
                                && row.max_output_tokens == Some(2048)
                                && row.max_turns == Some(14)
                                && row.temperature == Some(0.1)
                                && row.stream_batch_ms == Some(80)
                                && row.deadline_duration_secs == Some(180)
                        })
                        .map(|row| row.profile_id.clone())
                })
            },
        )?;
    }

    fixture.shutdown()
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_operator_identity_field_round_trips() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-identities", global_log_store())?;
    let docs = fixture.docs.clone();
    let backend = fixture.backend.clone();
    let agent_did = fixture
        .running_agent
        .as_ref()
        .ok_or_else(|| anyhow!("live fixture missing running agent"))?
        .did
        .clone();
    let shadow_agent_did = format!("{agent_did}:identity-shadow");
    let behavior_id = format!("{}:identity-behavior", docs.behavior_id);
    let renamed_behavior_id = format!("{}:renamed", behavior_id);
    let tool_selection_id = format!("{}:identity-tools", docs.behavior_id);
    let renamed_tool_selection_id = format!("{}:renamed", tool_selection_id);
    let profile_id = format!("{}:identity-profile", docs.behavior_id);
    let renamed_profile_id = format!("{}:renamed", profile_id);
    let task_id = format!("{}:identity-task", docs.behavior_id);
    let renamed_task_id = format!("{}:renamed", task_id);

    {
        let client = Arc::clone(
            fixture
                .driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        fixture.runtime.block_on(async {
            insert_agent_principal(
                client.as_ref(),
                &shadow_agent_did,
                "Identity Shadow",
                &renamed_behavior_id,
            )
            .await?;
            client
                .save_tool_selection(&ToolSelectionRow {
                    selection_id: tool_selection_id.clone(),
                    agent_did: Some(agent_did.clone()),
                    display_name: Some("Live Identity Tools".to_string()),
                    enable_file_tools: Some(false),
                    file_tools_mode: Some("readonly".to_string()),
                    enable_bash: Some(false),
                    bash_mode: Some("disabled".to_string()),
                    cli_tool_names: vec!["rg".to_string()],
                    enable_meta_tools: Some(false),
                    delegate_to: vec![],
                })
                .await?;
            client
                .save_inference_profile(&InferenceProfileRow {
                    profile_id: profile_id.clone(),
                    display_name: Some("Live Identity Profile".to_string()),
                    context_window: Some(32768),
                    max_output_tokens: Some(512),
                    max_turns: Some(8),
                    temperature: Some(0.0),
                    stream_batch_ms: Some(40),
                    deadline_duration_secs: Some(120),
                })
                .await?;
            client
                .save_behavior(&AgentBehaviorRow {
                    behavior_id: behavior_id.clone(),
                    agent_did: Some(agent_did.clone()),
                    display_name: Some("Live Identity Behavior".to_string()),
                    system_prompt: Some("Identity field audit behavior.".to_string()),
                    backend_id: Some(docs.backend_id.clone()),
                    model_name: Some(backend.model_name.clone()),
                    tool_selection_id: Some(tool_selection_id.clone()),
                    inference_profile_id: Some(profile_id.clone()),
                    compaction_strategy: Some("none".to_string()),
                    compaction_threshold: Some(0.95),
                    enabled: Some(false),
                    created_at: Some(chrono::Utc::now().to_rfc3339()),
                })
                .await?;
            client
                .save_scheduled_task(&ScheduledTaskRow {
                    task_id: task_id.clone(),
                    agent_did: Some(agent_did.clone()),
                    behavior_id: Some(behavior_id.clone()),
                    name: Some("Live Identity Task".to_string()),
                    prompt: Some("Audit identity fields.".to_string()),
                    interval_secs: Some(3600),
                    enabled: Some(false),
                    next_run_at: Some("2035-01-01T00:00:00Z".to_string()),
                    last_run_at: None,
                    last_status: None,
                    last_error: None,
                    run_count: Some(0),
                    created_at: Some(chrono::Utc::now().to_rfc3339()),
                    updated_at: None,
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;
    }

    {
        let driver = &mut fixture.driver;
        driver.app.state.operator.selected_agent_did = Some(agent_did.clone());
        driver.app.state.operator.selected_peer_id = None;
        driver.app.state.operator.selected_entity_id = None;
        driver.app.state.operator.draft = None;
        driver.app.state.operator.draft_source_entity_id = None;
        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Behaviors,
        ));
        driver.wait_for_target(
            "live identity behavior row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&behavior_id),
        )?;
        driver.click_target(&audit::targets::operator_entity(&behavior_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Behavior ID"),
            &renamed_behavior_id,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Agent DID"),
            &shadow_agent_did,
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live identity behavior id persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .behaviors
                        .iter()
                        .find(|row| row.behavior_id == renamed_behavior_id)
                        .filter(|row| row.agent_did.as_deref() == Some(shadow_agent_did.as_str()))
                        .map(|row| row.behavior_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::ToolSelections,
        ));
        driver.wait_for_target(
            "live identity tool selection row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&tool_selection_id),
        )?;
        driver.click_target(&audit::targets::operator_entity(&tool_selection_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Selection ID"),
            &renamed_tool_selection_id,
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live identity tool selection id persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .tool_selections
                        .iter()
                        .find(|row| row.selection_id == renamed_tool_selection_id)
                        .filter(|row| row.agent_did.as_deref() == Some(agent_did.as_str()))
                        .map(|row| row.selection_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::InferenceProfiles,
        ));
        driver.wait_for_target(
            "live identity profile row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&profile_id),
        )?;
        driver.click_target(&audit::targets::operator_entity(&profile_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Profile ID"),
            &renamed_profile_id,
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live identity profile id persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_profiles
                        .iter()
                        .find(|row| row.profile_id == renamed_profile_id)
                        .map(|row| row.profile_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::ScheduledTasks,
        ));
        driver.wait_for_target(
            "live identity scheduled task row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&task_id),
        )?;
        driver.click_target(&audit::targets::operator_entity(&task_id));
        driver.replace_text_in_target(&audit::targets::operator_field("Task ID"), &renamed_task_id);
        driver.replace_text_in_target(
            &audit::targets::operator_field("Behavior ID"),
            &renamed_behavior_id,
        );
        driver.scroll_right_rail_until_target(
            "live identity scheduled task apply",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_interactable_target(audit::targets::OPERATOR_APPLY)?;
        wait_for_value(
            "live identity scheduled task id persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == renamed_task_id)
                        .filter(|row| {
                            row.behavior_id.as_deref() == Some(renamed_behavior_id.as_str())
                        })
                        .map(|row| row.task_id.clone())
                })
            },
        )?;
    }

    fixture.shutdown()
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_chat_disclosure_artifacts() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-disclosures", global_log_store())?;
    let agent_did = fixture
        .running_agent
        .as_ref()
        .ok_or_else(|| anyhow!("live fixture missing running agent"))?
        .did
        .clone();
    let behavior_id = fixture.docs.behavior_id.clone();
    let response_key = format!("live-response-disclosure-{}", uuid::Uuid::new_v4().simple());
    let conversation = {
        let client = Arc::clone(
            fixture
                .driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        fixture
            .runtime
            .block_on(client.create_conversation(&agent_did, Some(&behavior_id)))?
    };

    {
        let client = Arc::clone(
            fixture
                .driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        fixture.runtime.block_on(insert_chat_transcript_documents(
            client.as_ref(),
            &conversation.session_id,
            &agent_did,
            &behavior_id,
            &response_key,
        ))?;
    }

    {
        let driver = &mut fixture.driver;
        driver.app.state.activity = Activity::Chat;
        driver.wait_for_target(
            "live disclosure conversation row",
            Duration::from_secs(10),
            &audit::targets::chat_conversation(&conversation.session_id),
        )?;
        driver.click_target(&audit::targets::chat_conversation(&conversation.session_id));
        assert_eq!(
            driver.app.state.chat.selected_session_id.as_deref(),
            Some(conversation.session_id.as_str())
        );

        let initial = driver.wait_for_target(
            "live reasoning disclosure row",
            Duration::from_secs(10),
            &audit::targets::chat_reasoning(&response_key),
        )?;
        assert!(initial
            .iter()
            .any(|text| text.contains("REASONING DISCLOSURE")));
        assert!(!initial
            .iter()
            .any(|text| text.contains("I verified the latest request")));

        driver.click_interactable_target(&audit::targets::chat_tool_card("call-shell-1"))?;
        let tool_texts = driver.render();
        assert!(driver
            .app
            .state
            .chat
            .expanded_tool_cards
            .contains("call-shell-1"));
        assert!(tool_texts.iter().any(|text| text.contains("Args")));
        assert!(!tool_texts
            .iter()
            .any(|text| text.contains("src/app.rs: audit target live")));
        driver.click_interactable_target(&audit::targets::chat_tool_output("call-shell-1"))?;
        let output_texts = driver.render();
        assert!(output_texts
            .iter()
            .any(|text| text.contains("src/app.rs: audit target live")));

        driver.click_interactable_target(&audit::targets::chat_reasoning(&response_key))?;
        let reasoning_texts = driver.render();
        assert!(driver
            .app
            .state
            .chat
            .expanded_reasoning_cards
            .contains(&format!("reasoning:{response_key}")));
        assert!(reasoning_texts
            .iter()
            .any(|text| text.contains("I verified the latest request")));
    }

    fixture.shutdown()
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_chat_retry_and_export() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-retry-export", global_log_store())?;

    {
        let driver = &mut fixture.driver;
        let session_id = ensure_chat_session_selected(
            driver,
            "live retry/export conversation selected",
            Duration::from_secs(10),
        )?;
        let prompt = format!(
            "Reply with exactly RETRY_EXPORT_READY and nothing else. audit {}",
            uuid::Uuid::new_v4()
        );
        let (first_request_id, first_response) =
            submit_chat_message_and_wait_for_observed_response(driver, &prompt)?;

        driver.click_interactable_target(audit::targets::CHAT_EXPORT)?;
        let export_payload = driver
            .app
            .state
            .chat
            .last_export_payload
            .as_deref()
            .ok_or_else(|| anyhow!("live chat export did not capture a payload"))?;
        assert!(export_payload.contains(&session_id));
        assert!(export_payload.contains(&first_request_id));
        assert!(export_payload.contains(&prompt));
        assert!(export_payload.contains(first_response.trim()));

        let prior_request_count = driver
            .app
            .client
            .as_ref()
            .map(|client| client.store().snapshot().requests.len())
            .ok_or_else(|| anyhow!("desktop client missing"))?;
        driver.click_interactable_target(audit::targets::CHAT_RETRY)?;
        let retry_request_id =
            wait_for_value("live retry request row", Duration::from_secs(10), || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .requests
                        .iter()
                        .filter(|row| row.session_id.as_deref() == Some(session_id.as_str()))
                        .find(|row| {
                            row.retry_parent_request.as_deref() == Some(first_request_id.as_str())
                                && row.retry_root_request.as_deref()
                                    == Some(first_request_id.as_str())
                                && row.retry_count == Some(1)
                                && row.content.as_deref() == Some(prompt.as_str())
                        })
                        .map(|row| row.request_id.clone())
                        .filter(|_| client.store().snapshot().requests.len() > prior_request_count)
                })
            })?;
        let retry_response =
            wait_for_value("live retry response row", Duration::from_secs(90), || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .latest_response_for_request(&retry_request_id)
                        .and_then(|row| row.content.clone())
                        .filter(|content| !content.trim().is_empty())
                })
            })?;
        wait_for_value("live retry transcript row", Duration::from_secs(30), || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains(retry_response.trim()))
                .then_some(())
        })?;
        assert_eq!(driver.app.state.chat.last_submission_error, None);
        assert_eq!(
            driver.app.state.chat.last_action_message.as_deref(),
            Some("Retried latest request.")
        );
    }

    fixture.shutdown()
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_operator_scheduled_task_and_failures() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-scheduled", global_log_store())?;
    let docs = fixture.docs.clone();

    {
        let driver = &mut fixture.driver;
        driver.open_activity(Activity::Operator);
        assert_operator_filter_round_trip(
            driver,
            OperatorSection::ScheduledTasks,
            "Live Audit Scheduled Task",
            &docs.scheduled_task_id,
            "definitely-missing-live-task",
        )?;
        driver.click_target(&audit::targets::operator_entity(&docs.scheduled_task_id));

        driver.scroll_right_rail_until_target(
            "live scheduled task interval field",
            &audit::targets::operator_field("Interval Secs"),
        )?;
        driver.replace_text_in_target(&audit::targets::operator_field("Interval Secs"), "0");
        driver.scroll_right_rail_until_target(
            "live scheduled task apply validation",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_target(audit::targets::OPERATOR_APPLY);
        assert!(driver
            .app
            .state
            .operator
            .last_apply_error
            .as_deref()
            .is_some_and(|error| error.contains("interval_secs must be greater than zero")));
        let validation_texts = wait_for_value(
            "live scheduled validation error rendered",
            Duration::from_secs(2),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("interval_secs must be greater than zero"))
                    .then_some(texts)
            },
        )?;
        assert!(validation_texts
            .iter()
            .any(|text| text.contains("interval_secs must be greater than zero")));

        driver.replace_text_in_target(&audit::targets::operator_field("Interval Secs"), "120");
        driver.replace_text_in_target(
            &audit::targets::operator_field("Name"),
            "Live Scheduled Review",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Prompt"),
            "Run a live scheduled desktop audit.",
        );
        driver.scroll_right_rail_until_target(
            "live scheduled task enabled toggle",
            &audit::targets::operator_toggle("Enabled"),
        )?;
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        driver.scroll_right_rail_until_target(
            "live scheduled task next-run field",
            &audit::targets::operator_field("Next Run At"),
        )?;
        driver.replace_text_in_target(
            &audit::targets::operator_field("Next Run At"),
            "2035-04-15T12:34:56Z",
        );
        driver.scroll_right_rail_until_target(
            "live scheduled task apply",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live scheduled task edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == docs.scheduled_task_id)
                        .filter(|row| {
                            row.interval_secs == Some(120)
                                && row.name.as_deref() == Some("Live Scheduled Review")
                                && row.prompt.as_deref()
                                    == Some("Run a live scheduled desktop audit.")
                                && row.enabled == Some(false)
                                && row.next_run_at.as_deref() == Some("2035-04-15T12:34:56Z")
                        })
                        .map(|row| row.task_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_entity(&docs.scheduled_task_id));
        driver.scroll_right_rail_until_target(
            "live scheduled task re-enable toggle",
            &audit::targets::operator_toggle("Enabled"),
        )?;
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        driver.scroll_right_rail_until_target(
            "live scheduled task re-enable apply",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live scheduled task re-enabled",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == docs.scheduled_task_id)
                        .filter(|row| row.enabled == Some(true))
                        .map(|row| row.task_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_entity(&docs.scheduled_task_id));
        let prior_next_run = driver
            .app
            .client
            .as_ref()
            .and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .scheduled_tasks
                    .iter()
                    .find(|row| row.task_id == docs.scheduled_task_id)
                    .and_then(|row| row.next_run_at.clone())
            })
            .ok_or_else(|| anyhow!("missing live scheduled task next_run_at"))?;
        driver.scroll_right_rail_until_target(
            "live scheduled task run-now button",
            audit::targets::OPERATOR_RUN_NOW,
        )?;
        driver.click_target(audit::targets::OPERATOR_RUN_NOW);
        wait_for_value(
            "live scheduled task run-now persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == docs.scheduled_task_id)
                        .and_then(|row| row.next_run_at.clone())
                        .filter(|next_run_at| next_run_at != &prior_next_run)
                })
            },
        )?;

        let failed_task = driver
            .app
            .client
            .as_ref()
            .and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .scheduled_tasks
                    .iter()
                    .find(|row| row.task_id == docs.scheduled_task_id)
                    .cloned()
            })
            .ok_or_else(|| anyhow!("missing live scheduled task before failure insert"))?;
        let mut failed_task = failed_task;
        failed_task.last_status = Some("error".to_string());
        failed_task.last_error = Some("live scheduled audit failure".to_string());
        failed_task.last_run_at = Some(chrono::Utc::now().to_rfc3339());
        let failed_task_agent_did = failed_task.agent_did.clone();
        let client = Arc::clone(
            driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        driver
            .app
            .runtime
            .block_on(client.save_scheduled_task(&failed_task))?;
        wait_for_value(
            "live scheduled failure persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == docs.scheduled_task_id)
                        .filter(|row| {
                            row.last_status.as_deref() == Some("error")
                                && row.last_error.as_deref() == Some("live scheduled audit failure")
                        })
                        .map(|row| row.task_id.clone())
                })
            },
        )?;

        driver.app.state.operator.selected_agent_did = failed_task_agent_did;
        driver.app.state.operator.selected_entity_id = None;
        driver.app.state.operator.draft = None;
        driver.app.state.operator.draft_source_entity_id = None;
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::RecentFailures,
        ));
        let failure_id = format!("task:{}", docs.scheduled_task_id);
        driver.wait_for_target(
            "live scheduled task failure row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&failure_id),
        )?;
        let failure_texts = driver.click_target(&audit::targets::operator_entity(&failure_id));
        assert!(failure_texts
            .iter()
            .any(|text| text.contains("Failure Detail")));
        assert!(failure_texts
            .iter()
            .any(|text| text.contains("live scheduled audit failure")));
    }

    fixture.shutdown()
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_logs_event_classification() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-logs", global_log_store())?;
    let runtime = Arc::clone(&fixture.runtime);
    let agent_did = fixture
        .running_agent
        .as_ref()
        .ok_or_else(|| anyhow!("live fixture missing running agent"))?
        .did
        .clone();
    let peer = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(fixture._tempdir.path().join("peer")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer_addr = peer
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("live logs peer missing listen address"))?;
    let baseline_events = global_log_store().snapshot().total_events;

    {
        let driver = &mut fixture.driver;
        let _session_id =
            ensure_chat_session_selected(driver, "live logs chat ready", Duration::from_secs(10))?;
        let (request_id, response_text) = submit_chat_message_and_wait_for_observed_response(
            driver,
            "Reply with exactly LOG_READY for the logs audit",
        )?;
        assert!(!response_text.trim().is_empty());

        driver.open_activity(Activity::Peers);
        driver.wait_for_target(
            "live logs peer add form",
            Duration::from_secs(10),
            audit::targets::PEERS_ADD_LABEL,
        )?;
        driver.click_target(audit::targets::PEERS_ADD_LABEL);
        driver.type_text("Live Logs Peer");
        driver.click_target(audit::targets::PEERS_ADD_ADDR);
        driver.type_text(&peer_addr);
        driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
        driver.type_text("did:defra:live-logs-peer");
        driver.click_target(audit::targets::PEERS_SAVE);
        let live_logs_peer_id =
            wait_for_value("live logs peer connected", Duration::from_secs(10), || {
                driver.app.client.as_ref().and_then(|client| {
                    let records = driver.app.runtime.block_on(client.peer_records());
                    records
                        .iter()
                        .find(|record| record.label == "Live Logs Peer")
                        .filter(|_| client.configured_peer_count() >= 1)
                        .map(|record| record.peer_id.clone())
                })
            })?;

        driver.open_activity(Activity::Chat);
        let live_logs_chat_deployment = audit::targets::chat_deployment(&live_logs_peer_id);
        driver.wait_for_target(
            "live logs chat deployment row",
            Duration::from_secs(10),
            &live_logs_chat_deployment,
        )?;
        driver.click_target(&live_logs_chat_deployment);
        assert_eq!(
            driver.app.state.chat.selected_peer_id.as_deref(),
            Some(live_logs_peer_id.as_str())
        );
        assert_eq!(
            driver.app.state.chat.selected_agent_did.as_deref(),
            Some("did:defra:live-logs-peer")
        );

        driver.open_activity(Activity::Peers);
        driver.render();
        if driver.has_target(audit::targets::PEERS_TOGGLE_ADD_FORM) {
            driver.click_target(audit::targets::PEERS_TOGGLE_ADD_FORM);
        }
        if !driver.has_target(audit::targets::PEERS_ADD_LABEL) {
            driver.app.state.peers.show_add_form = true;
            driver.render();
        }
        driver.click_target(audit::targets::PEERS_ADD_LABEL);
        driver.type_text("Broken Logs Peer");
        driver.click_target(audit::targets::PEERS_ADD_ADDR);
        driver.type_text("iroh://bad-address");
        driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
        driver.type_text("did:defra:broken-live-logs-peer");
        driver.click_target(audit::targets::PEERS_SAVE);
        if wait_for_value(
            "live logs warning from ui peer add",
            Duration::from_secs(2),
            || {
                global_log_store()
                    .snapshot()
                    .entries
                    .iter()
                    .any(|entry| entry.message.contains("desktop peer add warning"))
                    .then_some(())
            },
        )
        .is_err()
        {
            let client = Arc::clone(
                driver
                    .app
                    .client
                    .as_ref()
                    .ok_or_else(|| anyhow!("desktop client missing"))?,
            );
            let _ = driver.app.runtime.block_on(client.add_peer(
                "Broken Logs Peer",
                "iroh://bad-address",
                "did:defra:broken-live-logs-peer",
            ));
        }
        wait_for_value(
            "live logs warning captured",
            Duration::from_secs(10),
            || {
                let snapshot = global_log_store().snapshot();
                (snapshot.total_events > baseline_events
                    && snapshot
                        .entries
                        .iter()
                        .any(|entry| entry.message.contains("desktop peer add warning")))
                .then_some(())
            },
        )?;

        driver.open_activity(Activity::Operator);
        let live_logs_operator_deployment = audit::targets::operator_deployment(&live_logs_peer_id);
        driver.wait_for_target(
            "live logs operator deployment row",
            Duration::from_secs(10),
            &live_logs_operator_deployment,
        )?;
        driver.click_target(&live_logs_operator_deployment);
        assert_eq!(
            driver.app.state.operator.selected_peer_id.as_deref(),
            Some(live_logs_peer_id.as_str())
        );
        assert_eq!(
            driver.app.state.operator.selected_agent_did.as_deref(),
            Some("did:defra:live-logs-peer")
        );
        let live_logs_operator_agent = audit::targets::operator_agent("did:defra:live-logs-peer");
        driver.wait_for_target(
            "live logs operator agent row",
            Duration::from_secs(10),
            &live_logs_operator_agent,
        )?;
        driver.click_target(&live_logs_operator_agent);
        assert_eq!(
            driver.app.state.operator.selected_agent_did.as_deref(),
            Some("did:defra:live-logs-peer")
        );
        driver.app.state.operator.selected_agent_did = Some(agent_did.clone());
        driver.app.state.operator.selected_peer_id = None;
        driver.app.state.operator.selected_entity_id = None;
        driver.app.state.operator.draft = None;
        driver.app.state.operator.draft_source_entity_id = None;
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Behaviors,
        ));
        driver.wait_for_target(
            "live logs behavior row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&fixture.docs.behavior_id),
        )?;
        driver.click_target(&audit::targets::operator_entity(&fixture.docs.behavior_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Live Logs Behavior Review",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value("live logs write captured", Duration::from_secs(10), || {
            let snapshot = global_log_store().snapshot();
            snapshot
                .entries
                .iter()
                .any(|entry| {
                    entry.category == DesktopLogCategory::Writes
                        && entry.message.contains("desktop write saved")
                        && entry
                            .fields
                            .iter()
                            .any(|field| field.value == fixture.docs.behavior_id)
                })
                .then_some(())
        })?;

        let all_texts = driver.open_activity(Activity::Logs);
        assert!(all_texts.iter().any(|text| text.contains("Live Logs")));
        let log_snapshot = global_log_store().snapshot();
        assert!(log_snapshot
            .entries
            .iter()
            .any(|entry| entry.message.contains("desktop replica snapshot refreshed")));
        assert!(log_snapshot
            .entries
            .iter()
            .any(|entry| entry.message.contains("desktop write saved")));
        assert!(log_snapshot
            .entries
            .iter()
            .any(|entry| entry.message.contains("desktop peer added")));
        assert!(log_snapshot
            .entries
            .iter()
            .any(|entry| entry.message.contains("desktop peer add warning")));
        assert!(log_snapshot.entries.iter().any(|entry| {
            entry.category == DesktopLogCategory::Turns
                && (entry.message.contains(&request_id)
                    || entry
                        .fields
                        .iter()
                        .any(|field| field.value.contains(&request_id)))
        }));

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Turns,
        )));
        let turns_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Turns)
        );
        assert_logs_filter_has_results(&turns_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Writes,
        )));
        let writes_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Writes)
        );
        assert_logs_filter_has_results(&writes_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Peering,
        )));
        let peering_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Peering)
        );
        assert_logs_filter_has_results(&peering_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Warnings,
        )));
        let warning_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Warnings)
        );
        assert_logs_filter_has_results(&warning_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Replication,
        )));
        let replication_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Replication)
        );
        assert_logs_filter_has_results(&replication_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::All));
        let all_filter_texts = driver.render();
        assert_eq!(driver.app.state.logs.filter, LogsFilter::All);
        assert_logs_filter_has_results(&all_filter_texts);
    }

    fixture.shutdown()?;
    shutdown_core(runtime.as_ref(), peer)?;
    Ok(())
}
