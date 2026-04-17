pub(crate) fn open_chat_conversation_and_assert_isolation(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    submission: &LiveSubmissionCase,
    hidden_prompt: &str,
    leak_message: &str,
) -> Result<Vec<String>> {
    if driver.app.state.activity != Activity::Chat {
        driver.open_activity(Activity::Chat);
    }
    driver.click_target(&audit::targets::chat_deployment(&deployment.peer_id));
    assert_eq!(
        driver.app.state.chat.shell.selected_peer_id.as_deref(),
        Some(deployment.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.chat.shell.selected_agent_did.as_deref(),
        Some(deployment.agent_did.as_str())
    );
    assert_eq!(driver.app.state.chat.editor.selected_behavior_override, None);
    assert_eq!(driver.app.state.chat.editor.last_submission_error, None);

    let conversation_target = audit::targets::chat_conversation(&submission.session_id);
    driver.wait_for_target(
        &format!("chat conversation {}", submission.session_id),
        Duration::from_secs(10),
        &conversation_target,
    )?;
    let texts = driver.click_target(&conversation_target);
    assert_chat_context(driver, deployment, Some(submission.session_id.as_str()));
    assert!(texts
        .iter()
        .any(|text| text.contains(submission.prompt.as_str())));
    assert!(texts
        .iter()
        .any(|text| text.contains(submission.response.trim())));
    assert!(
        !texts.iter().any(|text| text.contains(hidden_prompt)),
        "{leak_message}"
    );

    Ok(texts)
}

pub(crate) fn open_operator_entity_and_assert_visibility(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    section: OperatorSection,
    entity_id: &str,
    hidden_entity_ids: &[&str],
    wait_label: &str,
) -> Result<Vec<String>> {
    if driver.app.state.activity != Activity::Operator {
        driver.open_activity(Activity::Operator);
    }
    if driver.app.state.operator.selected_peer_id.as_deref() != Some(deployment.peer_id.as_str()) {
        driver.click_target(&audit::targets::operator_deployment(&deployment.peer_id));
    }
    if driver.app.state.operator.selected_agent_did.as_deref() != Some(deployment.agent_did.as_str())
    {
        driver.click_target(&audit::targets::operator_agent(&deployment.agent_did));
    }
    assert_eq!(
        driver.app.state.operator.selected_peer_id.as_deref(),
        Some(deployment.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.operator.selected_agent_did.as_deref(),
        Some(deployment.agent_did.as_str())
    );

    let entity_target = audit::targets::operator_entity(entity_id);
    wait_for_value(wait_label, Duration::from_secs(20), || {
        let client = Arc::clone(driver.app.client.as_ref()?);
        driver.app.block_on_runtime(client.refresh_store()).ok()?;

        if driver.app.state.operator.selected_section != section
            || audit::target_rect(&driver.ctx, &entity_target).is_none()
        {
            driver.click_target(&audit::targets::operator_section(section));
        }

        let texts = driver.render();
        audit::target_rect(&driver.ctx, &entity_target).map(|_| texts)
    })?;

    for hidden_entity_id in hidden_entity_ids {
        assert!(!driver.has_target(&audit::targets::operator_entity(hidden_entity_id)));
    }
    let texts = driver.click_target(&entity_target);
    assert_operator_context(driver, deployment, section, Some(entity_id));
    Ok(texts)
}

pub(crate) fn open_operator_request_timeline_and_assert_visibility(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    request_id: &str,
    hidden_request_ids: &[&str],
    wait_label: &str,
) -> Result<Vec<String>> {
    if driver.app.state.activity != Activity::Operator {
        driver.open_activity(Activity::Operator);
    }
    if driver.app.state.operator.selected_peer_id.as_deref() != Some(deployment.peer_id.as_str()) {
        driver.click_target(&audit::targets::operator_deployment(&deployment.peer_id));
    }
    if driver.app.state.operator.selected_agent_did.as_deref() != Some(deployment.agent_did.as_str())
    {
        driver.click_target(&audit::targets::operator_agent(&deployment.agent_did));
    }

    let request_target = audit::targets::operator_entity(request_id);
    wait_for_value(wait_label, Duration::from_secs(20), || {
        let client = Arc::clone(driver.app.client.as_ref()?);
        driver.app.block_on_runtime(client.refresh_store()).ok()?;

        if driver.app.state.operator.selected_section != OperatorSection::RequestTimeline
            || audit::target_rect(&driver.ctx, &request_target).is_none()
        {
            driver.click_target(&audit::targets::operator_section(
                OperatorSection::RequestTimeline,
            ));
        }

        let texts = driver.render();
        audit::target_rect(&driver.ctx, &request_target).map(|_| texts)
    })?;

    for hidden_request_id in hidden_request_ids {
        assert!(!driver.has_target(&audit::targets::operator_entity(hidden_request_id)));
    }

    let texts = driver.click_target(&request_target);
    assert_operator_context(
        driver,
        deployment,
        OperatorSection::RequestTimeline,
        Some(request_id),
    );
    Ok(texts)
}

pub(crate) fn assert_behavior_draft_bindings(
    driver: &AuditDriver,
    behavior_id: &str,
    agent_did: &str,
    backend_id: &str,
    tool_selection_id: &str,
    inference_profile_id: &str,
) {
    match driver.app.state.operator.draft.as_ref() {
        Some(OperatorDraft::Behavior(draft)) => {
            assert_eq!(draft.behavior_id, behavior_id);
            assert_eq!(draft.agent_did, agent_did);
            assert_eq!(draft.backend_id, backend_id);
            assert_eq!(draft.tool_selection_id, tool_selection_id);
            assert_eq!(draft.inference_profile_id, inference_profile_id);
        }
        other => panic!("expected behavior draft, got {other:?}"),
    }
}

pub(crate) fn assert_backend_draft_matches(
    driver: &AuditDriver,
    backend_id: &str,
    provider_kind: &str,
    endpoint: &str,
    model_name: &str,
) {
    match driver.app.state.operator.draft.as_ref() {
        Some(OperatorDraft::Backend(draft)) => {
            assert_eq!(draft.backend_id, backend_id);
            assert_eq!(draft.provider_kind, provider_kind);
            assert_eq!(draft.endpoint, endpoint);
            assert!(draft.models.contains(model_name));
        }
        other => panic!("expected backend draft, got {other:?}"),
    }
}

pub(crate) fn assert_inference_profile_draft_matches(
    driver: &AuditDriver,
    profile_id: &str,
    max_output_tokens: &str,
    max_turns: &str,
) {
    match driver.app.state.operator.draft.as_ref() {
        Some(OperatorDraft::InferenceProfile(draft)) => {
            assert_eq!(draft.profile_id, profile_id);
            assert_eq!(draft.max_output_tokens, max_output_tokens);
            assert_eq!(draft.max_turns, max_turns);
        }
        other => panic!("expected inference profile draft, got {other:?}"),
    }
}
