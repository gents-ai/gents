use super::*;

#[test]
fn desktop_app_renders_chat_activity_with_live_session_data() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;

    let principal_resp = runtime.block_on(core.node().execute(
        r#"mutation {
            add_AgentPrincipal(input: {
                agent_did: "did:defra:amy"
                display_name: "Amy"
                default_behavior_id: "amy-default"
                enabled: true
            }) { agent_did }
        }"#,
    ));
    assert!(!principal_resp.has_errors());

    let created =
        runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
    runtime.block_on(core.submit_request(
        &created.session_id,
        "did:defra:amy",
        "hello operator",
        None,
    ))?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );

    let texts = render_once(&mut app, &ctx);

    assert_eq!(app.state.activity, Activity::Chat);
    assert_eq!(
        app.state.chat.shell.selected_agent_did.as_deref(),
        Some("did:defra:amy")
    );
    assert_eq!(
        app.state.chat.shell.selected_session_id.as_deref(),
        Some(created.session_id.as_str())
    );
    assert!(!texts.iter().any(|text| text.contains("Operator Console")));
    assert!(texts.iter().any(|text| text.contains("hello operator")));
    assert!(texts.iter().any(|text| text.contains("Amy")));
    Ok(())
}

#[test]
fn desktop_app_renders_request_only_transcript_fallback() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;

    runtime.block_on(insert_agent_principal(
        &core,
        "did:defra:amy",
        "Amy",
        "amy-default",
    ))?;
    let created =
        runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
    runtime.block_on(core.submit_request(
        &created.session_id,
        "did:defra:amy",
        "request only transcript row",
        None,
    ))?;

    let mut driver = build_driver(
        Arc::clone(&runtime),
        core,
        Arc::new(DesktopLogStore::new(64)),
    );
    driver.app.state.activity = Activity::Chat;
    let texts = driver.render();

    assert_eq!(
        driver.app.state.chat.shell.selected_session_id.as_deref(),
        Some(created.session_id.as_str())
    );
    assert!(texts
        .iter()
        .any(|text| text.contains("request only transcript row")));
    assert!(texts.iter().any(|text| text.contains("waiting for claim")));
    assert!(!texts.iter().any(|text| text.contains("Transcript Empty")));
    Ok(())
}

#[test]
fn desktop_app_chat_header_retry_and_export_use_transcript_state() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;

    let principal_resp = runtime.block_on(core.node().execute(
        r#"mutation {
            add_AgentPrincipal(input: {
                agent_did: "did:defra:amy"
                display_name: "Amy"
                default_behavior_id: "amy-default"
                enabled: true
            }) { agent_did }
        }"#,
    ));
    assert!(!principal_resp.has_errors());

    let created =
        runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
    let submitted = runtime.block_on(core.submit_request(
        &created.session_id,
        "did:defra:amy",
        "hello operator",
        None,
    ))?;
    let response = runtime.block_on(core.node().execute(&format!(
        r#"mutation {{
            add_AgentResponse(input: {{
                response_key: "response-retry-export-1"
                request_id: "{request_id}"
                agent_did: "did:defra:amy"
                behavior_id: "amy-default"
                session_id: "{session_id}"
                content: "assistant complete"
                reasoning: ""
                status: "completed"
                error_message: ""
                token_count: 2
                progress_seq: 1
                created_at: "2026-04-14T00:00:04Z"
                completed_at: "2026-04-14T00:00:05Z"
            }}) {{ response_key }}
        }}"#,
        request_id = escape_graphql_string(&submitted.request_id),
        session_id = escape_graphql_string(&created.session_id),
    )));
    assert!(!response.has_errors());
    runtime.block_on(core.refresh_store())?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    let initial = driver.render();
    assert!(initial.iter().any(|text| text.contains("Retry")));
    assert!(initial.iter().any(|text| text.contains("Export")));
    assert!(initial.iter().any(|text| text.contains("completed")));
    assert!(driver.has_target(audit::targets::CHAT_RETRY));
    assert!(driver.has_target(audit::targets::CHAT_EXPORT));

    let selected_session_id = driver.app.state.chat.shell.selected_session_id.clone();
    driver.click_interactable_target(audit::targets::CHAT_EXPORT)?;
    let export_payload = driver
        .app
        .state
        .chat
        .editor
        .last_export_payload
        .as_deref()
        .ok_or_else(|| anyhow!("chat export did not capture a payload"))?;
    assert!(export_payload.contains("hello operator"));
    assert!(export_payload.contains("assistant complete"));
    assert!(export_payload.contains(&submitted.request_id));

    driver.click_interactable_target(audit::targets::CHAT_RETRY)?;
    let retry_request = driver
        .app
        .client
        .as_ref()
        .and_then(|client| {
            client
                .store()
                .snapshot()
                .requests
                .iter()
                .find(|row| row.retry_parent_request.as_deref() == Some(&submitted.request_id))
                .cloned()
        })
        .ok_or_else(|| anyhow!("retry request was not created"))?;

    assert_eq!(
        driver.app.state.chat.shell.selected_session_id,
        selected_session_id
    );
    assert_eq!(driver.app.state.chat.editor.last_submission_error, None);
    assert_eq!(retry_request.content.as_deref(), Some("hello operator"));
    assert_eq!(
        retry_request.retry_root_request.as_deref(),
        Some(submitted.request_id.as_str())
    );
    assert_eq!(retry_request.retry_count, Some(1));
    Ok(())
}

