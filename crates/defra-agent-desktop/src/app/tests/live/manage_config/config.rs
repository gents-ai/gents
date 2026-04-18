use super::*;

#[derive(Debug, Clone)]
pub(crate) struct LiveSwitchConfig {
    pub(crate) backend_id: String,
    pub(crate) profile_id: String,
    pub(super) tool_prompt: &'static str,
    pub(super) profile_max_output_tokens: i64,
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
