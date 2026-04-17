use super::*;

#[derive(Debug, Clone)]
pub(crate) struct LiveSwitchConfig {
    pub(crate) backend_id: String,
    pub(crate) profile_id: String,
    tool_prompt: &'static str,
    profile_max_output_tokens: i64,
}

impl LiveSwitchConfig {
    pub(crate) fn for_deployment(deployment: &LiveDeploymentCase<'_>) -> Self {
        Self {
            backend_id: format!("{}:switch-backend", deployment.docs.behavior_id),
            profile_id: format!("{}:switch-profile", deployment.docs.behavior_id),
            tool_prompt: "When the user asks about local files, you must call read_file instead of guessing. The token is not available in the conversation. For multi-file requests, call read_file separately for every requested path and respond with only the requested tokens.",
            profile_max_output_tokens: 1536,
        }
    }
}

pub(crate) fn prepare_live_switch_config(
    runtime: &tokio::runtime::Runtime,
    desktop_client: &ClientCore,
    deployment: &LiveDeploymentCase<'_>,
    backend: &AgentBackendConfig,
) -> Result<LiveSwitchConfig> {
    let config = LiveSwitchConfig::for_deployment(deployment);

    runtime.block_on(async {
        desktop_client
            .save_backend(&InferenceBackendRow {
                backend_id: config.backend_id.clone(),
                name: Some("Alpha Switch Backend".to_string()),
                provider_kind: Some(backend.provider_kind.as_str().to_string()),
                endpoint: Some(backend.endpoint.clone()),
                api_key: backend.api_key.clone(),
                api_key_env_var: backend.api_key_env_var.clone(),
                max_concurrent: Some(2),
                max_queue_depth: Some(100),
                enabled: Some(true),
                models: vec![backend.model_name.clone()],
                last_probe: None,
                probe_status: Some("healthy".to_string()),
            })
            .await?;
        desktop_client
            .save_inference_profile(&InferenceProfileRow {
                profile_id: config.profile_id.clone(),
                display_name: Some("Alpha Switch Profile".to_string()),
                context_window: Some(65536),
                max_output_tokens: Some(2048),
                max_turns: Some(16),
                temperature: Some(0.0),
                stream_batch_ms: Some(40),
                deadline_duration_secs: Some(240),
            })
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    wait_for_value(
        &format!(
            "{} switch backend saved in live desktop store",
            deployment.label
        ),
        Duration::from_secs(20),
        || {
            runtime.block_on(desktop_client.refresh_store()).ok()?;
            let snapshot = desktop_client.store().snapshot();
            let has_backend = snapshot
                .inference_backends
                .iter()
                .any(|row| row.backend_id == config.backend_id);
            let has_profile = snapshot
                .inference_profiles
                .iter()
                .any(|row| row.profile_id == config.profile_id);
            (has_backend && has_profile).then_some(())
        },
    )?;

    Ok(config)
}

pub(crate) fn assert_live_operator_switching_baseline(
    driver: &mut AuditDriver,
    alpha: &LiveDeploymentCase<'_>,
    alpha_submission: &LiveSubmissionCase,
    bravo: &LiveDeploymentCase<'_>,
    bravo_submission: &LiveSubmissionCase,
    backend: &AgentBackendConfig,
) -> Result<()> {
    open_chat_conversation_and_assert_isolation(
        driver,
        alpha,
        alpha_submission,
        bravo_submission.prompt.as_str(),
        "alpha transcript leaked bravo prompt after switching deployments",
    )?;
    open_chat_conversation_and_assert_isolation(
        driver,
        bravo,
        bravo_submission,
        alpha_submission.prompt.as_str(),
        "bravo transcript leaked alpha prompt after switching deployments",
    )?;

    open_operator_entity_and_assert_visibility(
        driver,
        alpha,
        OperatorSection::Behaviors,
        &alpha.docs.behavior_id,
        &[bravo.docs.behavior_id.as_str()],
        "alpha behavior row after operator server switch",
    )?;
    assert_behavior_draft_bindings(
        driver,
        &alpha.docs.behavior_id,
        &alpha.agent_did,
        &alpha.docs.backend_id,
        &alpha.docs.tool_selection_id,
        &alpha.docs.inference_profile_id,
    );

    open_operator_entity_and_assert_visibility(
        driver,
        alpha,
        OperatorSection::Backends,
        &alpha.docs.backend_id,
        &[bravo.docs.backend_id.as_str()],
        "alpha backend row after operator server switch",
    )?;
    assert_backend_draft_matches(
        driver,
        &alpha.docs.backend_id,
        backend.provider_kind.as_str(),
        backend.endpoint.as_str(),
        backend.model_name.as_str(),
    );

    open_operator_entity_and_assert_visibility(
        driver,
        alpha,
        OperatorSection::InferenceProfiles,
        &alpha.docs.inference_profile_id,
        &[bravo.docs.inference_profile_id.as_str()],
        "alpha inference profile row after operator server switch",
    )?;
    assert_inference_profile_draft_matches(driver, &alpha.docs.inference_profile_id, "1024", "12");

    let alpha_timeline_texts = open_operator_request_timeline_and_assert_visibility(
        driver,
        alpha,
        &alpha_submission.request_id,
        &[bravo_submission.request_id.as_str()],
        "alpha request row after operator server switch",
    )?;
    assert!(alpha_timeline_texts
        .iter()
        .any(|text| text.contains(alpha_submission.prompt.as_str())));
    assert!(alpha_timeline_texts
        .iter()
        .any(|text| text.contains(alpha_submission.response.trim())));

    open_operator_entity_and_assert_visibility(
        driver,
        bravo,
        OperatorSection::Behaviors,
        &bravo.docs.behavior_id,
        &[alpha.docs.behavior_id.as_str()],
        "bravo behavior row after operator server switch",
    )?;
    assert_behavior_draft_bindings(
        driver,
        &bravo.docs.behavior_id,
        &bravo.agent_did,
        &bravo.docs.backend_id,
        &bravo.docs.tool_selection_id,
        &bravo.docs.inference_profile_id,
    );

    open_operator_entity_and_assert_visibility(
        driver,
        bravo,
        OperatorSection::Backends,
        &bravo.docs.backend_id,
        &[alpha.docs.backend_id.as_str()],
        "bravo backend row after operator server switch",
    )?;
    assert_backend_draft_matches(
        driver,
        &bravo.docs.backend_id,
        backend.provider_kind.as_str(),
        backend.endpoint.as_str(),
        backend.model_name.as_str(),
    );

    open_operator_entity_and_assert_visibility(
        driver,
        bravo,
        OperatorSection::InferenceProfiles,
        &bravo.docs.inference_profile_id,
        &[alpha.docs.inference_profile_id.as_str()],
        "bravo inference profile row after operator server switch",
    )?;
    assert_inference_profile_draft_matches(driver, &bravo.docs.inference_profile_id, "1024", "12");

    let bravo_timeline_texts = open_operator_request_timeline_and_assert_visibility(
        driver,
        bravo,
        &bravo_submission.request_id,
        &[alpha_submission.request_id.as_str()],
        "bravo request row after operator server switch",
    )?;
    assert!(bravo_timeline_texts
        .iter()
        .any(|text| text.contains(bravo_submission.prompt.as_str())));
    assert!(bravo_timeline_texts
        .iter()
        .any(|text| text.contains(bravo_submission.response.trim())));

    Ok(())
}

pub(crate) fn apply_live_switch_config_in_operator(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    backend: &AgentBackendConfig,
    config: &LiveSwitchConfig,
) -> Result<()> {
    open_operator_entity_and_assert_visibility(
        driver,
        deployment,
        OperatorSection::Behaviors,
        &deployment.docs.behavior_id,
        &[],
        "behavior row before config edit",
    )?;
    driver.replace_text_in_target(
        &audit::targets::operator_field("System Prompt"),
        config.tool_prompt,
    );
    driver.replace_text_in_target(
        &audit::targets::operator_field("Backend ID"),
        &config.backend_id,
    );
    driver.replace_text_in_target(
        &audit::targets::operator_field("Model Name"),
        backend.model_name.as_str(),
    );
    driver.replace_text_in_target(
        &audit::targets::operator_field("Inference Profile ID"),
        &config.profile_id,
    );
    match driver.app.state.operator.draft.as_ref() {
        Some(OperatorDraft::Behavior(draft)) => {
            assert_eq!(draft.behavior_id, deployment.docs.behavior_id);
            assert_eq!(draft.backend_id, config.backend_id);
            assert_eq!(draft.inference_profile_id, config.profile_id);
            assert_eq!(draft.system_prompt, config.tool_prompt);
        }
        other => panic!("expected edited behavior draft, got {other:?}"),
    }
    driver.click_target(audit::targets::OPERATOR_APPLY);
    wait_for_value(
        "behavior config edit persisted on desktop",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .behaviors
                    .iter()
                    .find(|row| row.behavior_id == deployment.docs.behavior_id)
                    .filter(|row| {
                        row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                            && row.backend_id.as_deref() == Some(config.backend_id.as_str())
                            && row.inference_profile_id.as_deref()
                                == Some(config.profile_id.as_str())
                            && row.system_prompt.as_deref() == Some(config.tool_prompt)
                    })
                    .map(|row| row.behavior_id.clone())
            })
        },
    )?;

    open_operator_entity_and_assert_visibility(
        driver,
        deployment,
        OperatorSection::ToolSelections,
        &deployment.docs.tool_selection_id,
        &[],
        "tool selection after config edit",
    )?;
    driver.click_target(&audit::targets::operator_toggle("Enable File Tools"));
    driver.replace_text_in_target(
        &audit::targets::operator_field("File Tools Mode"),
        "ReadOnly",
    );
    match driver.app.state.operator.draft.as_ref() {
        Some(OperatorDraft::ToolSelection(draft)) => {
            assert_eq!(draft.selection_id, deployment.docs.tool_selection_id);
            assert_eq!(draft.agent_did, deployment.agent_did);
            assert!(draft.enable_file_tools);
            assert_eq!(draft.file_tools_mode, "ReadOnly");
            assert!(!draft.enable_bash);
        }
        other => panic!("expected edited tool selection draft, got {other:?}"),
    }
    driver.click_target(audit::targets::OPERATOR_APPLY);
    wait_for_value(
        "tool selection edit persisted on desktop",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .tool_selections
                    .iter()
                    .find(|row| row.selection_id == deployment.docs.tool_selection_id)
                    .filter(|row| {
                        row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                            && row.enable_file_tools == Some(true)
                            && row.file_tools_mode.as_deref() == Some("ReadOnly")
                    })
                    .map(|row| row.selection_id.clone())
            })
        },
    )?;

    open_operator_entity_and_assert_visibility(
        driver,
        deployment,
        OperatorSection::Backends,
        &config.backend_id,
        &[deployment.docs.backend_id.as_str()],
        "switched backend row after behavior binding edit",
    )?;
    match driver.app.state.operator.draft.as_ref() {
        Some(OperatorDraft::Backend(draft)) => {
            assert_eq!(draft.backend_id, config.backend_id);
            assert_eq!(draft.endpoint, backend.endpoint);
            assert!(draft.models.contains(backend.model_name.as_str()));
            assert_eq!(draft.probe_status, "healthy");
        }
        other => panic!("expected switched backend draft, got {other:?}"),
    }

    open_operator_entity_and_assert_visibility(
        driver,
        deployment,
        OperatorSection::InferenceProfiles,
        &config.profile_id,
        &[deployment.docs.inference_profile_id.as_str()],
        "switched inference profile row after behavior binding edit",
    )?;
    driver.replace_text_in_target(
        &audit::targets::operator_field("Max Output Tokens"),
        &config.profile_max_output_tokens.to_string(),
    );
    match driver.app.state.operator.draft.as_ref() {
        Some(OperatorDraft::InferenceProfile(draft)) => {
            assert_eq!(draft.profile_id, config.profile_id);
            assert_eq!(
                draft.max_output_tokens,
                config.profile_max_output_tokens.to_string()
            );
            assert_eq!(draft.max_turns, "16");
            assert_eq!(draft.temperature, "0");
        }
        other => panic!("expected edited switched profile draft, got {other:?}"),
    }
    driver.click_target(audit::targets::OPERATOR_APPLY);
    wait_for_value(
        "inference profile edit persisted on desktop",
        Duration::from_secs(10),
        || {
            driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .inference_profiles
                    .iter()
                    .find(|row| row.profile_id == config.profile_id)
                    .filter(|row| row.max_output_tokens == Some(config.profile_max_output_tokens))
                    .map(|row| row.profile_id.clone())
            })
        },
    )?;

    Ok(())
}

