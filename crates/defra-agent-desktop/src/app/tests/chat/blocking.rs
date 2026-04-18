use super::*;

#[test]
fn desktop_app_blocks_chat_send_while_turn_is_waiting_for_claim() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;
    let local_addr = core
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("desktop core missing listen address"))?;
    let saved_peer =
        runtime.block_on(core.add_peer("Amy Deployment", &local_addr, "did:defra:amy"))?;
    runtime.block_on(seed_manage_documents(&core))?;
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
    let mut driver = AuditDriver::new(app, ctx);
    driver.render();
    driver.click_target(&audit::targets::chat_deployment(&saved_peer.peer_id));
    driver.click_target(&audit::targets::chat_conversation(&created.session_id));

    let waiting_texts = wait_for_value(
        "waiting-for-claim turn state",
        Duration::from_secs(5),
        || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains("turn waiting..."))
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
