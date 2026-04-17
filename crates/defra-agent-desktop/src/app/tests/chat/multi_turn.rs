use super::*;

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

    let mut driver = build_chat_driver(Arc::clone(&runtime), core);
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
