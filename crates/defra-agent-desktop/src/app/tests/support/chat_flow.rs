pub(crate) fn assert_chat_context(
    driver: &AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    session_id: Option<&str>,
) {
    assert_eq!(
        driver.app.state.chat.shell.selected_peer_id.as_deref(),
        Some(deployment.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.chat.shell.selected_agent_did.as_deref(),
        Some(deployment.agent_did.as_str())
    );
    assert_eq!(
        driver.app.state.chat.shell.selected_session_id.as_deref(),
        session_id
    );
    assert_eq!(driver.app.state.chat.editor.selected_behavior_override, None);
    assert_eq!(driver.app.state.chat.editor.last_submission_error, None);
}

pub(crate) fn assert_operator_context(
    driver: &AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    section: OperatorSection,
    entity_id: Option<&str>,
) {
    assert_eq!(
        driver.app.state.operator.selected_peer_id.as_deref(),
        Some(deployment.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.operator.selected_agent_did.as_deref(),
        Some(deployment.agent_did.as_str())
    );
    assert_eq!(driver.app.state.operator.selected_section, section);
    assert_eq!(
        driver.app.state.operator.selected_entity_id.as_deref(),
        entity_id
    );
}

pub(crate) fn ensure_chat_agent_selected(
    driver: &mut AuditDriver,
    wait_label: &str,
    timeout: Duration,
) -> Result<String> {
    if let Some(agent_did) = driver.app.state.chat.shell.selected_agent_did.clone() {
        return Ok(agent_did);
    }

    if let Some(peer_status) = driver
        .app
        .client
        .as_ref()
        .and_then(|client| client.peer_statuses().into_iter().next())
    {
        let deployment_target = audit::targets::chat_deployment(&peer_status.peer_id);
        if driver
            .wait_for_target(wait_label, timeout, &deployment_target)
            .is_ok()
        {
            driver.click_target(&deployment_target);
        }
        return wait_for_value(wait_label, timeout, || {
            driver.app.state.chat.shell.selected_agent_did.clone()
        });
    }

    if let Some(agent_did) = driver.app.client.as_ref().and_then(|client| {
        client
            .store()
            .snapshot()
            .agent_principals
            .first()
            .map(|row| row.agent_did.clone())
    }) {
        driver.app.state.chat.shell.selected_agent_did = Some(agent_did.clone());
        driver.render();
        return Ok(agent_did);
    }

    anyhow::bail!("unable to select a chat agent for {wait_label}")
}

pub(crate) fn ensure_chat_session_selected(
    driver: &mut AuditDriver,
    wait_label: &str,
    timeout: Duration,
) -> Result<String> {
    const MANUAL_CREATE_SENTINEL: &str = "__manual_create__";

    let _ = ensure_chat_agent_selected(driver, wait_label, timeout)?;

    if let Some(session_id) = driver.app.state.chat.shell.selected_session_id.clone() {
        return Ok(session_id);
    }

    if let Some(existing_session_id) = driver.app.client.as_ref().and_then(|client| {
        let agent_did = driver.app.state.chat.shell.selected_agent_did.as_deref()?;
        client
            .store()
            .snapshot()
            .conversation_rows(agent_did)
            .first()
            .map(|row| row.session_id.clone())
    }) {
        let conversation_target = audit::targets::chat_conversation(&existing_session_id);
        if driver
            .wait_for_target(wait_label, timeout, &conversation_target)
            .is_ok()
        {
            driver.click_target(&conversation_target);
        }
        return wait_for_value(wait_label, timeout, || {
            driver.app.state.chat.shell.selected_session_id.clone()
        });
    }

    let outcome = wait_for_value(wait_label, timeout, || {
        let texts = driver.render();
        driver
            .app
            .state
            .chat
            .shell
            .selected_session_id
            .clone()
            .or_else(|| {
                texts
                    .iter()
                    .any(|text| text.contains("Create Conversation"))
                    .then_some(MANUAL_CREATE_SENTINEL.to_string())
            })
    })?;

    if outcome == MANUAL_CREATE_SENTINEL {
        driver.click_target(audit::targets::CHAT_CREATE_CONVERSATION);
        wait_for_value(wait_label, timeout, || {
            driver.app.state.chat.shell.selected_session_id.clone()
        })
    } else {
        Ok(outcome)
    }
}

pub(crate) fn submit_live_prompt_for_deployment(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    exact_token: &str,
) -> Result<LiveSubmissionCase> {
    let prompt = format!(
        "Reply with exactly {exact_token} and nothing else. Multi-agent server isolation audit {}",
        uuid::Uuid::new_v4()
    );
    submit_custom_live_prompt_for_deployment(driver, deployment, &prompt)
}

pub(crate) fn submit_custom_live_prompt_for_deployment(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    prompt: &str,
) -> Result<LiveSubmissionCase> {
    driver.open_activity(Activity::Chat);
    let deployment_target = audit::targets::chat_deployment(&deployment.peer_id);
    driver.wait_for_target(
        &format!("chat deployment row for {}", deployment.label),
        Duration::from_secs(10),
        &deployment_target,
    )?;
    driver.click_target(&deployment_target);
    assert_chat_context(driver, deployment, None);

    let _session_id = ensure_chat_session_selected(
        driver,
        &format!("chat session ready for {}", deployment.label),
        Duration::from_secs(10),
    )?;

    let (request_id, response) =
        submit_chat_message_and_wait_for_observed_response(driver, prompt)?;
    let session_id = driver
        .app
        .state
        .chat
        .shell
        .selected_session_id
        .clone()
        .ok_or_else(|| anyhow!("missing selected session after live submission"))?;
    assert_chat_context(driver, deployment, Some(session_id.as_str()));

    wait_for_value(
        &format!("request {request_id} bound to {}", deployment.label),
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .requests
                    .iter()
                    .find(|row| row.request_id == request_id)
                    .filter(|row| {
                        row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                            && row.session_id.as_deref() == Some(session_id.as_str())
                            && row.behavior_id.as_deref()
                                == Some(deployment.docs.behavior_id.as_str())
                    })
                    .map(|row| row.request_id.clone())
            })
        },
    )?;

    Ok(LiveSubmissionCase {
        prompt: prompt.to_string(),
        request_id,
        response,
        session_id,
    })
}
