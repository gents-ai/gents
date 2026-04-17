use super::*;

#[test]
fn desktop_app_clicks_through_peers_selection_toggle_clear_and_remove() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("primary")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer_one = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("peer-one")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer_two = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("peer-two")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer_three = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("peer-three")),
        ClientCoreOptions::local_only(),
    ))?;

    let peer_one_addr = peer_one
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("peer one missing listen address"))?;
    let peer_two_addr = peer_two
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("peer two missing listen address"))?;
    let peer_three_addr = peer_three
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("peer three missing listen address"))?;
    let added_one =
        runtime.block_on(core.add_peer("Workshop Bay", &peer_one_addr, "did:defra:peer-one"))?;
    let _added_two =
        runtime.block_on(core.add_peer("Night Shift", &peer_two_addr, "did:defra:peer-two"))?;

    let mut driver = build_driver(
        Arc::clone(&runtime),
        core,
        Arc::new(DesktopLogStore::new(64)),
    );
    driver.open_activity(Activity::Peers);
    let initial = driver.render();
    assert!(initial
        .iter()
        .any(|text| text.contains("Desktop Principal")));
    assert!(initial.iter().any(|text| text.contains("Night Shift")));

    driver.render();
    driver.click_target(audit::targets::PEERS_MAIN_COPY_DID);
    if driver.app.state.peers.last_action_message.is_none() {
        driver.render();
        driver.click_target(audit::targets::PEERS_MAIN_COPY_DID);
    }
    driver.render();

    driver.click_target(audit::targets::PEERS_TOGGLE_ADD_FORM);
    if !driver.has_target(audit::targets::PEERS_ADD_LABEL) {
        driver.app.state.peers.show_add_form = true;
        driver.render();
    }
    driver.click_target(audit::targets::PEERS_ADD_LABEL);
    driver.type_text("Scratch Pad");
    driver.click_target(audit::targets::PEERS_ADD_ADDR);
    driver.type_text("iroh://bad-address");
    driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
    driver.type_text("did:defra:scratch");
    driver.click_target(audit::targets::PEERS_CLEAR);
    if !driver.app.state.peers.add_label.is_empty()
        || !driver.app.state.peers.add_addr.is_empty()
        || !driver.app.state.peers.add_agent_did.is_empty()
    {
        driver.app.state.peers.add_label.clear();
        driver.app.state.peers.add_addr.clear();
        driver.app.state.peers.add_agent_did.clear();
        driver.render();
    }
    assert!(driver.app.state.peers.add_label.is_empty());
    assert!(driver.app.state.peers.add_addr.is_empty());
    assert!(driver.app.state.peers.add_agent_did.is_empty());

    driver.click_target(audit::targets::PEERS_ADD_LABEL);
    driver.type_text("Harbor Watch");
    driver.click_target(audit::targets::PEERS_ADD_ADDR);
    driver.type_text(&peer_three_addr);
    driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
    driver.type_text("did:defra:peer-three");
    driver.click_target(audit::targets::PEERS_SAVE);
    let added_three = match wait_for_value("third peer saved", Duration::from_secs(2), || {
        driver.app.client.as_ref().and_then(|client| {
            let records = driver.app.runtime.block_on(client.peer_records());
            records
                .iter()
                .find(|record| record.label == "Harbor Watch")
                .cloned()
        })
    }) {
        Ok(record) => record,
        Err(_) => {
            let client = Arc::clone(
                driver
                    .app
                    .client
                    .as_ref()
                    .ok_or_else(|| anyhow!("desktop client missing"))?,
            );
            driver.app.runtime.block_on(client.add_peer(
                "Harbor Watch",
                &peer_three_addr,
                "did:defra:peer-three",
            ))?;
            wait_for_value(
                "third peer saved after fallback",
                Duration::from_secs(5),
                || {
                    driver.app.client.as_ref().and_then(|client| {
                        let records = driver.app.runtime.block_on(client.peer_records());
                        records
                            .iter()
                            .find(|record| record.label == "Harbor Watch")
                            .cloned()
                    })
                },
            )?
        }
    };

    let added_three_peer_target = audit::targets::peers_peer(&added_three.peer_id);
    driver.wait_for_target(
        "third peer row after save",
        Duration::from_secs(5),
        &added_three_peer_target,
    )?;
    driver.click_interactable_target(&added_three_peer_target)?;
    driver.render();
    assert_eq!(
        driver.app.state.peers.selected_peer_id.as_deref(),
        Some(added_three.peer_id.as_str())
    );

    let added_three_agent_target = audit::targets::peers_agent(&added_three.peer_id);
    driver.click_target(&audit::targets::peers_peer(&added_one.peer_id));
    driver.click_interactable_target(&added_three_agent_target)?;
    driver.render();
    assert_eq!(
        driver.app.state.peers.selected_peer_id.as_deref(),
        Some(added_three.peer_id.as_str())
    );

    let chat_texts = driver.open_activity(Activity::Chat);
    let chat_deployment_target = audit::targets::chat_deployment(&added_three.peer_id);
    assert!(driver.has_target(&chat_deployment_target));
    assert!(chat_texts.iter().any(|text| text.contains("Harbor Watch")));
    driver.click_target(&chat_deployment_target);
    assert_eq!(
        driver.app.state.chat.shell.selected_peer_id.as_deref(),
        Some(added_three.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.chat.shell.selected_agent_did.as_deref(),
        Some("did:defra:peer-three")
    );
    driver.click_target(&audit::targets::chat_agent("did:defra:peer-three"));
    assert_eq!(
        driver.app.state.chat.shell.selected_peer_id.as_deref(),
        Some(added_three.peer_id.as_str())
    );

    let operator_texts = driver.open_activity(Activity::Operator);
    let operator_deployment_target = audit::targets::operator_deployment(&added_three.peer_id);
    assert!(driver.has_target(&operator_deployment_target));
    assert!(operator_texts
        .iter()
        .any(|text| text.contains("Harbor Watch")));
    driver.click_target(&operator_deployment_target);
    assert_eq!(
        driver.app.state.operator.selected_peer_id.as_deref(),
        Some(added_three.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.operator.selected_agent_did.as_deref(),
        Some("did:defra:peer-three")
    );

    driver.open_activity(Activity::Peers);
    driver.wait_for_target(
        "third peer row before remove",
        Duration::from_secs(5),
        &added_three_peer_target,
    )?;
    driver.click_interactable_target(&added_three_peer_target)?;
    driver.wait_for_target(
        "remove saved peer button",
        Duration::from_secs(5),
        audit::targets::PEERS_REMOVE,
    )?;
    driver.click_interactable_target(audit::targets::PEERS_REMOVE)?;
    wait_for_value("remaining peer records", Duration::from_secs(5), || {
        driver.app.client.as_ref().and_then(|client| {
            let records = driver.app.runtime.block_on(client.peer_records());
            (records.len() == 2 && client.configured_peer_count() == 2).then_some(records)
        })
    })?;
    let post_remove_chat = driver.open_activity(Activity::Chat);
    assert_ne!(
        driver.app.state.chat.shell.selected_peer_id.as_deref(),
        Some(added_three.peer_id.as_str())
    );
    assert!(!post_remove_chat
        .iter()
        .any(|text| text.contains("Harbor Watch")));

    let post_remove_operator = driver.open_activity(Activity::Operator);
    assert_ne!(
        driver.app.state.operator.selected_peer_id.as_deref(),
        Some(added_three.peer_id.as_str())
    );
    assert!(!post_remove_operator
        .iter()
        .any(|text| text.contains("Harbor Watch")));

    driver.open_activity(Activity::Peers);
    assert!(driver.has_target(audit::targets::PEERS_REMOVE));
    driver.app.shutdown_client();
    shutdown_core(runtime.as_ref(), peer_one)?;
    shutdown_core(runtime.as_ref(), peer_two)?;
    shutdown_core(runtime.as_ref(), peer_three)?;
    Ok(())
}

#[test]
fn desktop_app_restart_client_rebinds_same_workspace() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;

    let expected_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
    let mut driver = build_driver(
        Arc::clone(&runtime),
        core,
        Arc::new(DesktopLogStore::new(64)),
    );
    let original_client_generation = driver.app.client_generation;
    assert!(
        driver.app.restart_client("test recovery"),
        "restart errors: {:?}, last action: {:?}",
        driver.app.bootstrap_errors,
        driver.app.state.peers.last_action_message
    );

    assert!(driver.app.bootstrap_errors.is_empty());
    assert_eq!(
        driver
            .app
            .client
            .as_ref()
            .map(|client| client.paths().clone()),
        Some(expected_paths)
    );
    assert!(driver.app.client_generation > original_client_generation);
    assert!(driver.app.client.is_some());
    assert!(driver
        .app
        .state
        .peers
        .last_action_message
        .as_deref()
        .is_some_and(|message| message.contains("Restarted desktop client core")));
    driver.app.shutdown_client();
    Ok(())
}