#[test]
fn desktop_app_renders_chat_first_conversation_nudge() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;

    let principal_resp = runtime.block_on(core.node().execute(
        r#"mutation {
            add_AgentPrincipal(input: {
                agent_did: "did:defra:amy"
                display_name: "Amy"
                default_behavior_id: "amy-default"
                enabled: true
            }) { agent_did }
        }"#,
    ));
    assert!(!principal_resp.has_errors());
    runtime.block_on(core.refresh_store())?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    app.state.activity = Activity::Chat;

    let texts = render_once(&mut app, &ctx);

    assert_eq!(app.state.activity, Activity::Chat);
    assert_eq!(
        app.state.chat.shell.selected_agent_did.as_deref(),
        Some("did:defra:amy")
    );
    assert!(app.state.chat.shell.selected_session_id.is_some());
    assert!(texts.iter().any(|text| text.contains("Transcript Empty")));
    assert!(!texts
        .iter()
        .any(|text| text.contains("Automatic conversation creation did not complete")));
    Ok(())
}

#[test]
fn desktop_app_auto_creates_first_chat_conversation() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;

    let principal_resp = runtime.block_on(core.node().execute(
        r#"mutation {
            add_AgentPrincipal(input: {
                agent_did: "did:defra:amy"
                display_name: "Amy"
                default_behavior_id: "amy-default"
                enabled: true
            }) { agent_did }
        }"#,
    ));
    assert!(!principal_resp.has_errors());
    runtime.block_on(core.refresh_store())?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    let texts = driver.render();

    assert!(driver.app.state.chat.shell.selected_session_id.is_some());
    assert!(texts.iter().any(|text| text.contains("Transcript Empty")));
    assert!(!texts
        .iter()
        .any(|text| text.contains("Automatic conversation creation did not complete")));
    Ok(())
}

