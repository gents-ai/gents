use super::*;

pub(crate) fn wait_for_live_switch_config_replication(
    runtime: &tokio::runtime::Runtime,
    desktop_client: &ClientCore,
    deployment: &LiveDeploymentCase<'_>,
    backend: &AgentBackendConfig,
    config: &LiveSwitchConfig,
    desktop_initial_generation: i64,
    remote_initial_generation: i64,
) -> Result<()> {
    wait_for_local_switch_config(
        runtime,
        desktop_client,
        deployment,
        config,
        desktop_initial_generation,
    )
    .with_context(|| describe_switch_config_state(runtime, desktop_client, deployment, config))?;

    wait_for_remote_switch_config(
        runtime,
        deployment,
        backend,
        config,
        remote_initial_generation,
    )
    .with_context(|| describe_switch_config_state(runtime, desktop_client, deployment, config))?;

    wait_for_stable_runtime_ready(
        runtime,
        deployment.remote_core,
        "after remote config replication",
        &deployment.agent_did,
        Duration::from_secs(10),
        Duration::from_secs(90),
    )
}

fn wait_for_local_switch_config(
    runtime: &tokio::runtime::Runtime,
    desktop_client: &ClientCore,
    deployment: &LiveDeploymentCase<'_>,
    config: &LiveSwitchConfig,
    desktop_initial_generation: i64,
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
}

fn wait_for_remote_switch_config(
    runtime: &tokio::runtime::Runtime,
    deployment: &LiveDeploymentCase<'_>,
    backend: &AgentBackendConfig,
    config: &LiveSwitchConfig,
    remote_initial_generation: i64,
) -> Result<()> {
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
}

fn describe_switch_config_state(
    runtime: &tokio::runtime::Runtime,
    desktop_client: &ClientCore,
    deployment: &LiveDeploymentCase<'_>,
    config: &LiveSwitchConfig,
) -> String {
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
}
