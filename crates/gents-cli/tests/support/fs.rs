use crate::support::TestResult;
use std::fs;
use std::path::Path;

use gents::{default_agent_id_for_node, default_inference_profile_id_for_agent};
use serde_json::Value;

use super::graphql::escape_graphql_string;

pub fn copy_dir_all(from: &Path, to: &Path) -> TestResult<()> {
    fs::create_dir_all(to).map_err(|error| format!("creating {}: {error:#}", to.display()))?;
    for entry in
        fs::read_dir(from).map_err(|error| format!("reading {}: {error:#}", from.display()))?
    {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)
                .map_err(|error| format!("copying {}: {error:#}", entry.path().display()))?;
        }
    }
    Ok(())
}

pub fn write_json_file(path: &Path, value: &Value) -> TestResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!("creating parent directory {}: {error:#}", parent.display())
        })?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .map_err(|error| format!("writing JSON file {}: {error:#}", path.display()))?;
    Ok(())
}

pub fn read_json_file(path: &Path) -> TestResult<Value> {
    let bytes = fs::read(path)
        .map_err(|error| format!("reading JSON file {}: {error:#}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("decoding JSON file {}: {error:#}", path.display()).into())
}

pub fn rewrite_manifest_node_dids(root: &Path, node_did: &str) -> TestResult<()> {
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    config["node"]["node_did"] = Value::String(node_did.to_string());
    if let Some(object) = config.as_object_mut() {
        for value in object.values_mut() {
            let Some(rows) = value.as_array_mut() else {
                continue;
            };
            for row in rows {
                if row.get("node_did").is_some() {
                    row["node_did"] = Value::String(node_did.to_string());
                }
            }
        }
    }
    write_json_file(&path, &config)?;
    Ok(())
}

pub fn assert_manifest_node_dids(root: &Path, expected_node_did: &str) -> TestResult<()> {
    let config = read_json_file(&root.join("pack_config.json"))?;
    let principal = &config["node"];
    assert_eq!(
        principal.get("node_did").and_then(Value::as_str),
        Some(expected_node_did)
    );

    if let Some(collections) = config.as_object() {
        for (collection, value) in collections {
            let Some(rows) = value.as_array() else {
                continue;
            };
            for object in rows {
                if object.get("node_did").is_none() {
                    continue;
                }
                assert_eq!(
                    object.get("node_did").and_then(Value::as_str),
                    Some(expected_node_did),
                    "wrong node_did in {collection}"
                );
            }
        }
    }

    Ok(())
}

pub fn manifest_contains(root: &Path, needle: &str) -> TestResult<bool> {
    fn visit(path: &Path, needle: &str) -> TestResult<bool> {
        if path.is_dir() {
            for entry in fs::read_dir(path)
                .map_err(|error| format!("reading {}: {error:#}", path.display()))?
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
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("reading {}: {error:#}", path.display()))?;
        Ok(contents.contains(needle))
    }

    visit(root, needle)
}

pub fn read_captured_log(log: Option<&tempfile::NamedTempFile>) -> TestResult<String> {
    let Some(log) = log else {
        return Ok(String::new());
    };
    let bytes = fs::read(log.path())
        .map_err(|error| format!("reading captured log {}: {error:#}", log.path().display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn write_manifest_root_from_export(root: &Path, exported: &Value) -> TestResult<()> {
    let mut config = exported.clone();
    let object = config
        .as_object_mut()
        .ok_or_else(|| "exported configuration is not an object".to_string())?;
    for metadata in ["format", "node_did", "exported_at", "access_mode"] {
        object.remove(metadata);
    }
    write_json_file(&root.join("pack_config.json"), &config)
}

pub fn project_object_fields(value: &Value, fields: &[&str]) -> TestResult<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("expected object while projecting manifest fields: {value}"))?;
    let mut projected = serde_json::Map::new();
    for field in fields {
        if let Some(value) = object.get(*field) {
            projected.insert((*field).to_string(), value.clone());
        }
    }
    Ok(Value::Object(projected))
}

pub fn read_runtime_state_json(home_dir: &Path) -> TestResult<Value> {
    let path = if home_dir.join("runtime.json").exists() {
        home_dir.join("runtime.json")
    } else {
        home_dir.join(".gents").join("runtime.json")
    };
    let bytes =
        fs::read(&path).map_err(|error| format!("reading {}: {error:#}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("decoding {}: {error:#}", path.display()).into())
}

#[allow(clippy::too_many_arguments)]
pub async fn assert_runtime_init_state(
    graphql: &str,
    node_did: &str,
    backend_id: &str,
    endpoint: &str,
    expected_provider_kind: &str,
    expected_api_key: Option<&str>,
    expected_api_key_env_var: Option<&str>,
    model_name: &str,
    tools_id: &str,
    expected_file_tools_mode: &str,
    expected_bash_mode: &str,
    expected_prompt_snippet: &str,
) -> TestResult<()> {
    use super::graphql::{first_graphql_row, graphql_query};

    let default_agent_id = default_agent_id_for_node(node_did);
    let default_profile_id = default_inference_profile_id_for_agent(&default_agent_id);
    let query = format!(
        r#"{{
            Node(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 1) {{
                node_did
                default_agent_id
                enabled
            }}
            Agent(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 1) {{
                agent_id
                context_id
                inference_profile_id
                enabled
            }}
            AgentContext(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 1) {{
                context_id
                system_prompt
                tools_id
            }}
            InferenceProfile(filter: {{ profile_id: {{ _eq: "{}" }} }}, limit: 1) {{
                profile_id
                display_name
                backend_id
                model_name
                max_output_tokens
            }}
            InferenceBackend(filter: {{ backend_id: {{ _eq: "{}" }} }}, limit: 1) {{
                backend_id
                provider_kind
                endpoint
                auth
                enabled
                probe_status
            }}
            Tools(filter: {{ node_did: {{ _eq: "{}" }}, tools_id: {{ _eq: "{}" }} }}, limit: 1) {{
                tools_id node_did host remote agents built_ins datastore integrations self_config tags
            }}
        }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(node_did),
        escape_graphql_string(node_did),
        escape_graphql_string(&default_profile_id),
        escape_graphql_string(backend_id),
        escape_graphql_string(node_did),
        escape_graphql_string(tools_id),
    );
    let response = graphql_query(graphql, &query).await?;
    let principal = first_graphql_row(&response, "Node")?;
    let agent = first_graphql_row(&response, "Agent")?;
    let context = first_graphql_row(&response, "AgentContext")?;
    let inference_profile = first_graphql_row(&response, "InferenceProfile")?;
    let backend = first_graphql_row(&response, "InferenceBackend")?;
    let tools = first_graphql_row(&response, "Tools")?;

    assert_eq!(
        principal.get("node_did").and_then(Value::as_str),
        Some(node_did)
    );
    assert_eq!(
        principal.get("default_agent_id").and_then(Value::as_str),
        Some(default_agent_id.as_str())
    );
    assert_eq!(
        principal.get("enabled").and_then(Value::as_bool),
        Some(true)
    );

    assert_eq!(
        agent.get("agent_id").and_then(Value::as_str),
        Some(default_agent_id.as_str())
    );
    assert_eq!(
        agent.get("inference_profile_id").and_then(Value::as_str),
        Some(default_profile_id.as_str())
    );
    assert!(
        context
            .get("system_prompt")
            .and_then(Value::as_str)
            .is_some_and(|prompt| prompt.contains(expected_prompt_snippet)),
        "expected system_prompt to contain {expected_prompt_snippet}: {context}"
    );
    assert_eq!(
        context.get("tools_id").and_then(Value::as_str),
        Some(tools_id)
    );
    assert_eq!(agent.get("enabled").and_then(Value::as_bool), Some(true));

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
        inference_profile.get("backend_id").and_then(Value::as_str),
        Some(backend_id)
    );
    assert_eq!(
        inference_profile.get("model_name").and_then(Value::as_str),
        Some(model_name)
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
    let auth = backend.get("auth").cloned().unwrap_or(Value::Null);
    let auth = auth
        .as_str()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or(auth);
    match (expected_api_key, expected_api_key_env_var) {
        (Some(value), _) => assert_eq!(auth.get("key").and_then(Value::as_str), Some(value)),
        (_, Some(name)) => assert_eq!(auth.get("variable").and_then(Value::as_str), Some(name)),
        _ => assert_eq!(
            auth.get("kind").and_then(Value::as_str),
            Some("unauthenticated")
        ),
    }
    assert_eq!(backend.get("enabled").and_then(Value::as_bool), Some(true));
    assert_eq!(
        backend.get("probe_status").and_then(Value::as_str),
        Some("healthy")
    );
    assert_eq!(
        tools.get("tools_id").and_then(Value::as_str),
        Some(tools_id)
    );
    assert_eq!(
        tools.pointer("/host/files/mode").and_then(Value::as_str),
        Some(expected_file_tools_mode)
    );
    assert_eq!(
        tools.pointer("/host/bash/mode").and_then(Value::as_str),
        Some(expected_bash_mode)
    );

    Ok(())
}

pub fn workspace_root() -> TestResult<std::path::PathBuf> {
    Ok(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| "unable to resolve workspace root".to_string())?)
}

pub fn read_workspace_json(relative_path: &str) -> TestResult<Value> {
    read_json_file(&workspace_root()?.join(relative_path))
}

pub fn parse_jsonl(output: &str, label: &str) -> TestResult<Vec<Value>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .map_err(|error| format!("parsing {label} line: {error:#}").into())
        })
        .collect()
}

pub fn assert_json_schema_valid(schema: &Value, instance: &Value, label: &str) -> TestResult<()> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|error| format!("compiling {label} schema: {error:#}"))?;
    let errors = validator
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{label} failed JSON Schema validation:\n{}",
        errors.join("\n")
    );
    Ok(())
}