#[test]
fn desktop_app_clicks_through_activity_bar_navigation() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;
    let log_store = Arc::new(DesktopLogStore::new(64));
    log_store.record_manual(
        chrono::Utc::now(),
        tracing::Level::INFO,
        "defra_agent_desktop::replication",
        "activity navigation marker",
        [("marker", "activity".to_string())],
    );

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        log_store,
    );
    app.state.onboarding.first_launch_redirect_done = true;
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    let chat_texts = driver.render();
    assert!(chat_texts.iter().any(|text| text.contains("DEFRA DESKTOP")));
    assert!(chat_texts.iter().any(|text| text.contains("CHAT")));
    assert!(chat_texts.iter().any(|text| text.contains("conversations")));
    assert!(chat_texts.iter().any(|text| text.contains("LOGS")));
    assert!(chat_texts
        .iter()
        .any(|text| text.contains("Add Deployment")));

    let logs_texts = driver.open_activity(Activity::Logs);
    assert_eq!(driver.app.state.activity, Activity::Logs);
    assert!(logs_texts.iter().any(|text| text.contains("Live Logs")));
    assert!(logs_texts.iter().any(|text| text.contains("Log Controls")));

    let operator_texts = driver.open_activity(Activity::Operator);
    assert_eq!(driver.app.state.activity, Activity::Operator);
    assert!(operator_texts
        .iter()
        .any(|text| text.contains("Operator Console")));
    assert!(operator_texts.iter().any(|text| text.contains("OPERATOR")));

    let peers_texts = driver.open_activity(Activity::Peers);
    assert_eq!(driver.app.state.activity, Activity::Peers);
    assert!(peers_texts
        .iter()
        .any(|text| text.contains("Add Your First Deployment")));
    assert!(peers_texts.iter().any(|text| text.contains("PEERS")));

    let back_to_chat = driver.open_activity(Activity::Chat);
    assert_eq!(driver.app.state.activity, Activity::Chat);
    assert!(back_to_chat
        .iter()
        .any(|text| text.contains("Add Deployment")));
    Ok(())
}

