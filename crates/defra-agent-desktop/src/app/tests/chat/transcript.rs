use super::*;

#[test]
fn desktop_app_clicks_through_chat_reasoning_and_tool_card_disclosures() -> Result<()> {
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
        .ok_or_else(|| anyhow!("peer missing listen address"))?;
    let saved_peer =
        runtime.block_on(core.add_peer("Amy Deployment", &peer_addr, "did:defra:amy"))?;
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

    let mut driver = build_chat_driver(Arc::clone(&runtime), core);
    driver.render();
    driver.click_target(&audit::targets::chat_deployment(&saved_peer.peer_id));
    driver.click_target(&audit::targets::chat_conversation(&conversation.session_id));
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
    driver.app.shutdown_client();
    shutdown_core(runtime.as_ref(), peer)?;
    Ok(())
}
