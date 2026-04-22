use super::*;

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
    // `seed_desktop_origin_config_docs` only seeds the config documents
    // that the replication probe waits for (backend/profile/tools/
    // behavior); it does not write Task or Schedule rows. We still
    // produce stable Task/Schedule identifiers so callers depending on
    // `LiveAgentDocs` have the complete shape.
    let task_id = format!("{behavior_id}:task");
    let schedule_id = format!("{behavior_id}:schedule");

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
        task_id,
        schedule_id,
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