#[test]
fn desktop_app_clicks_through_chat_deployment_and_conversation_switching() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer_alpha = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("peer-alpha")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer_beta = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("peer-beta")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer_alpha_addr = peer_alpha
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("peer alpha missing listen address"))?;
    let peer_beta_addr = peer_beta
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("peer beta missing listen address"))?;
    let alpha_peer =
        runtime.block_on(core.add_peer("Alpha Bay", &peer_alpha_addr, "did:defra:amy"))?;
    let beta_peer =
        runtime.block_on(core.add_peer("Beta Bay", &peer_beta_addr, "did:defra:bob"))?;
    runtime.block_on(insert_agent_principal(
        &core,
        "did:defra:amy",
        "Amy",
        "amy-default",
    ))?;
    runtime.block_on(insert_agent_principal(
        &core,
        "did:defra:bob",
        "Bob",
        "bob-default",
    ))?;
    let amy_session =
        runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
    runtime.block_on(core.submit_request(
        &amy_session.session_id,
        "did:defra:amy",
        "amy conversation request",
        None,
    ))?;
    let bob_first =
        runtime.block_on(core.create_conversation("did:defra:bob", Some("bob-default")))?;
    runtime.block_on(core.submit_request(
        &bob_first.session_id,
        "did:defra:bob",
        "bob first request",
        None,
    ))?;
    let bob_second =
        runtime.block_on(core.create_conversation("did:defra:bob", Some("bob-default")))?;
    runtime.block_on(core.submit_request(
        &bob_second.session_id,
        "did:defra:bob",
        "bob second request",
        None,
    ))?;
    runtime.block_on(core.refresh_store())?;

    let mut driver = build_driver(
        Arc::clone(&runtime),
        core,
        Arc::new(DesktopLogStore::new(64)),
    );
    driver.app.state.onboarding.first_launch_redirect_done = true;
    driver.app.state.activity = Activity::Chat;
    driver.render();
    driver.click_target(&audit::targets::chat_deployment(&alpha_peer.peer_id));
    driver.click_target(&audit::targets::chat_conversation(&amy_session.session_id));
    let initial = driver.render();

    assert_eq!(
        driver.app.state.chat.shell.selected_peer_id.as_deref(),
        Some(alpha_peer.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.chat.shell.selected_agent_did.as_deref(),
        Some("did:defra:amy")
    );
    assert!(initial
        .iter()
        .any(|text| text.contains("amy conversation request")));

    driver.click_target(&audit::targets::chat_deployment(&beta_peer.peer_id));
    let beta_texts = driver.render();
    assert_eq!(
        driver.app.state.chat.shell.selected_agent_did.as_deref(),
        Some("did:defra:bob")
    );
    assert_eq!(
        driver.app.state.chat.shell.selected_session_id.as_deref(),
        None
    );
    assert!(!beta_texts.is_empty());

    driver.click_target(&audit::targets::chat_deployment(&alpha_peer.peer_id));
    driver.click_target(&audit::targets::chat_agent("did:defra:bob"));
    let beta_agent_texts = driver.render();
    assert_eq!(
        driver.app.state.chat.shell.selected_peer_id.as_deref(),
        Some(beta_peer.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.chat.shell.selected_agent_did.as_deref(),
        Some("did:defra:bob")
    );
    assert_eq!(driver.app.state.chat.shell.selected_session_id, None);
    assert!(!beta_agent_texts.is_empty());

    driver.click_target(&audit::targets::chat_conversation(&bob_first.session_id));
    let switched = driver.render();
    assert_eq!(
        driver.app.state.chat.shell.selected_session_id.as_deref(),
        Some(bob_first.session_id.as_str())
    );
    assert!(switched
        .iter()
        .any(|text| text.contains("bob first request")));
    driver.app.shutdown_client();
    shutdown_core(runtime.as_ref(), peer_alpha)?;
    shutdown_core(runtime.as_ref(), peer_beta)?;
    Ok(())
}

#[test]
fn desktop_app_clicks_through_chat_reasoning_and_tool_card_disclosures() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;
    runtime.block_on(insert_agent_principal(
        &core,
        "did:defra:amy",
        "Amy",
        "amy-default",
    ))?;
    let conversation =
        runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
    runtime.block_on(insert_chat_transcript_documents(
        &core,
        &conversation.session_id,
        "did:defra:amy",
        "amy-default",
        "response-disclosure-1",
    ))?;

    let mut driver = build_driver(
        Arc::clone(&runtime),
        core,
        Arc::new(DesktopLogStore::new(64)),
    );
    driver.app.state.onboarding.first_launch_redirect_done = true;
    driver.app.state.activity = Activity::Chat;
    let initial = driver.render();

    assert!(initial
        .iter()
        .any(|text| text.contains("I checked the queue and opened the trace.")));
    assert!(initial.iter().any(|text| text.contains("Queue checked.")));
    assert!(!initial.iter().any(|text| text.contains("\"call_id\"")));
    assert!(initial
        .iter()
        .any(|text| text.contains("REASONING DISCLOSURE")));
    assert!(!initial
        .iter()
        .any(|text| text.contains("I verified the latest request")));

    driver.click_target(&audit::targets::chat_tool_card("call-shell-1"));
    let tool_texts = driver.render();
    assert!(driver
        .app
        .state
        .chat
        .editor
        .expanded_tool_cards
        .contains("call-shell-1"));
    assert!(tool_texts.iter().any(|text| text.contains("Args")));
    assert!(tool_texts.iter().any(|text| text.contains("Output")));
    assert!(tool_texts.iter().any(|text| text.contains("completed")));
    assert!(!tool_texts.iter().any(|text| text.contains("ARGS")));
    assert!(!tool_texts
        .iter()
        .any(|text| text.contains("src/app.rs: audit target live")));
    driver.click_target(&audit::targets::chat_tool_output("call-shell-1"));
    let output_texts = driver.render();
    assert!(output_texts.iter().any(|text| text.contains("TOOL OUTPUT")));
    assert!(output_texts
        .iter()
        .any(|text| text.contains("src/app.rs: audit target live")));

    driver.click_target(&audit::targets::chat_reasoning("response-disclosure-1"));
    let reasoning_texts = driver.render();
    assert!(driver
        .app
        .state
        .chat
        .editor
        .expanded_reasoning_cards
        .contains("reasoning:response-disclosure-1"));
    assert!(reasoning_texts
        .iter()
        .any(|text| text.contains("I verified the latest request")));
    Ok(())
}

