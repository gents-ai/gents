use super::*;

#[test]
fn desktop_app_redirects_blank_first_launch_to_setup_onboarding() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
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
    assert!(app.state.onboarding.first_launch_redirect_done);
    assert!(app.state.setup.workspace_open);
    assert!(app.state.setup.show_add_form);
    assert!(texts.iter().any(|text| text.contains("First Launch")));
    assert!(texts.iter().any(|text| text.contains("Add Deployment")));
    Ok(())
}

#[test]
fn desktop_app_clicks_through_first_launch_add_peer_flow() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("primary")),
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

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    let mut driver = AuditDriver::new(app, ctx);

    let initial = driver.render();
    assert!(initial.iter().any(|text| text == "First Launch"));
    assert_eq!(driver.app.state.activity, Activity::Chat);
    assert!(driver.app.state.setup.workspace_open);
    assert!(driver.app.state.setup.show_add_form);

    driver.click_target(audit::targets::SETUP_ONBOARDING_COPY_DID);
    assert_eq!(
        driver.app.state.setup.last_action_message.as_deref(),
        Some("Copied desktop DID to clipboard.")
    );

    driver.click_target(audit::targets::SETUP_ADD_LABEL);
    driver.type_text("Workshop Bay");
    driver.click_target(audit::targets::SETUP_ADD_ADDR);
    driver.type_text(&peer_addr);
    driver.click_target(audit::targets::SETUP_ADD_AGENT_DID);
    driver.type_text("did:defra:peer");
    let texts = driver.click_target(audit::targets::SETUP_SAVE);

    assert!(driver.app.state.setup.selected_peer_id.is_some());
    assert_eq!(driver.app.state.activity, Activity::Chat);
    assert!(texts.iter().any(|text| text.contains("Workshop Bay")));
    assert!(texts.iter().any(|text| text.contains("Conversation")));
    driver.app.shutdown_client();
    shutdown_core(runtime.as_ref(), peer)?;
    Ok(())
}

#[test]
fn desktop_app_clicks_through_first_launch_add_peer_with_dial_warning() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    ))?;

    let ctx = egui::Context::default();
    let cc = eframe::CreationContext::_new_kittest(ctx.clone());
    let app = DesktopApp::from_parts(
        &cc,
        Arc::clone(&runtime),
        Some(Arc::new(core)),
        Vec::new(),
        Arc::new(DesktopLogStore::new(64)),
    );
    let mut driver = AuditDriver::new(app, ctx);

    let initial = driver.render();
    assert!(initial.iter().any(|text| text.contains("First Launch")));

    driver.click_target(audit::targets::SETUP_ADD_LABEL);
    driver.type_text("Broken Relay");
    driver.click_target(audit::targets::SETUP_ADD_ADDR);
    driver.type_text("iroh://bad-address");
    driver.click_target(audit::targets::SETUP_ADD_AGENT_DID);
    driver.type_text("did:defra:broken");
    driver.click_target(audit::targets::SETUP_SAVE);

    let warning_message: String = wait_for_value(
        "deployment save warning after invalid address",
        Duration::from_secs(5),
        || {
            driver
                .app
                .state
                .setup
                .last_action_message
                .as_ref()
                .filter(|message| message.contains("dial failed"))
                .cloned()
        },
    )?;
    assert!(warning_message.contains("Saved Broken Relay."));

    wait_for_value(
        "saved deployment appears after dial warning",
        Duration::from_secs(5),
        || {
            driver.app.client.as_ref().and_then(|client| {
                let records = driver.app.runtime.block_on(client.peer_records());
                records
                    .iter()
                    .find(|record| record.label == "Broken Relay")
                    .map(|record| record.peer_id.clone())
            })
        },
    )?;

    let chat_texts = driver.open_activity(Activity::Chat);
    assert!(chat_texts.iter().any(|text| text.contains("Broken Relay")));

    driver.app.shutdown_client();
    Ok(())
}

#[test]
fn desktop_app_clicks_chat_open_setup_from_empty_sidebar() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
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
    app.state.onboarding.first_launch_redirect_done = true;
    app.state.activity = Activity::Chat;
    app.state.setup.workspace_open = false;
    app.state.setup.show_add_form = false;
    let mut driver = AuditDriver::new(app, ctx);

    let texts = driver.render();
    assert!(texts.iter().any(|text| text.contains("Add Deployment")));

    let after_click = driver.click_target(audit::targets::CHAT_OPEN_SETUP);
    assert_eq!(driver.app.state.activity, Activity::Chat);
    assert!(driver.app.state.setup.workspace_open);
    assert!(driver.app.state.setup.show_add_form);
    assert!(after_click.iter().any(|text| text.contains("First Launch")));
    Ok(())
}

#[test]
fn desktop_app_renders_broken_peer_warning_from_live_status() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());
    seed_saved_peer_directory(
        &paths,
        "Broken Relay",
        "iroh://bad-address",
        "did:defra:broken",
    )?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        paths,
        ClientCoreOptions::local_only(),
    ))?;

    assert!(core.bootstrap_errors().is_empty());
    assert_eq!(core.peer_issue_count(), 1);

    let mut driver = build_driver(
        Arc::clone(&runtime),
        core,
        Arc::new(DesktopLogStore::new(64)),
    );

    wait_for_value(
        "deployment warning rendered from live status",
        Duration::from_secs(2),
        || {
            let _ = driver.open_activity(Activity::Chat);
            driver.click_target(audit::targets::CHAT_OPEN_SETUP);
            driver.click_target(audit::targets::SETUP_BACK_TO_DEPLOYMENTS);
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("Broken Relay"))
                .then_some(texts)
        },
    )?;
    let setup_texts = driver.render();
    assert!(setup_texts
        .iter()
        .any(|text| text.contains("peer Broken Relay dial failed")));

    driver.app.shutdown_client();
    Ok(())
}
