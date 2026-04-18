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

pub(crate) fn open_manage_entity_and_assert_visibility(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    section: ManageSection,
    entity_id: &str,
    hidden_entity_ids: &[&str],
    wait_label: &str,
) -> Result<Vec<String>> {
    if driver.app.state.activity != Activity::Manage {
        driver.open_activity(Activity::Manage);
    }
    if driver.app.state.manage.selected_peer_id.as_deref() != Some(deployment.peer_id.as_str()) {
        driver.click_target(&audit::targets::manage_deployment(&deployment.peer_id));
    }
    if driver.app.state.manage.selected_agent_did.as_deref() != Some(deployment.agent_did.as_str())
    {
        driver.click_target(&audit::targets::manage_agent(&deployment.agent_did));
    }
    assert_eq!(
        driver.app.state.manage.selected_peer_id.as_deref(),
        Some(deployment.peer_id.as_str())
    );
    assert_eq!(
        driver.app.state.manage.selected_agent_did.as_deref(),
        Some(deployment.agent_did.as_str())
    );

    let entity_target = audit::targets::manage_entity(entity_id);
    wait_for_value(wait_label, Duration::from_secs(20), || {
        let client = Arc::clone(driver.app.client.as_ref()?);
        driver.app.block_on_runtime(client.refresh_store()).ok()?;

        if driver.app.state.manage.selected_section != section {
            driver.click_target(&audit::targets::manage_section(section));
        }
        let mut texts = driver.render();
        if audit::target_interact_rect(&driver.ctx, &entity_target).is_none() {
            driver.replace_text_in_target(audit::targets::MANAGE_ENTITY_FILTER, entity_id);
            texts = driver.render();
        }
        audit::target_interact_rect(&driver.ctx, &entity_target).map(|_| texts)
    })?;

    for hidden_entity_id in hidden_entity_ids {
        assert!(!driver.has_target(&audit::targets::manage_entity(hidden_entity_id)));
    }
    let texts = driver.click_target(&entity_target);
    assert_manage_context(driver, deployment, section, Some(entity_id));
    Ok(texts)
}

pub(crate) fn open_manage_request_timeline_and_assert_visibility(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    request_id: &str,
    hidden_request_ids: &[&str],
    wait_label: &str,
) -> Result<Vec<String>> {
    if driver.app.state.activity != Activity::Manage {
        driver.open_activity(Activity::Manage);
    }
    if driver.app.state.manage.selected_peer_id.as_deref() != Some(deployment.peer_id.as_str()) {
        driver.click_target(&audit::targets::manage_deployment(&deployment.peer_id));
    }
    if driver.app.state.manage.selected_agent_did.as_deref() != Some(deployment.agent_did.as_str())
    {
        driver.click_target(&audit::targets::manage_agent(&deployment.agent_did));
    }

    let request_target = audit::targets::manage_entity(request_id);
    wait_for_value(wait_label, Duration::from_secs(20), || {
        let client = Arc::clone(driver.app.client.as_ref()?);
        driver.app.block_on_runtime(client.refresh_store()).ok()?;

        if driver.app.state.manage.selected_section != ManageSection::RequestTimeline {
            driver.click_target(&audit::targets::manage_section(
                ManageSection::RequestTimeline,
            ));
        }
        let mut texts = driver.render();
        if audit::target_interact_rect(&driver.ctx, &request_target).is_none() {
            driver.replace_text_in_target(audit::targets::MANAGE_ENTITY_FILTER, request_id);
            texts = driver.render();
        }
        audit::target_interact_rect(&driver.ctx, &request_target).map(|_| texts)
    })?;

    for hidden_request_id in hidden_request_ids {
        assert!(!driver.has_target(&audit::targets::manage_entity(hidden_request_id)));
    }

    let texts = driver.click_target(&request_target);
    assert_manage_context(
        driver,
        deployment,
        ManageSection::RequestTimeline,
        Some(request_id),
    );
    Ok(texts)
}

pub(crate) struct ManageBackendForm<'a> {
    pub(crate) backend_id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) provider_kind: &'a str,
    pub(crate) endpoint: &'a str,
    pub(crate) api_key: &'a str,
    pub(crate) api_key_env_var: &'a str,
    pub(crate) max_concurrent: &'a str,
    pub(crate) max_queue_depth: &'a str,
    pub(crate) enabled: bool,
    pub(crate) models: &'a str,
    pub(crate) probe_status: &'a str,
}

