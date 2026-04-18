use super::*;

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
    let local_addr = core
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("desktop core missing listen address"))?;
    let saved_peer =
        runtime.block_on(core.add_peer("Tool Loop Deployment", &local_addr, &agent_did))?;

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

    let mut driver = build_chat_driver(Arc::clone(&runtime), core);
    driver.render();
    driver.click_target(&audit::targets::chat_deployment(&saved_peer.peer_id));
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