#[test]
fn desktop_app_clicks_through_chat_send_without_precreating_conversation() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;
    let mock_endpoint = MockModelEndpoint::start("default")?;
    let running_agent = runtime.block_on(spawn_backed_agent(
        core.node_arc(),
        tempdir.path().join("agent").join("audit-direct-send.key"),
        "audit-direct-send",
        &AgentBackendConfig::mock(mock_endpoint.endpoint()),
    ))?;
    runtime.block_on(core.refresh_store())?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    let _session_id = ensure_chat_session_selected(
        &mut driver,
        "chat session ready before direct send",
        Duration::from_secs(5),
    )?;
    assert_eq!(
        driver.app.state.chat.shell.selected_agent_did.as_deref(),
        Some(running_agent.did.as_str())
    );

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text("send directly without creating the session first");
    driver.render();
    driver.click_target(audit::targets::CHAT_SEND);
    if wait_for_value(
        "session created by first direct-send click",
        Duration::from_secs(1),
        || driver.app.state.chat.shell.selected_session_id.clone(),
    )
    .is_err()
    {
        driver.render();
        driver.click_target(audit::targets::CHAT_SEND);
    }

    let session_id = wait_for_value(
        "session created by direct send",
        Duration::from_secs(5),
        || driver.app.state.chat.shell.selected_session_id.clone(),
    )?;
    assert!(driver.app.state.chat.editor.last_submission_error.is_none());
    assert!(driver.app.state.chat.editor.composer_text.is_empty());

    let request_id = wait_for_value(
        "direct-send focused request id",
        Duration::from_secs(5),
        || {
            driver
                .app
                .client
                .as_ref()
                .and_then(|client| client.store().focused_request_id())
        },
    )?;
    wait_for_value(
        "direct-send response row in store",
        Duration::from_secs(10),
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
    let transcript_texts = wait_for_value(
        "direct-send transcript response",
        Duration::from_secs(10),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("mock response"))
                .then_some(texts)
        },
    )?;
    assert_eq!(
        driver.app.state.chat.shell.selected_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert!(transcript_texts
        .iter()
        .any(|text| { text.contains("send directly without creating the session first") }));

    runtime.block_on(running_agent.shutdown())?;
    Ok(())
}

#[test]
fn desktop_app_blocks_chat_send_while_turn_is_waiting_for_claim() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;
    runtime.block_on(seed_operator_documents(&core))?;
    let created =
        runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
    runtime.block_on(core.submit_request(
        &created.session_id,
        "did:defra:amy",
        "existing pending request",
        Some("amy-default"),
    ))?;
    runtime.block_on(core.refresh_store())?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    app.state.activity = Activity::Chat;
    app.state.chat.shell.selected_agent_did = Some("did:defra:amy".to_string());
    app.state.chat.shell.selected_session_id = Some(created.session_id.clone());
    let mut driver = AuditDriver::new(app, ctx);

    let waiting_texts = wait_for_value(
        "waiting-for-claim turn state",
        Duration::from_secs(5),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("waiting for claim"))
                .then_some(texts)
        },
    )?;
    assert!(waiting_texts
        .iter()
        .any(|text| text.contains("existing pending request")));

    let initial_request_count = driver
        .app
        .client
        .as_ref()
        .map(|client| client.store().snapshot().requests.len())
        .ok_or_else(|| anyhow!("desktop client missing"))?;

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text("blocked follow-up");
    driver.click_target(audit::targets::CHAT_SEND);
    driver.press_key(
        egui::Key::Enter,
        egui::Modifiers {
            command: true,
            mac_cmd: true,
            ..Default::default()
        },
    );
    driver.render();

    let request_count_after = driver
        .app
        .client
        .as_ref()
        .map(|client| client.store().snapshot().requests.len())
        .ok_or_else(|| anyhow!("desktop client missing"))?;
    assert_eq!(request_count_after, initial_request_count);
    assert_eq!(
        driver.app.state.chat.editor.composer_text,
        "blocked follow-up"
    );
    assert_eq!(driver.app.state.chat.editor.last_submission_error, None);
    Ok(())
}

