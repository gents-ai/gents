use super::*;

use crate::client::PeerDirectory;

#[test]
fn desktop_bootstrap_init_launch_and_gui_chat_round_trip_without_manual_refresh() -> Result<()> {
    init_test_tracing();

    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let mock_endpoint = MockModelEndpoint::start("default")?;
    let backend = AgentBackendConfig::mock(mock_endpoint.endpoint());

    let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote")),
        bootstrap_live_core_options(),
    ))?);
    let agent_name = "bootstrap-demo";
    let running_agent = runtime.block_on(spawn_backed_agent(
        remote_core.node_arc(),
        tempdir.path().join("agent").join("bootstrap-demo.key"),
        agent_name,
        &backend,
    ))?;
    let docs = runtime.block_on(seed_live_operator_documents(
        remote_core.as_ref(),
        &running_agent.did,
        agent_name,
        &backend,
    ))?;
    let remote_addr = runtime.block_on(wait_for_connectable_iroh_addr(
        remote_core.as_ref(),
        "mock runtime",
    ))?;
    let runtime_api =
        BootstrapRuntimeApi::start(&runtime, Arc::clone(&remote_core), remote_addr.clone())?;

    let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
    let agent_home = tempdir.path().join("agent-home");
    seed_agent_home_runtime_state_for_bootstrap(
        &agent_home,
        agent_name,
        &running_agent.did,
        runtime_api.graphql_url(),
        remote_core.local_peer_id(),
        &remote_addr,
    )?;

    let init_summary = runtime.block_on(crate::local_runtime::init_standard_local_runtime(
        crate::local_runtime::DesktopInitOptions {
            agent_home: agent_home.clone(),
            desktop_paths: desktop_paths.clone(),
            label: "Bootstrap Demo".to_string(),
        },
    ))?;
    assert_eq!(init_summary.status, "initialized");
    assert_eq!(init_summary.graphql, runtime_api.graphql_url());
    assert_eq!(init_summary.p2p_transport, "iroh");

    let peer_record = runtime.block_on(async {
        let directory = PeerDirectory::load(desktop_paths.peer_directory_path()).await?;
        directory
            .records()
            .first()
            .cloned()
            .context("desktop init did not persist a peer record")
    })?;

    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        desktop_paths,
        bootstrap_live_core_options(),
    ))?;
    assert!(
        desktop_core.bootstrap_errors().is_empty(),
        "unexpected bootstrap errors: {:?}",
        desktop_core.bootstrap_errors()
    );
    runtime.block_on(wait_for_connected_peer(
        &desktop_core,
        remote_core.local_peer_id(),
        "desktop bootstrap",
    ))?;
    runtime.block_on(wait_for_connected_peer(
        remote_core.as_ref(),
        desktop_core.local_peer_id(),
        "mock runtime bootstrap",
    ))?;
    drop(runtime_api);

    wait_for_value(
        "bootstrapped behavior docs on desktop",
        Duration::from_secs(20),
        || {
            desktop_core
                .store()
                .snapshot()
                .behaviors
                .iter()
                .find(|row| row.behavior_id == docs.behavior_id)
                .map(|row| row.behavior_id.clone())
        },
    )?;
    wait_for_value(
        "bootstrapped inference backend docs on desktop",
        Duration::from_secs(20),
        || {
            desktop_core
                .store()
                .snapshot()
                .inference_backends
                .iter()
                .find(|row| row.backend_id == docs.backend_id)
                .map(|row| row.backend_id.clone())
        },
    )?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(desktop_core)),
        Vec::new(),
        global_log_store(),
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    wait_for_value("desktop bootstrap status", Duration::from_secs(10), || {
        let texts = driver.render();
        texts
            .iter()
            .any(|text| text.contains("replication: subscriptions armed"))
            .then_some(texts)
    })?;

    let deployment_target = audit::targets::chat_deployment(&peer_record.peer_id);
    driver.wait_for_target(
        "bootstrapped deployment row",
        Duration::from_secs(10),
        &deployment_target,
    )?;
    driver.click_target(&deployment_target);
    assert_eq!(
        driver.app.state.chat.shell.selected_peer_id.as_deref(),
        Some(peer_record.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.chat.shell.selected_agent_did.as_deref(),
        Some(running_agent.did.as_str())
    );

    let session_id = ensure_chat_session_selected(
        &mut driver,
        "desktop-created session",
        Duration::from_secs(10),
    )?;

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text("say hello from the saved-peer bootstrap journey");
    driver.click_target(audit::targets::CHAT_SEND);
    assert_eq!(driver.app.state.chat.editor.last_submission_error, None);
    assert!(driver.app.state.chat.editor.composer_text.is_empty());

    let request_id = wait_for_value(
        "bootstrapped focused request id",
        Duration::from_secs(10),
        || {
            driver
                .app
                .client
                .as_ref()
                .and_then(|client| client.store().focused_request_id())
        },
    )?;
    wait_for_value(
        "mock runtime received bootstrapped request",
        Duration::from_secs(20),
        || {
            runtime
                .block_on(query_has_row_by_unique_field(
                    remote_core.as_ref(),
                    "AgentRequest",
                    "request_id",
                    &request_id,
                ))
                .ok()
                .filter(|received| *received)
                .map(|_| ())
        },
    )?;
    wait_for_value(
        "bootstrapped response row on desktop",
        Duration::from_secs(30),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .latest_response_for_request(&request_id)
                    .and_then(|row| row.content.as_deref())
                    .filter(|content| !content.trim().is_empty())
                    .map(str::to_string)
            })
        },
    )?;
    let transcript = wait_for_value(
        "bootstrapped transcript response",
        Duration::from_secs(30),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("mock response"))
                .then_some(texts)
        },
    )?;
    assert!(transcript
        .iter()
        .any(|text| { text.contains("say hello from the saved-peer bootstrap journey") }));
    assert_eq!(
        driver.app.state.chat.shell.selected_session_id.as_deref(),
        Some(session_id.as_str())
    );

    driver.app.shutdown_client();
    drop(driver);
    runtime.block_on(running_agent.shutdown())?;
    runtime.block_on(remote_core.shutdown())?;
    Ok(())
}

