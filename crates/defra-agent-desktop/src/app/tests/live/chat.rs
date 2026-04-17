use super::*;

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
    let running_agent = runtime.block_on(spawn_backed_agent(
        core.node_arc(),
        tempdir.path().join("agent").join("audit-live.key"),
        "audit-live-remote",
        &backend,
    ))?;
    let live_agent_did = running_agent.did.clone();
    runtime.block_on(core.refresh_store())?;
    let initial_generation = core
        .store()
        .snapshot()
        .latest_runtime(&live_agent_did)
        .and_then(|row| row.router_generation.or(row.active_generation))
        .unwrap_or_default();
    let live_behavior_id = default_behavior_id_for_agent(&live_agent_did);
    let live_tool_selection_id = format!("{live_behavior_id}:tools");
    let live_tool_prompt = "When the user asks about local files, the token is not available in the conversation and you must not guess. Call read_file separately for every requested path in order, then reply with only the requested file tokens.";
    let first_tokens = vec![
        uuid::Uuid::new_v4().simple().to_string(),
        uuid::Uuid::new_v4().simple().to_string(),
        uuid::Uuid::new_v4().simple().to_string(),
    ];
    let second_tokens = vec![
        uuid::Uuid::new_v4().simple().to_string(),
        uuid::Uuid::new_v4().simple().to_string(),
        uuid::Uuid::new_v4().simple().to_string(),
    ];
    let followup_token = uuid::Uuid::new_v4().simple().to_string();
    let first_paths = vec![
        "live-smoke-files/first/alpha.txt".to_string(),
        "live-smoke-files/first/beta.txt".to_string(),
        "live-smoke-files/first/gamma.txt".to_string(),
    ];
    let second_paths = vec![
        "live-smoke-files/second/alpha.txt".to_string(),
        "live-smoke-files/second/beta.txt".to_string(),
        "live-smoke-files/second/gamma.txt".to_string(),
    ];
    running_agent.write_tool_file(&first_paths[0], &first_tokens[0])?;
    running_agent.write_tool_file(&first_paths[1], &first_tokens[1])?;
    running_agent.write_tool_file(&first_paths[2], &first_tokens[2])?;
    running_agent.write_tool_file(&second_paths[0], &second_tokens[0])?;
    running_agent.write_tool_file(&second_paths[1], &second_tokens[1])?;
    running_agent.write_tool_file(&second_paths[2], &second_tokens[2])?;
    running_agent.write_tool_file("live-smoke-files/second/followup.txt", &followup_token)?;

    runtime.block_on(async {
        core.save_tool_selection(&ToolSelectionRow {
            selection_id: live_tool_selection_id.clone(),
            agent_did: Some(live_agent_did.clone()),
            display_name: Some("Live Smoke Tools".to_string()),
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
            behavior_id: live_behavior_id.clone(),
            agent_did: Some(live_agent_did.clone()),
            display_name: Some("Live Smoke Default".to_string()),
            system_prompt: Some(live_tool_prompt.to_string()),
            backend_id: Some(live_backend_id.clone()),
            model_name: Some(backend.model_name.clone()),
            tool_selection_id: Some(live_tool_selection_id.clone()),
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
        "live smoke runtime reconciled with file tools",
        Duration::from_secs(60),
        || {
            runtime.block_on(core.refresh_store()).ok()?;
            let snapshot = core.store().snapshot();
            let generation_ready = snapshot
                .latest_runtime(&live_agent_did)
                .and_then(|row| row.router_generation.or(row.active_generation))
                .is_some_and(|generation| generation > initial_generation);
            let tool_selection_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == live_tool_selection_id)
                .is_some_and(|row| {
                    row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == live_behavior_id)
                .is_some_and(|row| {
                    row.backend_id.as_deref() == Some(live_backend_id.as_str())
                        && row.model_name.as_deref() == Some(backend.model_name.as_str())
                        && row.tool_selection_id.as_deref() == Some(live_tool_selection_id.as_str())
                        && row.system_prompt.as_deref() == Some(live_tool_prompt)
                });
            (generation_ready && tool_selection_ready && behavior_ready).then_some(())
        },
    )?;

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

    let prompt = tool_loop_prompt("first", "live-smoke-files/first", &first_paths);
    let prompt_snippet = "Call read_file separately for each of these files";
    let second_prompt = tool_loop_prompt("second", "live-smoke-files/second", &second_paths);
    let followup_prompt = format!(
        "Continue this same conversation. Call read_file for live-smoke-files/second/followup.txt. Reply with the previous three tokens from this conversation, followed by the exact token from live-smoke-files/second/followup.txt, separated by single spaces."
    );

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
    let request_id = submit_chat_message_and_wait_for_request_observed(&mut driver, &prompt)?;
    wait_for_value(
        "first live request bound to first conversation",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .requests
                    .iter()
                    .find(|row| row.request_id == request_id)
                    .filter(|row| row.session_id.as_deref() == Some(first_session_id.as_str()))
                    .map(|row| row.request_id.clone())
            })
        },
    )?;

    driver.wait_for_target(
        "chat new conversation button",
        Duration::from_secs(10),
        audit::targets::CHAT_NEW_CONVERSATION,
    )?;
    driver.click_target(audit::targets::CHAT_NEW_CONVERSATION);
    let second_session_id = wait_for_value(
        "live second conversation selected",
        Duration::from_secs(10),
        || {
            driver
                .app
                .state
                .chat
                .shell
                .selected_session_id
                .clone()
                .filter(|session_id| session_id != &first_session_id)
        },
    )?;
    let second_session_target = audit::targets::chat_conversation(&second_session_id);
    driver.wait_for_target(
        "live second conversation row",
        Duration::from_secs(10),
        &second_session_target,
    )?;
    let second_request_id =
        submit_chat_message_and_wait_for_request_observed(&mut driver, &second_prompt)?;
    wait_for_value(
        "second live request bound to second conversation",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .requests
                    .iter()
                    .find(|row| row.request_id == second_request_id)
                    .filter(|row| row.session_id.as_deref() == Some(second_session_id.as_str()))
                    .map(|row| row.request_id.clone())
            })
        },
    )?;
    let desktop_client = Arc::clone(
        driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );
    wait_for_two_requests_in_flight(
        runtime.as_ref(),
        desktop_client.as_ref(),
        &request_id,
        &second_request_id,
    )?;

    let second_response_content =
        wait_for_observed_response_for_request(&mut driver, &second_request_id, &second_prompt)?;
    assert_response_contains_tokens(
        "second conversation initial response",
        &second_response_content,
        &second_tokens,
    )?;

    let first_session_target = audit::targets::chat_conversation(&first_session_id);
    driver.wait_for_target(
        "live first conversation row",
        Duration::from_secs(10),
        &first_session_target,
    )?;
    let first_conversation_texts = driver.click_target(&first_session_target);
    assert_eq!(
        driver.app.state.chat.shell.selected_session_id.as_deref(),
        Some(first_session_id.as_str())
    );
    let response_content =
        wait_for_observed_response_for_request(&mut driver, &request_id, &prompt)?;
    assert_response_contains_tokens(
        "first conversation response",
        &response_content,
        &first_tokens,
    )?;

    let chat_texts = wait_for_value(
        "live response in first transcript",
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

    let second_conversation_texts = driver.click_target(&second_session_target);
    assert_eq!(
        driver.app.state.chat.shell.selected_session_id.as_deref(),
        Some(second_session_id.as_str())
    );
    let (multi_turn_request_id, multi_turn_response_content) =
        submit_chat_message_and_wait_for_observed_response(&mut driver, &followup_prompt)?;
    assert_response_contains_tokens(
        "second conversation follow-up response",
        &multi_turn_response_content,
        &[
            second_tokens[0].clone(),
            second_tokens[1].clone(),
            second_tokens[2].clone(),
            followup_token.clone(),
        ],
    )?;
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
                .filter(|row| row.session_id.as_deref() == Some(second_session_id.as_str()))
                .count()
        })
        .ok_or_else(|| anyhow!("desktop client missing"))?;
    assert_eq!(second_session_request_count, 2);

    assert!(first_conversation_texts
        .iter()
        .any(|text| text.contains(prompt_snippet)));
    assert!(first_conversation_texts
        .iter()
        .any(|text| text.contains(response_content.trim())));

    assert!(second_conversation_texts
        .iter()
        .any(|text| text.contains(&second_prompt)));
    assert!(second_conversation_texts
        .iter()
        .any(|text| text.contains(second_response_content.trim())));
    let second_followup_texts = driver.render();
    assert!(second_followup_texts
        .iter()
        .any(|text| text.contains(&followup_prompt)));
    assert!(second_followup_texts
        .iter()
        .any(|text| text.contains(multi_turn_response_content.trim())));
    wait_for_value(
        "follow-up request persisted on second conversation",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .requests
                    .iter()
                    .find(|row| row.request_id == multi_turn_request_id)
                    .filter(|row| row.session_id.as_deref() == Some(second_session_id.as_str()))
                    .map(|row| row.request_id.clone())
            })
        },
    )?;

    let returned_chat_texts = driver.open_activity(Activity::Chat);
    assert_eq!(
        driver.app.state.chat.shell.selected_session_id.as_deref(),
        Some(second_session_id.as_str())
    );
    let returned_request_count = driver
        .app
        .client
        .as_ref()
        .map(|client| {
            client
                .store()
                .snapshot()
                .requests
                .iter()
                .filter(|row| row.session_id.as_deref() == Some(second_session_id.as_str()))
                .count()
        })
        .ok_or_else(|| anyhow!("desktop client missing"))?;
    assert_eq!(returned_request_count, 2);
    assert!(returned_chat_texts
        .iter()
        .any(|text| text.contains(second_response_content.trim())));
    assert!(returned_chat_texts
        .iter()
        .any(|text| text.contains(multi_turn_response_content.trim())));

    runtime.block_on(running_agent.shutdown())?;
    driver.app.shutdown_client();
    Ok(())
}