pub(crate) struct ManageBehaviorForm<'a> {
    pub(crate) behavior_id: &'a str,
    pub(crate) agent_did: &'a str,
    pub(crate) display_name: &'a str,
    pub(crate) system_prompt: &'a str,
    pub(crate) backend_id: &'a str,
    pub(crate) model_name: &'a str,
    pub(crate) tool_selection_id: &'a str,
    pub(crate) inference_profile_id: &'a str,
    pub(crate) compaction_strategy: &'a str,
    pub(crate) compaction_threshold: &'a str,
    pub(crate) enabled: bool,
}

pub(crate) struct ManageInferenceProfileForm<'a> {
    pub(crate) profile_id: &'a str,
    pub(crate) display_name: &'a str,
    pub(crate) context_window: &'a str,
    pub(crate) max_output_tokens: &'a str,
    pub(crate) max_turns: &'a str,
    pub(crate) temperature: &'a str,
    pub(crate) stream_batch_ms: &'a str,
    pub(crate) deadline_duration_secs: &'a str,
}

pub(crate) fn fill_behavior_draft(driver: &mut AuditDriver, behavior: &ManageBehaviorForm<'_>) {
    driver.replace_text_in_target(
        &audit::targets::manage_field("Behavior ID"),
        behavior.behavior_id,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Agent DID"),
        behavior.agent_did,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Display Name"),
        behavior.display_name,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("System Prompt"),
        behavior.system_prompt,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Backend ID"),
        behavior.backend_id,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Model Name"),
        behavior.model_name,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Tool Selection ID"),
        behavior.tool_selection_id,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Inference Profile ID"),
        behavior.inference_profile_id,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Compaction Strategy"),
        behavior.compaction_strategy,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Compaction Threshold"),
        behavior.compaction_threshold,
    );
    let current_enabled = matches!(
        driver.app.state.manage.draft.as_ref(),
        Some(ManageDraft::Behavior(draft)) if draft.enabled
    );
    if current_enabled != behavior.enabled {
        driver.click_target(&audit::targets::manage_toggle("Enabled"));
    }
}

pub(crate) fn fill_backend_draft(driver: &mut AuditDriver, backend: &ManageBackendForm<'_>) {
    driver.replace_text_in_target(
        &audit::targets::manage_field("Backend ID"),
        backend.backend_id,
    );
    driver.replace_text_in_target(&audit::targets::manage_field("Name"), backend.name);
    driver.replace_text_in_target(
        &audit::targets::manage_field("Provider Kind"),
        backend.provider_kind,
    );
    driver.replace_text_in_target(&audit::targets::manage_field("Endpoint"), backend.endpoint);
    driver.replace_text_in_target(&audit::targets::manage_field("API Key"), backend.api_key);
    driver.replace_text_in_target(
        &audit::targets::manage_field("API Key Env Var"),
        backend.api_key_env_var,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Max Concurrent"),
        backend.max_concurrent,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Max Queue Depth"),
        backend.max_queue_depth,
    );
    let current_enabled = matches!(
        driver.app.state.manage.draft.as_ref(),
        Some(ManageDraft::Backend(draft)) if draft.enabled
    );
    if current_enabled != backend.enabled {
        driver.click_target(&audit::targets::manage_toggle("Enabled"));
    }
    driver.replace_text_in_target(&audit::targets::manage_field("Models"), backend.models);
    driver.replace_text_in_target(
        &audit::targets::manage_field("Probe Status"),
        backend.probe_status,
    );
    if let Some(ManageDraft::Backend(draft)) = driver.app.state.manage.draft.as_mut() {
        draft.backend_id = backend.backend_id.to_string();
        draft.name = backend.name.to_string();
        draft.provider_kind = backend.provider_kind.to_string();
        draft.endpoint = backend.endpoint.to_string();
        draft.api_key = backend.api_key.to_string();
        draft.api_key_env_var = backend.api_key_env_var.to_string();
        draft.max_concurrent = backend.max_concurrent.to_string();
        draft.max_queue_depth = backend.max_queue_depth.to_string();
        draft.enabled = backend.enabled;
        draft.models = backend.models.to_string();
        draft.probe_status = backend.probe_status.to_string();
    }
}

