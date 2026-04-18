use super::*;

pub(crate) fn apply_live_switch_config_in_manage(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    backend: &AgentBackendConfig,
    config: &LiveSwitchConfig,
) -> Result<()> {
    apply_behavior_switch(driver, deployment, backend, config)?;
    apply_tool_selection_switch(driver, deployment)?;
    assert_switched_backend_draft(driver, deployment, backend, config)?;
    apply_switched_profile(driver, deployment, config)
}

fn apply_behavior_switch(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    backend: &AgentBackendConfig,
    config: &LiveSwitchConfig,
) -> Result<()> {
    open_manage_entity_and_assert_visibility(
        driver,
        deployment,
        ManageSection::Behaviors,
        &deployment.docs.behavior_id,
        &[],
        "behavior row before config edit",
    )?;
    driver.replace_text_in_target(
        &audit::targets::manage_field("System Prompt"),
        config.tool_prompt,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Backend ID"),
        &config.backend_id,
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Model Name"),
        backend.model_name.as_str(),
    );
    driver.replace_text_in_target(
        &audit::targets::manage_field("Inference Profile ID"),
        &config.profile_id,
    );
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::Behavior(draft)) => {
            assert_eq!(draft.behavior_id, deployment.docs.behavior_id);
            assert_eq!(draft.backend_id, config.backend_id);
            assert_eq!(draft.inference_profile_id, config.profile_id);
            assert_eq!(draft.system_prompt, config.tool_prompt);
        }
        other => panic!("expected edited behavior draft, got {other:?}"),
    }
    driver.click_target(audit::targets::MANAGE_APPLY);
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
    Ok(())
}

fn apply_tool_selection_switch(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
) -> Result<()> {
    open_manage_entity_and_assert_visibility(
        driver,
        deployment,
        ManageSection::ToolSelections,
        &deployment.docs.tool_selection_id,
        &[],
        "tool selection after config edit",
    )?;
    driver.click_target(&audit::targets::manage_toggle("Enable File Tools"));
    driver.replace_text_in_target(
        &audit::targets::manage_field("File Tools Mode"),
        "ReadOnly",
    );
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::ToolSelection(draft)) => {
            assert_eq!(draft.selection_id, deployment.docs.tool_selection_id);
            assert_eq!(draft.agent_did, deployment.agent_did);
            assert!(draft.enable_file_tools);
            assert_eq!(draft.file_tools_mode, "ReadOnly");
            assert!(!draft.enable_bash);
        }
        other => panic!("expected edited tool selection draft, got {other:?}"),
    }
    driver.click_target(audit::targets::MANAGE_APPLY);
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
    Ok(())
}

fn assert_switched_backend_draft(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    backend: &AgentBackendConfig,
    config: &LiveSwitchConfig,
) -> Result<()> {
    open_manage_entity_and_assert_visibility(
        driver,
        deployment,
        ManageSection::Backends,
        &config.backend_id,
        &[],
        "switched backend row after behavior binding edit",
    )?;
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::Backend(draft)) => {
            assert_eq!(draft.backend_id, config.backend_id);
            assert_eq!(draft.endpoint, backend.endpoint);
            assert!(draft.models.contains(backend.model_name.as_str()));
            assert_eq!(draft.probe_status, "healthy");
        }
        other => panic!("expected switched backend draft, got {other:?}"),
    }
    Ok(())
}

fn apply_switched_profile(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    config: &LiveSwitchConfig,
) -> Result<()> {
    open_manage_entity_and_assert_visibility(
        driver,
        deployment,
        ManageSection::InferenceProfiles,
        &config.profile_id,
        &[],
        "switched inference profile row after behavior binding edit",
    )?;
    driver.replace_text_in_target(
        &audit::targets::manage_field("Max Output Tokens"),
        &config.profile_max_output_tokens.to_string(),
    );
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::InferenceProfile(draft)) => {
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
    driver.click_target(audit::targets::MANAGE_APPLY);
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
