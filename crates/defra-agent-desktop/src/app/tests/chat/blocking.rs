use super::*;

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