#[test]
fn desktop_bootstrap_multi_agent_gui_switching_round_trip_without_manual_refresh() -> Result<()> {
    init_test_tracing();

    let mock_endpoint = MockModelEndpoint::start("default")?;
    let backend = AgentBackendConfig::mock(mock_endpoint.endpoint());
    let mut fixture = build_multi_agent_desktop_fixture_with_backend(
        "audit-bootstrap-multi-agent",
        &backend,
        global_log_store(),
    )?;
    assert_eq!(fixture.deployments.len(), 2);

    let alpha = live_deployment_case(&fixture.deployments[0]);
    let bravo = live_deployment_case(&fixture.deployments[1]);

    let (alpha_submission, bravo_submission);
    {
        let driver = &mut fixture.driver;
        alpha_submission =
            submit_live_prompt_for_deployment(driver, &alpha, "ALPHA_BOOTSTRAP_READY")?;
        bravo_submission =
            submit_live_prompt_for_deployment(driver, &bravo, "BRAVO_BOOTSTRAP_READY")?;
    }

    wait_for_value(
        "alpha mock runtime received bootstrapped request",
        Duration::from_secs(20),
        || {
            fixture
                .runtime
                .block_on(query_has_row_by_unique_field(
                    alpha.remote_core,
                    "AgentRequest",
                    "request_id",
                    &alpha_submission.request_id,
                ))
                .ok()
                .filter(|received| *received)
                .map(|_| ())
        },
    )?;
    wait_for_value(
        "bravo mock runtime received bootstrapped request",
        Duration::from_secs(20),
        || {
            fixture
                .runtime
                .block_on(query_has_row_by_unique_field(
                    bravo.remote_core,
                    "AgentRequest",
                    "request_id",
                    &bravo_submission.request_id,
                ))
                .ok()
                .filter(|received| *received)
                .map(|_| ())
        },
    )?;

    {
        let driver = &mut fixture.driver;
        open_chat_conversation_and_assert_isolation(
            driver,
            &alpha,
            &alpha_submission,
            bravo_submission.prompt.as_str(),
            "alpha transcript leaked bravo prompt after bootstrap switching",
        )?;
        open_chat_conversation_and_assert_isolation(
            driver,
            &bravo,
            &bravo_submission,
            alpha_submission.prompt.as_str(),
            "bravo transcript leaked alpha prompt after bootstrap switching",
        )?;

        open_operator_entity_and_assert_visibility(
            driver,
            &alpha,
            OperatorSection::Behaviors,
            &alpha.docs.behavior_id,
            &[bravo.docs.behavior_id.as_str()],
            "alpha behavior row after bootstrap operator switch",
        )?;
        open_operator_request_timeline_and_assert_visibility(
            driver,
            &alpha,
            &alpha_submission.request_id,
            &[bravo_submission.request_id.as_str()],
            "alpha request row after bootstrap operator switch",
        )?;
        open_operator_entity_and_assert_visibility(
            driver,
            &bravo,
            OperatorSection::Behaviors,
            &bravo.docs.behavior_id,
            &[alpha.docs.behavior_id.as_str()],
            "bravo behavior row after bootstrap operator switch",
        )?;
        open_operator_request_timeline_and_assert_visibility(
            driver,
            &bravo,
            &bravo_submission.request_id,
            &[alpha_submission.request_id.as_str()],
            "bravo request row after bootstrap operator switch",
        )?;
    }

    fixture.shutdown()
}
