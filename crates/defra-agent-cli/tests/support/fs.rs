use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use defra_agent::{default_behavior_id_for_agent, default_inference_profile_id_for_behavior};
use serde_json::Value;

use super::graphql::escape_graphql_string;

pub fn write_json_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory {}", parent.display()))?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("writing JSON file {}", path.display()))?;
    Ok(())
}

pub fn read_json_file(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("reading JSON file {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("decoding JSON file {}", path.display()))
}

pub fn rewrite_manifest_agent_dids(root: &Path, agent_did: &str) -> Result<()> {
    let principal_path = root.join("agent-principal.json");
    let mut principal = read_json_file(&principal_path)?;
    principal["agent_did"] = Value::String(agent_did.to_string());
    write_json_file(&principal_path, &principal)?;

    for dir_name in ["agent-behaviors", "tool-selections"] {
        let collection_dir = root.join(dir_name);
        if !collection_dir.exists() {
            continue;
        }
        for entry in fs::read_dir(&collection_dir)
            .with_context(|| format!("reading {}", collection_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("object.json");
            let mut object = read_json_file(&path)?;
            object["agent_did"] = Value::String(agent_did.to_string());
            write_json_file(&path, &object)?;
        }
    }

    Ok(())
}

pub fn assert_manifest_agent_dids(root: &Path, expected_agent_did: &str) -> Result<()> {
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    assert_eq!(
        principal.get("agent_did").and_then(Value::as_str),
        Some(expected_agent_did)
    );

    for dir_name in ["agent-behaviors", "tool-selections"] {
        let collection_dir = root.join(dir_name);
        if !collection_dir.exists() {
            continue;
        }
        for entry in fs::read_dir(&collection_dir)
            .with_context(|| format!("reading {}", collection_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("object.json");
            let object = read_json_file(&path)?;
            assert_eq!(
                object.get("agent_did").and_then(Value::as_str),
                Some(expected_agent_did),
                "wrong agent_did in {}",
                path.display()
            );
        }
    }

    Ok(())
}

pub fn manifest_contains(root: &Path, needle: &str) -> Result<bool> {
    fn visit(path: &Path, needle: &str) -> Result<bool> {
        if path.is_dir() {
            for entry in
                fs::read_dir(path).with_context(|| format!("reading {}", path.display()))?
            {
                if visit(&entry?.path(), needle)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            return Ok(false);
        }
        let contents =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(contents.contains(needle))
    }

    visit(root, needle)
}

pub fn read_captured_log(log: Option<&tempfile::NamedTempFile>) -> Result<String> {
    let Some(log) = log else {
        return Ok(String::new());
    };
    let bytes = fs::read(log.path())
        .with_context(|| format!("reading captured log {}", log.path().display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn write_manifest_root_from_export(root: &Path, exported: &Value) -> Result<()> {
    write_json_file(
        &root.join("agent-principal.json"),
        &project_object_fields(
            exported
                .get("agent_principal")
                .ok_or_else(|| anyhow!("exported bundle missing agent_principal"))?,
            &[
                "agent_did",
                "display_name",
                "default_behavior_id",
                "enabled",
            ],
        )?,
    )?;

    write_per_doc_collection(
        root,
        "agent-behaviors",
        "behavior_id",
        exported
            .get("agent_behaviors")
            .ok_or_else(|| anyhow!("exported bundle missing agent_behaviors"))?,
        &[
            "behavior_id",
            "agent_did",
            "display_name",
            "system_prompt",
            "backend_id",
            "model_name",
            "tool_selection_id",
            "inference_profile_id",
            "compaction_strategy",
            "compaction_threshold",
            "enabled",
        ],
    )?;
    write_per_doc_collection(
        root,
        "tool-selections",
        "selection_id",
        exported
            .get("tool_selections")
            .ok_or_else(|| anyhow!("exported bundle missing tool_selections"))?,
        &[
            "selection_id",
            "agent_did",
            "display_name",
            "enable_file_tools",
            "file_tools_mode",
            "file_tool_root",
            "enable_bash",
            "bash_mode",
            "command_execution_policy",
            "command_allowed_argv_prefixes",
            "command_forbidden_argv_prefixes",
            "command_network_mode",
            "cli_tool_names",
            "enable_meta_tools",
            "allowed_mcp_service_ids",
            "delegate_to",
            "backgroundable_tool_names",
            "enable_defra_query",
            "defra_query_collections",
            "subagent_targets",
            "subagent_spawn_enabled",
            "subagent_steering_enabled",
            "subagent_background_enabled",
            "subagent_allow_cross_deployment",
            "cross_deployment_spawn_timeout_seconds",
        ],
    )?;
    write_per_doc_collection(
        root,
        "inference-backends",
        "backend_id",
        exported
            .get("inference_backends")
            .ok_or_else(|| anyhow!("exported bundle missing inference_backends"))?,
        &[
            "backend_id",
            "name",
            "endpoint",
            "api_key_env_var",
            "max_concurrent",
            "max_queue_depth",
            "enabled",
            "models",
        ],
    )?;
    if let Some(profiles) = exported.get("inference_profiles") {
        write_per_doc_collection(
            root,
            "inference-profiles",
            "profile_id",
            profiles,
            &[
                "profile_id",
                "display_name",
                "context_window",
                "max_output_tokens",
                "max_turns",
                "temperature",
                "stream_batch_ms",
                "deadline_duration_secs",
            ],
        )?;
    }
    if let Some(services) = exported.get("tool_service_registries") {
        write_per_doc_collection(
            root,
            "tool-services",
            "service_id",
            services,
            &[
                "service_id",
                "display_name",
                "description",
                "hostname",
                "tailscale_ip",
                "lan_ip",
                "mcp_port",
                "mcp_path",
                "send_agent_did",
            ],
        )?;
    }
    if let Some(tasks) = exported.get("tasks") {
        write_per_doc_collection(
            root,
            "tasks",
            "task_id",
            tasks,
            &[
                "task_id",
                "name",
                "description",
                "behavior_id",
                "prompt_template",
                "enabled",
                "output_schema_ref",
            ],
        )?;
    }
    if let Some(schedules) = exported.get("schedules") {
        write_per_doc_collection(
            root,
            "schedules",
            "schedule_id",
            schedules,
            &[
                "schedule_id",
                "task_id",
                "interval_secs",
                "cron",
                "timezone",
                "missed_run_policy",
                "enabled",
                "concurrency",
            ],
        )?;
    }

    Ok(())
}

fn write_per_doc_collection(
    root: &Path,
    dir_name: &str,
    unique_field: &str,
    rows: &Value,
    fields: &[&str],
) -> Result<()> {
    let Some(rows) = rows.as_array() else {
        return Ok(());
    };
    for row in rows {
        let object = project_object_fields(row, fields)?;
        let handle = object
            .get(unique_field)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("row missing {unique_field}: {row}"))?;
        let dir = root.join(dir_name).join(handle);
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        write_json_file(&dir.join("object.json"), &object)?;
    }
    Ok(())
}

pub fn project_object_fields(value: &Value, fields: &[&str]) -> Result<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("expected object while projecting manifest fields: {value}"))?;
    let mut projected = serde_json::Map::new();
    for field in fields {
        if let Some(value) = object.get(*field) {
            projected.insert((*field).to_string(), value.clone());
        }
    }
    Ok(Value::Object(projected))
}

pub fn read_runtime_state_json(home_dir: &Path) -> Result<Value> {
    let path = if home_dir.join("runtime.json").exists() {
        home_dir.join("runtime.json")
    } else {
        home_dir.join(".defra-agent").join("runtime.json")
    };
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("decoding {}", path.display()))
}

#[allow(clippy::too_many_arguments)]
pub async fn assert_runtime_init_state(
    graphql: &str,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
    expected_provider_kind: &str,
    expected_api_key: Option<&str>,
    expected_api_key_env_var: Option<&str>,
    model_name: &str,
    tool_selection_id: &str,
    expected_file_tools_mode: &str,
    expected_bash_mode: &str,
    expected_prompt_snippet: &str,
) -> Result<()> {
    use super::graphql::{first_graphql_row, graphql_query};

    let default_behavior_id = default_behavior_id_for_agent(agent_did);
    let default_profile_id = default_inference_profile_id_for_behavior(&default_behavior_id);
    let query = format!(
        r#"{{
            AgentPrincipal(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{
                agent_did
                default_behavior_id
                enabled
            }}
            AgentBehavior(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{
                behavior_id
                backend_id
                model_name
                tool_selection_id
                inference_profile_id
                system_prompt
                enabled
            }}
            InferenceProfile(filter: {{ profile_id: {{ _eq: "{}" }} }}, limit: 1) {{
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                stream_batch_ms
                deadline_duration_secs
            }}
            InferenceBackend(filter: {{ backend_id: {{ _eq: "{}" }} }}, limit: 1) {{
                backend_id
                provider_kind
                endpoint
                api_key
                api_key_env_var
                enabled
                probe_status
                models
            }}
            ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 1) {{
                selection_id
                enable_file_tools
                file_tools_mode
                enable_bash
                bash_mode
                enable_meta_tools
                allowed_mcp_service_ids
            }}
        }}"#,
        escape_graphql_string(agent_did),
        escape_graphql_string(agent_did),
        escape_graphql_string(&default_profile_id),
        escape_graphql_string(backend_id),
        escape_graphql_string(tool_selection_id),
    );
    let response = graphql_query(graphql, &query).await?;
    let principal = first_graphql_row(&response, "AgentPrincipal")?;
    let behavior = first_graphql_row(&response, "AgentBehavior")?;
    let inference_profile = first_graphql_row(&response, "InferenceProfile")?;
    let backend = first_graphql_row(&response, "InferenceBackend")?;
    let tool_selection = first_graphql_row(&response, "ToolSelection")?;

    assert_eq!(
        principal.get("agent_did").and_then(Value::as_str),
        Some(agent_did)
    );
    assert_eq!(
        principal.get("default_behavior_id").and_then(Value::as_str),
        Some(default_behavior_id.as_str())
    );
    assert_eq!(
        principal.get("enabled").and_then(Value::as_bool),
        Some(true)
    );

    assert_eq!(
        behavior.get("behavior_id").and_then(Value::as_str),
        Some(default_behavior_id.as_str())
    );
    assert_eq!(
        behavior.get("backend_id").and_then(Value::as_str),
        Some(backend_id)
    );
    assert_eq!(
        behavior.get("model_name").and_then(Value::as_str),
        Some(model_name)
    );
    assert_eq!(
        behavior.get("tool_selection_id").and_then(Value::as_str),
        Some(tool_selection_id)
    );
    assert_eq!(
        behavior.get("inference_profile_id").and_then(Value::as_str),
        Some(default_profile_id.as_str())
    );
    assert!(
        behavior
            .get("system_prompt")
            .and_then(Value::as_str)
            .is_some_and(|prompt| prompt.contains(expected_prompt_snippet)),
        "expected system_prompt to contain {expected_prompt_snippet}: {behavior}"
    );
    assert_eq!(behavior.get("enabled").and_then(Value::as_bool), Some(true));

    assert_eq!(
        inference_profile.get("profile_id").and_then(Value::as_str),
        Some(default_profile_id.as_str())
    );
    assert_eq!(
        inference_profile
            .get("display_name")
            .and_then(Value::as_str),
        Some("Default")
    );
    assert_eq!(
        inference_profile
            .get("max_output_tokens")
            .and_then(Value::as_i64),
        Some(32768)
    );

    assert_eq!(
        backend.get("backend_id").and_then(Value::as_str),
        Some(backend_id)
    );
    assert_eq!(
        backend.get("endpoint").and_then(Value::as_str),
        Some(endpoint)
    );
    assert_eq!(
        backend.get("provider_kind").and_then(Value::as_str),
        Some(expected_provider_kind)
    );
    assert_eq!(
        backend.get("api_key").and_then(Value::as_str),
        expected_api_key
    );
    assert_eq!(
        backend.get("api_key_env_var").and_then(Value::as_str),
        expected_api_key_env_var
    );
    assert_eq!(backend.get("enabled").and_then(Value::as_bool), Some(true));
    assert_eq!(
        backend.get("probe_status").and_then(Value::as_str),
        Some("healthy")
    );
    assert_eq!(
        backend.pointer("/models/0").and_then(Value::as_str),
        Some(model_name)
    );
    assert_eq!(
        tool_selection.get("selection_id").and_then(Value::as_str),
        Some(tool_selection_id)
    );
    assert_eq!(
        tool_selection
            .get("enable_file_tools")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        tool_selection
            .get("file_tools_mode")
            .and_then(Value::as_str),
        Some(expected_file_tools_mode)
    );
    assert_eq!(
        tool_selection.get("enable_bash").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        tool_selection.get("bash_mode").and_then(Value::as_str),
        Some(expected_bash_mode)
    );
    assert_eq!(
        tool_selection
            .get("enable_meta_tools")
            .and_then(Value::as_bool),
        Some(true)
    );
    // An empty MCP allowlist may be stored as `null` rather than `[]`: the
    // GraphQL writer renders empty string-lists as `null` to avoid corrupting
    // DefraDB NillableStringArray columns, and the runtime collapses both
    // `null` and `[]` to an empty `Vec` (`unwrap_or_default`), so they are
    // semantically identical. Accept null/absent/[] as "empty".
    let mcp_allowlist = tool_selection.get("allowed_mcp_service_ids");
    assert!(
        mcp_allowlist.map_or(true, |value| {
            value.is_null() || value.as_array().is_some_and(Vec::is_empty)
        }),
        "expected default tool selection MCP allowlist to be empty (null or []): {tool_selection}"
    );

    Ok(())
}