#[test]
fn desktop_app_clicks_through_live_agent_submission() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;
    let mock_endpoint = MockModelEndpoint::start("default")?;
    let running_agent = runtime.block_on(spawn_backed_agent(
        core.node_arc(),
        tempdir.path().join("agent").join("audit-live.key"),
        "audit-live",
        &AgentBackendConfig::mock(mock_endpoint.endpoint()),
    ))?;
    runtime.block_on(core.refresh_store())?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    assert!(driver
        .app
        .client
        .as_ref()
        .expect("desktop client should exist")
        .store()
        .snapshot()
        .agent_principals
        .iter()
        .any(|row| row.agent_did == running_agent.did));

    let initial = driver.render();
    ensure_chat_agent_selected(
        &mut driver,
        "chat agent selected for live submission",
        Duration::from_secs(5),
    )?;
    assert_eq!(
        driver.app.state.chat.shell.selected_agent_did.as_deref(),
        Some(running_agent.did.as_str())
    );
    let _session_id = ensure_chat_session_selected(
        &mut driver,
        "chat session selected for live submission",
        Duration::from_secs(5),
    )?;
    let after_create = driver.render();
    assert!(driver.app.state.chat.shell.selected_session_id.is_some());
    assert!(!initial
        .iter()
        .any(|text| text.contains("Automatic conversation creation did not complete")));
    assert!(after_create
        .iter()
        .any(|text| text.contains("Transcript Empty")));

    driver.click_target(audit::targets::CHAT_COMPOSER_TEXT);
    driver.type_text("say hello from the desktop audit");
    assert_eq!(
        driver.app.state.chat.editor.composer_text,
        "say hello from the desktop audit"
    );
    driver.press_key(
        egui::Key::Enter,
        egui::Modifiers {
            command: true,
            mac_cmd: true,
            ..Default::default()
        },
    );
    assert_eq!(driver.app.state.chat.editor.last_submission_error, None);
    assert!(driver.app.state.chat.editor.composer_text.is_empty());

    let request_id = wait_for_value("focused request id", Duration::from_secs(5), || {
        driver
            .app
            .client
            .as_ref()
            .and_then(|client| client.store().focused_request_id())
    })?;
    wait_for_value(
        "response row in client store",
        Duration::from_secs(10),
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
    let response_texts = wait_for_value(
        "mock response in transcript",
        Duration::from_secs(10),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("mock response"))
                .then_some(texts)
        },
    )?;
    assert!(response_texts
        .iter()
        .any(|text| text.contains("say hello from the desktop audit")));
    assert!(driver
        .app
        .client
        .as_ref()
        .expect("desktop client should exist")
        .store()
        .snapshot()
        .responses
        .iter()
        .any(|row| row.request_id.as_deref() == Some(request_id.as_str())));

    runtime.block_on(running_agent.shutdown())?;
    Ok(())
}

