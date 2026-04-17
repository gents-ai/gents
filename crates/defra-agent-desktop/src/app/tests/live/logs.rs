use super::*;

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_logs_event_classification() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-logs", global_log_store())?;
    let runtime = Arc::clone(&fixture.runtime);
    let agent_did = fixture
        .running_agent
        .as_ref()
        .ok_or_else(|| anyhow!("live fixture missing running agent"))?
        .did
        .clone();
    let peer = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(fixture._tempdir.path().join("peer")),
        ClientCoreOptions::local_only(),
    ))?;
    let peer_addr = peer
        .listen_addresses()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("live logs peer missing listen address"))?;
    let baseline_events = global_log_store().snapshot().total_events;

    {
        let driver = &mut fixture.driver;
        let _session_id =
            ensure_chat_session_selected(driver, "live logs chat ready", Duration::from_secs(10))?;
        let (request_id, response_text) = submit_chat_message_and_wait_for_observed_response(
            driver,
            "Reply with exactly LOG_READY for the logs audit",
        )?;
        assert!(!response_text.trim().is_empty());

        driver.open_activity(Activity::Peers);
        driver.wait_for_target(
            "live logs peer add form",
            Duration::from_secs(10),
            audit::targets::PEERS_ADD_LABEL,
        )?;
        driver.click_target(audit::targets::PEERS_ADD_LABEL);
        driver.type_text("Live Logs Peer");
        driver.click_target(audit::targets::PEERS_ADD_ADDR);
        driver.type_text(&peer_addr);
        driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
        driver.type_text("did:defra:live-logs-peer");
        driver.click_target(audit::targets::PEERS_SAVE);
        let live_logs_peer_id =
            wait_for_value("live logs peer connected", Duration::from_secs(10), || {
                driver.app.client.as_ref().and_then(|client| {
                    let records = driver.app.runtime.block_on(client.peer_records());
                    records
                        .iter()
                        .find(|record| record.label == "Live Logs Peer")
                        .filter(|_| client.configured_peer_count() >= 1)
                        .map(|record| record.peer_id.clone())
                })
            })?;

        driver.open_activity(Activity::Chat);
        let live_logs_chat_deployment = audit::targets::chat_deployment(&live_logs_peer_id);
        driver.wait_for_target(
            "live logs chat deployment row",
            Duration::from_secs(10),
            &live_logs_chat_deployment,
        )?;
        driver.click_target(&live_logs_chat_deployment);
        assert_eq!(
            driver.app.state.chat.shell.selected_peer_id.as_deref(),
            Some(live_logs_peer_id.as_str())
        );
        assert_eq!(
            driver.app.state.chat.shell.selected_agent_did.as_deref(),
            Some("did:defra:live-logs-peer")
        );

        driver.open_activity(Activity::Peers);
        driver.render();
        if driver.has_target(audit::targets::PEERS_TOGGLE_ADD_FORM) {
            driver.click_target(audit::targets::PEERS_TOGGLE_ADD_FORM);
        }
        if !driver.has_target(audit::targets::PEERS_ADD_LABEL) {
            driver.app.state.peers.show_add_form = true;
            driver.render();
        }
        driver.click_target(audit::targets::PEERS_ADD_LABEL);
        driver.type_text("Broken Logs Peer");
        driver.click_target(audit::targets::PEERS_ADD_ADDR);
        driver.type_text("iroh://bad-address");
        driver.click_target(audit::targets::PEERS_ADD_AGENT_DID);
        driver.type_text("did:defra:broken-live-logs-peer");
        driver.click_target(audit::targets::PEERS_SAVE);
        if wait_for_value(
            "live logs warning from ui peer add",
            Duration::from_secs(2),
            || {
                global_log_store()
                    .snapshot()
                    .entries
                    .iter()
                    .any(|entry| entry.message.contains("desktop peer add warning"))
                    .then_some(())
            },
        )
        .is_err()
        {
            let client = Arc::clone(
                driver
                    .app
                    .client
                    .as_ref()
                    .ok_or_else(|| anyhow!("desktop client missing"))?,
            );
            let _ = driver.app.runtime.block_on(client.add_peer(
                "Broken Logs Peer",
                "iroh://bad-address",
                "did:defra:broken-live-logs-peer",
            ));
        }
        wait_for_value(
            "live logs warning captured",
            Duration::from_secs(10),
            || {
                let snapshot = global_log_store().snapshot();
                (snapshot.total_events > baseline_events
                    && snapshot
                        .entries
                        .iter()
                        .any(|entry| entry.message.contains("desktop peer add warning")))
                .then_some(())
            },
        )?;

        driver.open_activity(Activity::Operator);
        let live_logs_operator_deployment = audit::targets::operator_deployment(&live_logs_peer_id);
        driver.wait_for_target(
            "live logs operator deployment row",
            Duration::from_secs(10),
            &live_logs_operator_deployment,
        )?;
        driver.click_target(&live_logs_operator_deployment);
        assert_eq!(
            driver.app.state.operator.selected_peer_id.as_deref(),
            Some(live_logs_peer_id.as_str())
        );
        assert_eq!(
            driver.app.state.operator.selected_agent_did.as_deref(),
            Some("did:defra:live-logs-peer")
        );
        let live_logs_operator_agent = audit::targets::operator_agent("did:defra:live-logs-peer");
        driver.wait_for_target(
            "live logs operator agent row",
            Duration::from_secs(10),
            &live_logs_operator_agent,
        )?;
        driver.click_target(&live_logs_operator_agent);
        assert_eq!(
            driver.app.state.operator.selected_agent_did.as_deref(),
            Some("did:defra:live-logs-peer")
        );
        driver.app.state.operator.selected_agent_did = Some(agent_did.clone());
        driver.app.state.operator.selected_peer_id = None;
        driver.app.state.operator.selected_entity_id = None;
        driver.app.state.operator.draft = None;
        driver.app.state.operator.draft_source_entity_id = None;
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Behaviors,
        ));
        driver.wait_for_target(
            "live logs behavior row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&fixture.docs.behavior_id),
        )?;
        driver.click_target(&audit::targets::operator_entity(&fixture.docs.behavior_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Live Logs Behavior Review",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value("live logs write captured", Duration::from_secs(10), || {
            let snapshot = global_log_store().snapshot();
            snapshot
                .entries
                .iter()
                .any(|entry| {
                    entry.category == DesktopLogCategory::Writes
                        && entry.message.contains("desktop write saved")
                        && entry
                            .fields
                            .iter()
                            .any(|field| field.value == fixture.docs.behavior_id)
                })
                .then_some(())
        })?;

        let all_texts = driver.open_activity(Activity::Logs);
        assert!(all_texts.iter().any(|text| text.contains("Live Logs")));
        let log_snapshot = global_log_store().snapshot();
        assert!(log_snapshot
            .entries
            .iter()
            .any(|entry| entry.message.contains("desktop replica snapshot refreshed")));
        assert!(log_snapshot
            .entries
            .iter()
            .any(|entry| entry.message.contains("desktop write saved")));
        assert!(log_snapshot
            .entries
            .iter()
            .any(|entry| entry.message.contains("desktop peer added")));
        assert!(log_snapshot
            .entries
            .iter()
            .any(|entry| entry.message.contains("desktop peer add warning")));
        assert!(log_snapshot.entries.iter().any(|entry| {
            entry.category == DesktopLogCategory::Turns
                && (entry.message.contains(&request_id)
                    || entry
                        .fields
                        .iter()
                        .any(|field| field.value.contains(&request_id)))
        }));

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Turns,
        )));
        let turns_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Turns)
        );
        assert_logs_filter_has_results(&turns_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Writes,
        )));
        let writes_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Writes)
        );
        assert_logs_filter_has_results(&writes_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Peering,
        )));
        let peering_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Peering)
        );
        assert_logs_filter_has_results(&peering_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Warnings,
        )));
        let warning_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Warnings)
        );
        assert_logs_filter_has_results(&warning_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::Category(
            DesktopLogCategory::Replication,
        )));
        let replication_texts = driver.render();
        assert_eq!(
            driver.app.state.logs.filter,
            LogsFilter::Category(DesktopLogCategory::Replication)
        );
        assert_logs_filter_has_results(&replication_texts);

        driver.click_target(audit::targets::logs_filter(LogsFilter::All));
        let all_filter_texts = driver.render();
        assert_eq!(driver.app.state.logs.filter, LogsFilter::All);
        assert_logs_filter_has_results(&all_filter_texts);
    }

    fixture.shutdown()?;
    shutdown_core(runtime.as_ref(), peer)?;
    Ok(())
}
