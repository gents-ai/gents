use serde_json::{json, Map, Value};

pub const ACTION_PLAN_ABI: u32 = 1;

#[cfg(all(not(target_arch = "wasm32"), not(callback_fixture_skipped)))]
pub const WASI_COMMAND: Option<&[u8]> =
    Some(include_bytes!(env!("GENTS_CALLBACK_FIXTURE_WASI_COMMAND")));
#[cfg(all(not(target_arch = "wasm32"), callback_fixture_skipped))]
pub const WASI_COMMAND: Option<&[u8]> = None;

pub fn plan_create_workspace(
    source: &Value,
    args: &Value,
    _capabilities: &[String],
) -> Result<Value, String> {
    let work_unit_id = required_string(source, "work_unit_id")?;
    let repository_id = required_string(source, "repository_id")?;
    let base_sha = required_string(source, "base_sha")?;
    let branch = required_string(source, "branch")?;
    let workspace_id =
        optional_string(source, "workspace_id").unwrap_or_else(|| work_unit_id.clone());

    let creation_policy = match pick_string(args, source, "creation_policy").as_deref() {
        None | Some("git_worktree_diff") => "git_worktree_diff",
        Some(other) => {
            return Err(format!(
                "creation_policy `{other}` is not implemented in v1"
            ))
        }
    };
    let adapter = match pick_string(args, source, "adapter").as_deref() {
        None | Some("make_worktree") => "make_worktree",
        Some("git_worktree") => "git_worktree",
        Some(other) => return Err(format!("unknown workspace adapter `{other}`")),
    };

    let mut action = Map::new();
    action.insert(
        "path_capability".into(),
        json!({"mode": "exactPaths", "paths": []}),
    );
    action.insert("adapter".into(), Value::String(adapter.to_string()));
    action.insert("base_sha".into(), Value::String(base_sha));
    action.insert("branch".into(), Value::String(branch));
    if let Some(artifacts) = pick_string_array(args, source, "clone_artifacts") {
        action.insert("clone_artifacts".into(), Value::Array(artifacts));
    }
    action.insert(
        "creation_policy".into(),
        Value::String(creation_policy.to_string()),
    );
    action.insert("repository_id".into(), Value::String(repository_id));
    action.insert("type".into(), Value::String("create_workspace".into()));
    action.insert("work_unit_id".into(), Value::String(work_unit_id));
    action.insert("workspace_id".into(), Value::String(workspace_id));

    let mut plan = Map::new();
    plan.insert("abi".into(), json!(ACTION_PLAN_ABI));
    plan.insert("actions".into(), Value::Array(vec![Value::Object(action)]));
    Ok(Value::Object(plan))
}

pub fn plan_from_bytes(input: &[u8]) -> Result<Vec<u8>, String> {
    let value: Value = serde_json::from_slice(input)
        .map_err(|error| format!("planner input is not JSON: {error}"))?;
    if value.get("deployment_id").is_some() {
        return Err("planner input must not include deployment_id".into());
    }
    let source = value
        .get("source")
        .ok_or_else(|| "planner input missing `source`".to_string())?;
    let args = value.get("args").cloned().unwrap_or_else(|| json!({}));
    let capabilities = match value.get("capabilities") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        Some(_) => return Err("planner input `capabilities` must be an array".into()),
    };
    let plan = plan_create_workspace(source, &args, &capabilities)?;
    serde_json::to_vec(&plan).map_err(|error| format!("serialize ActionPlan: {error}"))
}

fn pick<'a>(args: &'a Value, source: &'a Value, field: &str) -> Option<&'a Value> {
    args.get(field)
        .filter(|value| !value.is_null())
        .or_else(|| source.get(field).filter(|value| !value.is_null()))
}

fn pick_string(args: &Value, source: &Value, field: &str) -> Option<String> {
    pick(args, source, field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn pick_string_array(args: &Value, source: &Value, field: &str) -> Option<Vec<Value>> {
    pick(args, source, field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|value| Value::String(value.to_string()))
                })
                .collect()
        })
}

fn required_string(source: &Value, field: &str) -> Result<String, String> {
    optional_string(source, field).ok_or_else(|| format!("source document missing `{field}`"))
}

fn optional_string(source: &Value, field: &str) -> Option<String> {
    source
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
