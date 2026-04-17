use super::*;

pub(crate) fn compact_field(value: Option<&str>) -> String {
    match value {
        Some(value) if value.len() > 96 => format!("{}...", &value[..96]),
        Some(value) => value.to_string(),
        None => "<none>".to_string(),
    }
}

pub(crate) fn describe_live_config_state(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    agent_did: &str,
    docs: &LiveAgentDocs,
    switch_backend_id: &str,
    switch_profile_id: &str,
) -> String {
    let refresh = runtime
        .block_on(core.refresh_store())
        .map(|_| "ok".to_string())
        .unwrap_or_else(|error| format!("error={error:#}"));
    let snapshot = core.store().snapshot();
    let behavior = snapshot
        .behaviors
        .iter()
        .find(|row| row.behavior_id == docs.behavior_id)
        .map(|row| {
            format!(
                "behavior(agent={:?}, backend={:?}, model={:?}, tool_selection={:?}, profile={:?}, enabled={:?}, prompt={})",
                row.agent_did,
                row.backend_id,
                row.model_name,
                row.tool_selection_id,
                row.inference_profile_id,
                row.enabled,
                compact_field(row.system_prompt.as_deref())
            )
        })
        .unwrap_or_else(|| "behavior=<missing>".to_string());
    let original_backend = snapshot
        .inference_backends
        .iter()
        .find(|row| row.backend_id == docs.backend_id)
        .map(|row| {
            format!(
                "original_backend(enabled={:?}, probe={:?}, endpoint={}, models={:?})",
                row.enabled,
                row.probe_status.as_deref(),
                compact_field(row.endpoint.as_deref()),
                row.models
            )
        })
        .unwrap_or_else(|| "original_backend=<missing>".to_string());
    let switch_backend = snapshot
        .inference_backends
        .iter()
        .find(|row| row.backend_id == switch_backend_id)
        .map(|row| {
            format!(
                "switch_backend(enabled={:?}, probe={:?}, endpoint={}, models={:?})",
                row.enabled,
                row.probe_status.as_deref(),
                compact_field(row.endpoint.as_deref()),
                row.models
            )
        })
        .unwrap_or_else(|| "switch_backend=<missing>".to_string());
    let tool_selection = snapshot
        .tool_selections
        .iter()
        .find(|row| row.selection_id == docs.tool_selection_id)
        .map(|row| {
            format!(
                "tools(agent={:?}, enable_file={:?}, file_mode={:?}, enable_bash={:?}, bash_mode={:?}, cli={:?}, meta={:?})",
                row.agent_did,
                row.enable_file_tools,
                row.file_tools_mode,
                row.enable_bash,
                row.bash_mode,
                row.cli_tool_names,
                row.enable_meta_tools
            )
        })
        .unwrap_or_else(|| "tools=<missing>".to_string());
    let switch_profile = snapshot
        .inference_profiles
        .iter()
        .find(|row| row.profile_id == switch_profile_id)
        .map(|row| {
            format!(
                "switch_profile(max_output={:?}, max_turns={:?}, temp={:?})",
                row.max_output_tokens, row.max_turns, row.temperature
            )
        })
        .unwrap_or_else(|| "switch_profile=<missing>".to_string());
    let runtime_row = snapshot
        .latest_runtime(agent_did)
        .map(|row| {
            format!(
                "runtime(process={:?}, phase={:?}, active={:?}, router={:?}, default={:?}, runnable={:?}, unavailable={:?}, result={:?}, error={})",
                row.process_state,
                row.reconcile_phase,
                row.active_generation,
                row.router_generation,
                row.default_behavior_id,
                row.runnable_behavior_count,
                row.unavailable_behavior_count,
                row.last_reconcile_result,
                compact_field(row.last_reconcile_error.as_deref())
            )
        })
        .unwrap_or_else(|| "runtime=<missing>".to_string());

    format!(
        "{label}: refresh={refresh}; {runtime_row}; {behavior}; {original_backend}; {switch_backend}; {tool_selection}; {switch_profile}"
    )
}
