use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use defra_agent::{discover_backend_models, BackendProviderKind};
use serde_json::{json, Value};

use crate::cli::args::{DiagnoseArgs, P2pTransportArg, ToolCeilingArg};
use crate::config_writes::ConfigAccess;
use crate::shared::{ConfigExportBundle, StoredInitConfig};
use crate::{
    build_config_export_bundle, graphql_endpoint_available, print_json, read_init_config,
    read_runtime_state, resolve_agent_did, resolve_config_access, resolve_home_dir,
    CONFIG_EXPORT_FORMAT, CONFIG_SCHEMA_COLLECTIONS, SCHEMA_COLLECTION_CHECKS,
};

pub(crate) async fn diagnose(args: DiagnoseArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let init_config = read_init_config(&home_dir)?;
    let runtime_state = read_runtime_state(&home_dir)?;
    let graphql = args
        .graphql
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.graphql.clone()));
    let graphql_reachable = match graphql.as_deref() {
        Some(endpoint) => graphql_endpoint_available(endpoint).await,
        None => false,
    };
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;

    let schema_checks = diagnose_schema_presence(&access).await;
    let bundle_result = build_config_export_bundle(&access, &agent_did).await;
    let config_load_error = bundle_result.as_ref().err().map(ToString::to_string);
    let bundle = bundle_result.unwrap_or_else(|_| ConfigExportBundle {
        format: CONFIG_EXPORT_FORMAT.to_string(),
        agent_did: agent_did.clone(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        access_mode: access.mode().to_string(),
        agent_principal: None,
        agent_behaviors: Vec::new(),
        tool_selections: Vec::new(),
        inference_backends: Vec::new(),
        inference_profiles: Vec::new(),
        tool_service_registries: Vec::new(),
        scheduled_tasks: Vec::new(),
    });
    let runtime_row = match load_runtime_row(&access, &agent_did).await {
        Ok(Some(row)) => row,
        Ok(None) => Value::Null,
        Err(error) => json!({
            "error": error.to_string(),
        }),
    };

    let behavior_ids = bundle
        .agent_behaviors
        .iter()
        .filter_map(|row| {
            row.get("behavior_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<std::collections::BTreeSet<_>>();
    let default_behavior_id = bundle
        .agent_principal
        .as_ref()
        .and_then(|row| row.get("default_behavior_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let default_behavior_check = match default_behavior_id.as_deref() {
        Some(behavior_id) if behavior_ids.contains(behavior_id) => json!({
            "ok": true,
            "default_behavior_id": behavior_id,
        }),
        Some(behavior_id) => json!({
            "ok": false,
            "default_behavior_id": behavior_id,
            "error": format!("default behavior {} is not present in AgentBehavior documents", behavior_id),
        }),
        None => json!({
            "ok": false,
            "error": format!("AgentPrincipal {} is missing or has no default_behavior_id", agent_did),
        }),
    };
    let tool_ceiling_check = diagnose_tool_ceiling(init_config.as_ref());
    let backend_reports = diagnose_backends(&bundle).await;
    let matching_runtime_state = runtime_state.as_ref().filter(|state| {
        graphql
            .as_deref()
            .is_some_and(|endpoint| endpoint == state.graphql)
    });
    let p2p_status = match graphql.as_deref().filter(|_| graphql_reachable) {
        Some(endpoint) => {
            crate::commands::p2p::load_live_http_p2p_status(args.home.as_deref(), endpoint).await
        }
        None => crate::commands::p2p::persisted_p2p_status(matching_runtime_state),
    };
    let p2p_transport = p2p_status
        .get("p2p_transport")
        .and_then(Value::as_str)
        .unwrap_or(P2pTransportArg::None.as_str());
    let p2p_peer_id = p2p_status
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let p2p_connected_peers = p2p_status
        .get("p2p_connected_peers")
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    let p2p_error = p2p_status
        .get("p2p_error")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let p2p_ok = if p2p_transport == P2pTransportArg::None.as_str() {
        true
    } else {
        p2p_peer_id.is_some() && p2p_error.is_none()
    };
    let schemas_ok = schema_checks
        .iter()
        .filter(|check| check.get("required_for_config").and_then(Value::as_bool) == Some(true))
        .all(|check| check.get("ok").and_then(Value::as_bool) == Some(true));
    let backends_ok = backend_reports
        .iter()
        .all(|check| check.get("ok").and_then(Value::as_bool) == Some(true));
    let default_behavior_ok = default_behavior_check
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tool_ceiling_ok = tool_ceiling_check
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let principal_present = bundle.agent_principal.is_some();
    let status = if schemas_ok
        && principal_present
        && default_behavior_ok
        && tool_ceiling_ok
        && backends_ok
        && p2p_ok
        && config_load_error.is_none()
    {
        "ok"
    } else {
        "degraded"
    };

    let mut output = json!({
        "status": status,
        "home": home_dir,
        "agent_did": agent_did,
        "access_mode": access.mode(),
        "graphql": graphql,
        "graphql_reachable": graphql_reachable,
        "runtime": runtime_row,
        "p2p": p2p_status,
        "checks": {
            "schemas": schema_checks,
            "config_documents_loadable": {
                "ok": config_load_error.is_none(),
                "error": config_load_error,
            },
            "agent_principal_present": principal_present,
            "default_behavior": default_behavior_check,
            "tool_ceiling": tool_ceiling_check,
            "backends": backend_reports,
            "p2p": {
                "ok": p2p_ok,
                "transport": p2p_transport,
                "peer_id": p2p_peer_id,
                "connected_peer_count": p2p_connected_peers,
                "error": p2p_error,
            },
        },
        "config_counts": {
            "agent_behaviors": bundle.agent_behaviors.len(),
            "tool_selections": bundle.tool_selections.len(),
            "inference_backends": bundle.inference_backends.len(),
            "inference_profiles": bundle.inference_profiles.len(),
            "tool_service_registries": bundle.tool_service_registries.len(),
            "scheduled_tasks": bundle.scheduled_tasks.len(),
        },
    });
    if let Some(map) = output.as_object_mut() {
        let p2p_value = map.get("p2p").cloned().unwrap_or(Value::Null);
        crate::commands::p2p::flatten_p2p_fields(map, &p2p_value);
    }
    print_json(&output)?;
    Ok(())
}

async fn load_runtime_row(access: &ConfigAccess, agent_did: &str) -> Result<Option<Value>> {
    use defra_agent::graphql::escape_graphql_string;
    let query = format!(
        r#"{{
            AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                agent_did
                process_state
                reconcile_phase
                active_generation
                router_generation
                default_behavior_id
                runnable_behavior_count
                unavailable_behavior_count
                last_reconcile_result
                last_reconcile_error
                last_reconcile_completed_at
                updated_at
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    Ok(crate::graphql_rows(access, "AgentRuntime", &query)
        .await?
        .into_iter()
        .next())
}

async fn diagnose_schema_presence(access: &ConfigAccess) -> Vec<Value> {
    let mut results = Vec::new();
    for (collection, field) in SCHEMA_COLLECTION_CHECKS {
        let required_for_config = CONFIG_SCHEMA_COLLECTIONS.contains(collection);
        let query = format!(
            r#"{{ {collection}(limit: 1) {{ {field} }} }}"#,
            collection = collection,
            field = field
        );
        match access.execute(&query).await {
            Ok(_) => results.push(json!({
                "collection": collection,
                "required_for_config": required_for_config,
                "ok": true,
            })),
            Err(error) => results.push(json!({
                "collection": collection,
                "required_for_config": required_for_config,
                "ok": false,
                "error": error.to_string(),
            })),
        }
    }
    results
}

fn diagnose_tool_ceiling(init_config: Option<&StoredInitConfig>) -> Value {
    match init_config {
        Some(config) => {
            let tool_root = config.tool_root.as_deref();
            let ok = match config.tool_ceiling {
                ToolCeilingArg::Readonly | ToolCeilingArg::Readwrite => tool_root
                    .map(Path::new)
                    .map(|path| path.is_dir())
                    .unwrap_or(false),
                ToolCeilingArg::MetaOnly => true,
            };
            let error = if ok {
                None
            } else {
                Some(
                    "readonly/readwrite tool ceiling requires an existing tool_root directory"
                        .to_string(),
                )
            };
            json!({
                "ok": ok,
                "tool_ceiling": format_tool_ceiling(config.tool_ceiling),
                "tool_root": config.tool_root,
                "error": error,
            })
        }
        None => json!({
            "ok": true,
            "error": null,
            "note": "no local init.json found; tool ceiling is unknown until `defra-agent init` runs"
        }),
    }
}

async fn diagnose_backends(bundle: &ConfigExportBundle) -> Vec<Value> {
    let mut models_by_backend = std::collections::BTreeMap::<String, Vec<String>>::new();
    for behavior in &bundle.agent_behaviors {
        let Some(backend_id) = behavior.get("backend_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(model_name) = behavior.get("model_name").and_then(Value::as_str) else {
            continue;
        };
        if backend_id.trim().is_empty() || model_name.trim().is_empty() {
            continue;
        }
        models_by_backend
            .entry(backend_id.to_string())
            .or_default()
            .push(model_name.to_string());
    }
    for models in models_by_backend.values_mut() {
        models.sort();
        models.dedup();
    }

    let mut reports = Vec::new();
    let present_backend_ids = bundle
        .inference_backends
        .iter()
        .filter_map(|backend| backend.get("backend_id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    for backend in &bundle.inference_backends {
        reports.push(
            diagnose_backend(
                backend,
                models_by_backend
                    .get(
                        backend
                            .get("backend_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    )
                    .cloned()
                    .unwrap_or_default(),
            )
            .await,
        );
    }
    for backend_id in models_by_backend.keys() {
        if !present_backend_ids.contains(backend_id) {
            reports.push(json!({
                "backend_id": backend_id,
                "ok": false,
                "error": format!("referenced backend {} is missing", backend_id),
                "required_models": models_by_backend.get(backend_id).cloned().unwrap_or_default(),
            }));
        }
    }
    reports.sort_by(|left, right| {
        let left_key = left
            .get("backend_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let right_key = right
            .get("backend_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        left_key.cmp(right_key)
    });
    reports
}

async fn diagnose_backend(backend: &Value, required_models: Vec<String>) -> Value {
    let backend_id = backend
        .get("backend_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let provider_kind = match BackendProviderKind::parse_optional(
        backend.get("provider_kind").and_then(Value::as_str),
    ) {
        Ok(kind) => kind,
        Err(error) => {
            return json!({
                "backend_id": backend_id,
                "ok": false,
                "provider_kind": backend.get("provider_kind"),
                "error": error.to_string(),
            });
        }
    };
    let endpoint = backend
        .get("endpoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let enabled = backend
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let probe_status = backend
        .get("probe_status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let api_key_env_var = backend
        .get("api_key_env_var")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let raw_api_key = backend
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let mut ok = enabled && probe_status == "healthy";
    let mut error = None::<String>;
    let mut discovered_models = Vec::<String>::new();

    let api_key = match (raw_api_key.as_ref(), api_key_env_var.as_deref()) {
        (Some(raw), Some(name)) => {
            ok = false;
            error = Some(format!(
                "backend {} sets both raw api_key and api_key_env_var {}",
                backend_id, name
            ));
            Some(raw.clone())
        }
        (Some(raw), None) => Some(raw.clone()),
        (None, Some(name)) => match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => Some(value),
            _ => {
                ok = false;
                error = Some(format!(
                    "required backend API key env var {} is not set",
                    name
                ));
                None
            }
        },
        (None, None) => None,
    };

    if ok {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(client) => client,
            Err(build_error) => {
                ok = false;
                error = Some(format!("building probe client: {build_error}"));
                return json!({
                    "backend_id": backend_id,
                    "ok": ok,
                    "provider_kind": provider_kind.as_str(),
                    "endpoint": endpoint,
                    "enabled": enabled,
                    "probe_status": probe_status,
                    "api_key": raw_api_key.as_ref().map(|_| "<redacted>"),
                    "api_key_env_var": api_key_env_var,
                    "required_models": required_models,
                    "discovered_models": discovered_models,
                    "error": error,
                });
            }
        };
        match discover_backend_models(&client, provider_kind, &endpoint, api_key.as_deref()).await {
            Ok(models) => {
                discovered_models = models;
                let missing_models = required_models
                    .iter()
                    .filter(|model| {
                        !discovered_models
                            .iter()
                            .any(|candidate| candidate == *model)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if !missing_models.is_empty() {
                    ok = false;
                    error = Some(format!(
                        "backend {} is missing required models: {}",
                        backend_id,
                        missing_models.join(", ")
                    ));
                }
            }
            Err(request_error) => {
                ok = false;
                error = Some(format!("backend discovery failed: {}", request_error));
            }
        }
    }

    json!({
        "backend_id": backend_id,
        "ok": ok,
        "provider_kind": provider_kind.as_str(),
        "endpoint": endpoint,
        "enabled": enabled,
        "probe_status": probe_status,
        "api_key": raw_api_key.as_ref().map(|_| "<redacted>"),
        "api_key_env_var": api_key_env_var,
        "required_models": required_models,
        "discovered_models": discovered_models,
        "error": error,
    })
}

fn format_tool_ceiling(value: ToolCeilingArg) -> &'static str {
    match value {
        ToolCeilingArg::MetaOnly => "meta-only",
        ToolCeilingArg::Readonly => "readonly",
        ToolCeilingArg::Readwrite => "readwrite",
    }
}
