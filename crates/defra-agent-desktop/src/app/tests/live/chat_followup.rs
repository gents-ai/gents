use super::*;

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_chat_disclosure_artifacts() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-disclosures", global_log_store())?;
    let agent_did = fixture
        .running_agent
        .as_ref()
        .ok_or_else(|| anyhow!("live fixture missing running agent"))?
        .did
        .clone();
    let behavior_id = fixture.docs.behavior_id.clone();
    let response_key = format!("live-response-disclosure-{}", uuid::Uuid::new_v4().simple());
    let conversation = {
        let client = Arc::clone(
            fixture
                .driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        fixture
            .runtime
            .block_on(client.create_conversation(&agent_did, Some(&behavior_id)))?
    };

    {
        let client = Arc::clone(
            fixture
                .driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        fixture.runtime.block_on(insert_chat_transcript_documents(
            client.as_ref(),
            &conversation.session_id,
            &agent_did,
            &behavior_id,
            &response_key,
        ))?;
    }

    {
        let driver = &mut fixture.driver;
        driver.app.state.activity = Activity::Chat;
        driver.wait_for_target(
            "live disclosure conversation row",
            Duration::from_secs(10),
            &audit::targets::chat_conversation(&conversation.session_id),
        )?;
        driver.click_target(&audit::targets::chat_conversation(&conversation.session_id));
        assert_eq!(
            driver.app.state.chat.shell.selected_session_id.as_deref(),
            Some(conversation.session_id.as_str())
        );

        let initial = driver.wait_for_target(
            "live reasoning disclosure row",
            Duration::from_secs(10),
            &audit::targets::chat_reasoning(&response_key),
        )?;
        assert!(initial
            .iter()
            .any(|text| text.contains("REASONING DISCLOSURE")));
        assert!(!initial
            .iter()
            .any(|text| text.contains("I verified the latest request")));

        driver.click_interactable_target(&audit::targets::chat_tool_card("call-shell-1"))?;
        let tool_texts = driver.render();
        assert!(driver
            .app
            .state
            .chat
            .editor
            .expanded_tool_cards
            .contains("call-shell-1"));
        assert!(tool_texts.iter().any(|text| text.contains("Args")));
        assert!(!tool_texts
            .iter()
            .any(|text| text.contains("src/app.rs: audit target live")));
        driver.click_interactable_target(&audit::targets::chat_tool_output("call-shell-1"))?;
        let output_texts = driver.render();
        assert!(output_texts
            .iter()
            .any(|text| text.contains("src/app.rs: audit target live")));

        driver.click_interactable_target(&audit::targets::chat_reasoning(&response_key))?;
        let reasoning_texts = driver.render();
        assert!(driver
            .app
            .state
            .chat
            .editor
            .expanded_reasoning_cards
            .contains(&format!("reasoning:{response_key}")));
        assert!(reasoning_texts
            .iter()
            .any(|text| text.contains("I verified the latest request")));
    }

    fixture.shutdown()
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_chat_retry_and_export() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-retry-export", global_log_store())?;

    {
        let driver = &mut fixture.driver;
        let session_id = ensure_chat_session_selected(
            driver,
            "live retry/export conversation selected",
            Duration::from_secs(10),
        )?;
        let prompt = format!(
            "Reply with exactly RETRY_EXPORT_READY and nothing else. audit {}",
            uuid::Uuid::new_v4()
        );
        let (first_request_id, first_response) =
            submit_chat_message_and_wait_for_observed_response(driver, &prompt)?;

        driver.click_interactable_target(audit::targets::CHAT_EXPORT)?;
        let export_payload = driver
            .app
            .state
            .chat
            .editor
            .last_export_payload
            .as_deref()
            .ok_or_else(|| anyhow!("live chat export did not capture a payload"))?;
        assert!(export_payload.contains(&session_id));
        assert!(export_payload.contains(&first_request_id));
        assert!(export_payload.contains(&prompt));
        assert!(export_payload.contains(first_response.trim()));

        let prior_request_count = driver
            .app
            .client
            .as_ref()
            .map(|client| client.store().snapshot().requests.len())
            .ok_or_else(|| anyhow!("desktop client missing"))?;
        driver.click_interactable_target(audit::targets::CHAT_RETRY)?;
        let retry_request_id =
            wait_for_value("live retry request row", Duration::from_secs(10), || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .requests
                        .iter()
                        .filter(|row| row.session_id.as_deref() == Some(session_id.as_str()))
                        .find(|row| {
                            row.retry_parent_request.as_deref() == Some(first_request_id.as_str())
                                && row.retry_root_request.as_deref()
                                    == Some(first_request_id.as_str())
                                && row.retry_count == Some(1)
                                && row.content.as_deref() == Some(prompt.as_str())
                        })
                        .map(|row| row.request_id.clone())
                        .filter(|_| client.store().snapshot().requests.len() > prior_request_count)
                })
            })?;
        let retry_response =
            wait_for_value("live retry response row", Duration::from_secs(90), || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .latest_response_for_request(&retry_request_id)
                        .and_then(|row| row.content.clone())
                        .filter(|content| !content.trim().is_empty())
                })
            })?;
        wait_for_value("live retry transcript row", Duration::from_secs(30), || {
            let texts = driver.render();
            texts
                .iter()
                .any(|text| text.contains(retry_response.trim()))
                .then_some(())
        })?;
        assert_eq!(driver.app.state.chat.editor.last_submission_error, None);
        assert_eq!(
            driver.app.state.chat.editor.last_action_message.as_deref(),
            Some("Retried latest request.")
        );
    }

    fixture.shutdown()
}