pub(crate) fn wait_for_live_switch_config_replication(
    runtime: &tokio::runtime::Runtime,
    desktop_client: &ClientCore,
    deployment: &LiveDeploymentCase<'_>,
    backend: &AgentBackendConfig,
    config: &LiveSwitchConfig,
    desktop_initial_generation: i64,
    remote_initial_generation: i64,
) -> Result<()> {
    wait_for_value(
        "behavior/tool config and generation after UI edits",
        Duration::from_secs(120),
        || {
            runtime.block_on(desktop_client.refresh_store()).ok()?;
            let snapshot = desktop_client.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| {
                    row.backend_id.as_deref() == Some(config.backend_id.as_str())
                        && row.inference_profile_id.as_deref() == Some(config.profile_id.as_str())
                        && row.system_prompt.as_deref() == Some(config.tool_prompt)
                });
            let tools_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == deployment.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            let profile_ready = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == config.profile_id)
                .is_some_and(|row| row.max_output_tokens == Some(config.profile_max_output_tokens));
            let runtime_ready = snapshot
                .latest_runtime(&deployment.agent_did)
                .is_some_and(|row| {
                    row.router_generation
                        .or(row.active_generation)
                        .is_some_and(|generation| generation > desktop_initial_generation)
                        && row.runnable_behavior_count == Some(1)
                        && row.unavailable_behavior_count == Some(0)
                        && row
                            .last_reconcile_error
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                });
            (behavior_ready && tools_ready && profile_ready && runtime_ready).then_some(())
        },
    )
    .with_context(|| {
        format!(
            "desktop state: {}\nremote state: {}",
            describe_live_config_state(
                runtime,
                desktop_client,
                "desktop",
                &deployment.agent_did,
                &deployment.docs,
                &config.backend_id,
                &config.profile_id,
            ),
            describe_live_config_state(
                runtime,
                deployment.remote_core,
                "remote",
                &deployment.agent_did,
                &deployment.docs,
                &config.backend_id,
                &config.profile_id,
            )
        )
    })?;

    wait_for_value(
        "behavior/tool config replicated to remote runtime",
        Duration::from_secs(120),
        || {
            runtime
                .block_on(deployment.remote_core.refresh_store())
                .ok()?;
            let snapshot = deployment.remote_core.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| {
                    row.backend_id.as_deref() == Some(config.backend_id.as_str())
                        && row.inference_profile_id.as_deref() == Some(config.profile_id.as_str())
                        && row.system_prompt.as_deref() == Some(config.tool_prompt)
                });
            let backend_ready = snapshot
                .inference_backends
                .iter()
                .find(|row| row.backend_id == config.backend_id)
                .is_some_and(|row| {
                    row.endpoint.as_deref() == Some(backend.endpoint.as_str())
                        && row.models.iter().any(|model| model == &backend.model_name)
                });
            let tools_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == deployment.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            let profile_ready = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == config.profile_id)
                .is_some_and(|row| row.max_output_tokens == Some(config.profile_max_output_tokens));
            let runtime_ready = snapshot
                .latest_runtime(&deployment.agent_did)
                .is_some_and(|row| {
                    row.router_generation
                        .or(row.active_generation)
                        .is_some_and(|generation| generation > remote_initial_generation)
                        && row.runnable_behavior_count == Some(1)
                        && row.unavailable_behavior_count == Some(0)
                        && row
                            .last_reconcile_error
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                });
            (behavior_ready && backend_ready && tools_ready && profile_ready && runtime_ready)
                .then_some(())
        },
    )
    .with_context(|| {
        format!(
            "desktop state: {}\nremote state: {}",
            describe_live_config_state(
                runtime,
                desktop_client,
                "desktop",
                &deployment.agent_did,
                &deployment.docs,
                &config.backend_id,
                &config.profile_id,
            ),
            describe_live_config_state(
                runtime,
                deployment.remote_core,
                "remote",
                &deployment.agent_did,
                &deployment.docs,
                &config.backend_id,
                &config.profile_id,
            )
        )
    })?;

    wait_for_stable_runtime_ready(
        runtime,
        deployment.remote_core,
        "after remote config replication",
        &deployment.agent_did,
        Duration::from_secs(10),
        Duration::from_secs(90),
    )
}