#[test]
fn desktop_app_executes_tool_loop_and_renders_tool_output() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;
    let mock_endpoint = MockModelEndpoint::start_tool_loop("default", "tool loop complete")?;
    let running_agent = runtime.block_on(spawn_backed_agent(
        core.node_arc(),
        tempdir.path().join("agent").join("audit-tool-loop.key"),
        "audit-tool-loop",
        &AgentBackendConfig::mock(mock_endpoint.endpoint()),
    ))?;
    let agent_did = running_agent.did.clone();
    let tool_token = running_agent.tool_token.clone();
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let backend_id = "audit-tool-loop-backend".to_string();
    let tool_selection_id = format!("{behavior_id}:tools");

    runtime.block_on(core.refresh_store())?;
    let initial_generation = core
        .store()
        .snapshot()
        .latest_runtime(&agent_did)
        .and_then(|row| row.router_generation.or(row.active_generation))
        .unwrap_or_default();

    runtime.block_on(async {
        core.save_tool_selection(&ToolSelectionRow {
            selection_id: tool_selection_id.clone(),
            agent_did: Some(agent_did.clone()),
            display_name: Some("Tool Loop Selection".to_string()),
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
            agent_did: Some(agent_did.clone()),
            display_name: Some("Tool Loop Behavior".to_string()),
            system_prompt: Some(
                "When asked to read notes.txt, call read_file and then answer with only the token."
                    .to_string(),
            ),
            backend_id: Some(backend_id.clone()),
            model_name: Some("default".to_string()),
            tool_selection_id: Some(tool_selection_id.clone()),
            inference_profile_id: None,
            compaction_strategy: Some("StripThenSummarize".to_string()),
            compaction_threshold: Some(0.95),
            enabled: Some(true),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
        })
        .await?;
        core.refresh_store().await?;
        Ok::<(), anyhow::Error>(())
    })?;

    wait_for_value(
        "tool loop runtime reconciled with file tools",
        Duration::from_secs(15),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let generation_ready = snapshot
                .latest_runtime(&agent_did)
                .and_then(|row| row.router_generation.or(row.active_generation))
                .is_some_and(|generation| generation > initial_generation);
            let tools_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == tool_selection_id)
                .is_some_and(|row| {
                    row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            (generation_ready && tools_ready).then_some(())
        },
    )?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    let session_id = ensure_chat_session_selected(
        &mut driver,
        "tool loop selected session",
        Duration::from_secs(5),
    )?;

    let prompt =
        "Use the read_file tool to read notes.txt. Reply with only the token from that file.";
    let (request_id, response) = submit_chat_message_and_wait_for_response(&mut driver, prompt)?;
    assert!(
        response.contains(&tool_token),
        "expected final response to contain tool token {tool_token}, got {response}"
    );

    let (tool_card_id, tool_output) = wait_for_value(
        "tool loop persisted tool call output",
        Duration::from_secs(15),
        || {
            driver.app.client.as_ref().and_then(|client| {
                let snapshot = client.store().snapshot();
                snapshot
                    .requests
                    .iter()
                    .find(|row| row.request_id == request_id)
                    .filter(|row| row.agent_did.as_deref() == Some(agent_did.as_str()))?;
                let transcript = snapshot.transcript(&session_id);
                let tool_call = transcript.tool_calls.iter().find(|row| {
                    row.tool_name.as_deref() == Some("read_file")
                        && row.status.as_deref() == Some("completed")
                })?;
                let output = transcript
                    .tool_results
                    .iter()
                    .find(|result| result.tool_name == tool_call.tool_name)
                    .and_then(|result| result.output_text.clone())
                    .or_else(|| tool_call.result.clone())
                    .filter(|value| value.contains(&tool_token))?;
                let card_id = tool_call
                    .tool_call_id
                    .clone()
                    .or_else(|| Some(tool_call.tool_call_key.clone()))
                    .unwrap_or_else(|| tool_call.tool_name.clone().unwrap_or_default());
                Some((card_id, output))
            })
        },
    )?;

    let tool_target = audit::targets::chat_tool_card(&tool_card_id);
    driver.wait_for_target(
        "tool loop card target",
        Duration::from_secs(10),
        &tool_target,
    )?;
    driver.click_interactable_target(&tool_target)?;
    let tool_texts = driver.render();
    assert!(tool_texts.iter().any(|text| text.contains("Args")));
    assert!(tool_texts.iter().any(|text| text.contains("Output")));
    assert!(!tool_texts.iter().any(|text| text.contains("OUTPUT")));
    let output_target = audit::targets::chat_tool_output(&tool_card_id);
    driver.click_interactable_target(&output_target)?;
    let output_modal = driver.render();
    assert!(tool_output.contains(&tool_token));
    assert!(output_modal
        .iter()
        .any(|text| text.contains(&tool_token) || tool_output.contains(text.trim())));

    runtime.block_on(running_agent.shutdown())?;
    Ok(())
}