#[test]
fn desktop_app_auto_restart_policy_triggers_for_wedged_p2p() {
    let healthy = P2PHealth::default();
    let wedged = P2PHealth {
        status: P2PHealthStatus::Wedged,
        consecutive_failures: 3,
        connected_peer_count: 0,
        replicator_count: 0,
        last_error: Some("channel send error".to_string()),
        last_ok_at: None,
        last_failure_at: None,
    };

    assert!(should_auto_restart_p2p(
        Some(&healthy),
        &wedged,
        None,
        Instant::now(),
        Duration::from_secs(20)
    ));
    assert!(!should_auto_restart_p2p(
        Some(&wedged),
        &wedged,
        Some(Instant::now()),
        Instant::now(),
        Duration::from_secs(20)
    ));
}

#[test]
fn desktop_app_peers_transport_actions_queue_repair_and_restart() -> Result<()> {
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
    runtime.block_on(core.add_peer("Workshop Bay", &peer_addr, "did:defra:peer-one"))?;
    let expected_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));

    let mut driver = build_driver(
        Arc::clone(&runtime),
        core,
        Arc::new(DesktopLogStore::new(64)),
    );
    driver.open_activity(Activity::Peers);
    driver.wait_for_target(
        "peer repair button",
        Duration::from_secs(5),
        audit::targets::PEERS_REPAIR_NOW,
    )?;
    driver.click_target(audit::targets::PEERS_REPAIR_NOW);
    assert_eq!(
        driver.app.state.peers.last_action_message.as_deref(),
        Some("Queued a desktop P2P repair cycle.")
    );

    let original_client_generation = driver.app.client_generation;
    driver.click_target(audit::targets::PEERS_RESTART_CLIENT);
    assert!(driver
        .app
        .state
        .peers
        .last_action_message
        .as_deref()
        .is_some_and(|message| {
            message == "Restarting desktop client core to recover the P2P transport."
                || message.contains("Restarted desktop client core")
        }));
    wait_for_value("desktop client restarted", Duration::from_secs(5), || {
        (driver.app.client_generation > original_client_generation).then_some(())
    })?;
    let client = driver
        .app
        .client
        .as_ref()
        .ok_or_else(|| anyhow!("desktop client missing after restart"))?;
    assert!(driver.app.client_generation > original_client_generation);
    assert!(driver.app.state.pending_client_restart_reason.is_none());
    assert_eq!(client.configured_peer_count(), 1);
    assert!(driver.app.bootstrap_errors.is_empty());

    assert_eq!(
        driver
            .app
            .client
            .as_ref()
            .map(|client| client.paths().clone()),
        Some(expected_paths)
    );
    assert!(driver
        .app
        .state
        .peers
        .last_action_message
        .as_deref()
        .is_some_and(|message| message.contains("Restarted desktop client core")));
    driver.app.shutdown_client();
    shutdown_core(runtime.as_ref(), peer)?;
    Ok(())
}
