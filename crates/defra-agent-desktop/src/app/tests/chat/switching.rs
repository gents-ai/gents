use super::*;

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

    let mut driver = build_chat_driver(Arc::clone(&runtime), core);
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