#[test]
fn desktop_app_clicks_through_live_agent_multi_turn_conversation() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;
    let mock_endpoint = MockModelEndpoint::start("default")?;
    let running_agent = runtime.block_on(spawn_backed_agent(
        core.node_arc(),
        tempdir.path().join("agent").join("audit-live-multi.key"),
        "audit-live-multi",
        &AgentBackendConfig::mock(mock_endpoint.endpoint()),
    ))?;
    runtime.block_on(core.refresh_store())?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let mut app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    app.state.activity = Activity::Chat;
    let mut driver = AuditDriver::new(app, ctx);

    let session_id = ensure_chat_session_selected(
        &mut driver,
        "session selected for multi-turn audit",
        Duration::from_secs(5),
    )?;

    let (_first_request_id, first_response) =
        submit_chat_message_and_wait_for_response(&mut driver, "first desktop audit turn")?;
    let (second_request_id, second_response) =
        submit_chat_message_and_wait_for_response(&mut driver, "follow up desktop audit turn")?;
    assert_eq!(first_response, "mock response");
    assert_eq!(second_response, "mock response");

    wait_for_value(
        "multi-turn conversation state persisted",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                let snapshot = client.store().snapshot();
                let conversation = snapshot
                    .conversations
                    .iter()
                    .find(|row| row.session_id == session_id)?;
                (snapshot.requests_for_session(&session_id).len() == 2
                    && snapshot
                        .responses
                        .iter()
                        .filter(|row| row.session_id.as_deref() == Some(session_id.as_str()))
                        .count()
                        >= 2
                    && conversation.latest_request_id.as_deref()
                        == Some(second_request_id.as_str())
                    && conversation.preview_text.as_deref() == Some("follow up desktop audit turn"))
                .then_some(())
            })
        },
    )?;

    let final_texts = driver.render();
    assert!(final_texts
        .iter()
        .any(|text| text.contains("first desktop audit turn")));
    assert!(final_texts
        .iter()
        .any(|text| text.contains("follow up desktop audit turn")));
    assert!(final_texts.iter().any(|text| text.contains("completed")));

    runtime.block_on(running_agent.shutdown())?;
    Ok(())
}

#[test]
fn desktop_app_places_tool_cards_in_tool_turn_without_raw_duplicate_message() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;
    runtime.block_on(insert_agent_principal(
        &core,
        "did:defra:amy",
        "Amy",
        "amy-default",
    ))?;
    let conversation =
        runtime.block_on(core.create_conversation("did:defra:amy", Some("amy-default")))?;
    runtime.block_on(insert_chat_transcript_documents(
        &core,
        &conversation.session_id,
        "did:defra:amy",
        "amy-default",
        "response-tool-structure-1",
    ))?;

    let mut driver = build_driver(
        Arc::clone(&runtime),
        core,
        Arc::new(DesktopLogStore::new(64)),
    );
    driver.app.state.activity = Activity::Chat;
    let texts = driver.render();

    assert!(texts.iter().any(|text| text.contains("TOOL")));
    assert!(texts.iter().any(|text| text.contains("shell  completed")));
    assert!(!texts
        .iter()
        .any(|text| text.contains("src/app.rs: audit target live")));
    assert!(!texts.iter().any(|text| text.contains("\"call_id\"")));
    Ok(())
}