pub(crate) fn fill_inference_profile_draft(
    driver: &mut AuditDriver,
    profile: &ManageInferenceProfileForm<'_>,
) {
    driver.replace_text_in_target(
        &audit::targets::manage_field("Profile ID"),
        profile.profile_id,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Display Name"),
        profile.display_name,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Context Window"),
        profile.context_window,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Max Output Tokens"),
        profile.max_output_tokens,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Max Turns"),
        profile.max_turns,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Temperature"),
        profile.temperature,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Stream Batch Ms"),
        profile.stream_batch_ms,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Deadline Duration Secs"),
        profile.deadline_duration_secs,
    );
}

pub(crate) fn assert_behavior_draft_bindings(
    driver: &AuditDriver,
    behavior_id: &str,
    agent_did: &str,
    backend_id: &str,
    tool_selection_id: &str,
    inference_profile_id: &str,
) {
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::Behavior(draft)) => {
            assert_eq!(draft.behavior_id, behavior_id);
            assert_eq!(draft.agent_did, agent_did);
            assert_eq!(draft.backend_id, backend_id);
            assert_eq!(draft.tool_selection_id, tool_selection_id);
            assert_eq!(draft.inference_profile_id, inference_profile_id);
        }
        other => panic!("expected behavior draft, got {other:?}"),
    }
}

pub(crate) fn assert_behavior_draft_fields(
    driver: &AuditDriver,
    behavior: &ManageBehaviorForm<'_>,
) {
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::Behavior(draft)) => {
            assert_eq!(draft.behavior_id, behavior.behavior_id);
            assert_eq!(draft.agent_did, behavior.agent_did);
            assert_eq!(draft.display_name, behavior.display_name);
            assert_eq!(draft.system_prompt, behavior.system_prompt);
            assert_eq!(draft.backend_id, behavior.backend_id);
            assert_eq!(draft.model_name, behavior.model_name);
            assert_eq!(draft.tool_selection_id, behavior.tool_selection_id);
            assert_eq!(draft.inference_profile_id, behavior.inference_profile_id);
            assert_eq!(draft.compaction_strategy, behavior.compaction_strategy);
            assert_eq!(draft.compaction_threshold, behavior.compaction_threshold);
            assert_eq!(draft.enabled, behavior.enabled);
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
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::Backend(draft)) => {
            assert_eq!(draft.backend_id, backend_id);
            assert_eq!(draft.provider_kind, provider_kind);
            assert_eq!(draft.endpoint, endpoint);
            assert!(draft.models.contains(model_name));
        }
        other => panic!("expected backend draft, got {other:?}"),
    }
}

pub(crate) fn assert_backend_draft_fields(
    driver: &AuditDriver,
    backend: &ManageBackendForm<'_>,
) {
    let expected_models = backend
        .models
        .split([',', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::Backend(draft)) => {
            assert_eq!(draft.backend_id, backend.backend_id);
            assert_eq!(draft.name, backend.name);
            assert_eq!(draft.provider_kind, backend.provider_kind);
            assert_eq!(draft.endpoint, backend.endpoint);
            assert_eq!(draft.api_key, backend.api_key);
            assert_eq!(draft.api_key_env_var, backend.api_key_env_var);
            assert_eq!(draft.max_concurrent, backend.max_concurrent);
            assert_eq!(draft.max_queue_depth, backend.max_queue_depth);
            assert_eq!(draft.enabled, backend.enabled);
            assert_eq!(
                draft
                    .models
                    .split([',', '\n'])
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>(),
                expected_models
            );
            assert_eq!(draft.probe_status, backend.probe_status);
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
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::InferenceProfile(draft)) => {
            assert_eq!(draft.profile_id, profile_id);
            assert_eq!(draft.max_output_tokens, max_output_tokens);
            assert_eq!(draft.max_turns, max_turns);
        }
        other => panic!("expected inference profile draft, got {other:?}"),
    }
}

pub(crate) fn assert_inference_profile_draft_fields(
    driver: &AuditDriver,
    profile: &ManageInferenceProfileForm<'_>,
) {
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::InferenceProfile(draft)) => {
            assert_eq!(draft.profile_id, profile.profile_id);
            assert_eq!(draft.display_name, profile.display_name);
            assert_eq!(draft.context_window, profile.context_window);
            assert_eq!(draft.max_output_tokens, profile.max_output_tokens);
            assert_eq!(draft.max_turns, profile.max_turns);
            assert_eq!(draft.temperature, profile.temperature);
            assert_eq!(draft.stream_batch_ms, profile.stream_batch_ms);
            assert_eq!(draft.deadline_duration_secs, profile.deadline_duration_secs);
        }
        other => panic!("expected inference profile draft, got {other:?}"),
    }
}
