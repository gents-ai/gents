//! Tauri commands for operator-surfaces panels. Stubs return an `Err`
//! describing the panel issue that will replace them; real implementations
//! land via their own panel PRs. The MCP-health commands (panel #278) are
//! live.

use std::time::Duration;

use defra_agent::backend_registry::{derive_display_state, list_all_backends};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use reqwest::Url;
use tauri::State;

use super::super::commands::{load_mcp_services_with_health, probe_mcp_service};
use super::super::state::{current_core, DesktopAppState};
use super::super::types::{
    BackendHealthView, CascadeCancelPreview, DesktopInterruptRequest,
    DesktopListSubagentTreeRequest, DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest,
    DesktopPreviewInterruptCascadeRequest, DesktopProbeMcpServiceRequest,
    InferenceCallSummaryView, InterruptRequestResult, MCPServiceHealthView, McpServiceProbeResult,
    SubagentTreeView,
};

const SUBAGENT_TREE_TIMEOUT: Duration = Duration::from_secs(10);

/// Number of most-recent `InferenceCall` rows surfaced per backend in the
/// health panel. Picked to give the operator enough history to see a
/// pattern (e.g. consecutive `QueueFull` rejections) without overwhelming
/// the row's expanded detail.
const RECENT_CALLS_PER_BACKEND: usize = 10;

#[tauri::command]
pub(crate) async fn desktop_operations_snapshot(
    _state: State<'_, DesktopAppState>,
    _request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot, String> {
    Err(
        "desktop_operations_snapshot not implemented yet; landing via panel #277 \
         (backgrounded tools / operations projection)"
            .to_string(),
    )
}

#[tauri::command]
pub(crate) async fn desktop_list_subagent_tree(
    state: State<'_, DesktopAppState>,
    request: DesktopListSubagentTreeRequest,
) -> Result<SubagentTreeView, String> {
    let root_request_id = request.root_request_id.trim();
    if root_request_id.is_empty() {
        return Err("rootRequestId is required".to_string());
    }
    let core = current_core(&state)
        .ok_or_else(|| "desktop bridge has not finished bootstrapping".to_string())?;

    let agent_did = match request.agent_did.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => core
            .selected_agent_did()
            .ok_or_else(|| "no agent selected; pass agentDid explicitly".to_string())?,
    };

    let graphql = core
        .graphql_for_agent(&agent_did)
        .await
        .ok_or_else(|| format!("no graphql URL configured for agent {agent_did}"))?;
    let url = subagent_tree_url(&graphql, root_request_id, &request)?;

    let client = reqwest::Client::builder()
        .timeout(SUBAGENT_TREE_TIMEOUT)
        .build()
        .map_err(|error| format!("build subagent tree http client: {error}"))?;

    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| format!("subagent tree fetch failed: {error}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "subagent tree fetch returned {status}: {}",
            body.trim()
        ));
    }

    response
        .json::<SubagentTreeView>()
        .await
        .map_err(|error| format!("decode subagent tree response: {error}"))
}

/// Translate the agent's GraphQL URL into the runtime's `/subagents/tree`
/// endpoint URL. Mirrors the path-stripping logic in
/// `defra_agent_desktop_core::local_runtime::runtime_status_url` but targets
/// the R5 subagent-lineage handler.
fn subagent_tree_url(
    graphql: &str,
    root_request_id: &str,
    request: &DesktopListSubagentTreeRequest,
) -> Result<Url, String> {
    let trimmed = graphql.trim();
    if trimmed.is_empty() {
        return Err("agent graphql URL is empty".to_string());
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme)
        .map_err(|error| format!("agent graphql URL is not a valid URL: {error}"))?;
    let path = url.path().trim_end_matches('/').to_string();
    if path.is_empty() || path == "/api/v0" || path == "/api/v0/graphql" {
        url.set_path("/subagents/tree");
    } else if !path.ends_with("/subagents/tree") {
        url.set_path(&format!("{path}/subagents/tree"));
    }
    url.set_query(None);
    url.set_fragment(None);

    let mut pairs = url.query_pairs_mut();
    pairs.append_pair("root_request_id", root_request_id);
    if let Some(include_terminal) = request.include_terminal {
        pairs.append_pair("include_terminal", &include_terminal.to_string());
    }
    if let Some(max_depth) = request.max_depth {
        pairs.append_pair("max_depth", &max_depth.to_string());
    }
    drop(pairs);
    Ok(url)
}

#[cfg(test)]
mod subagent_tree_url_tests {
    use super::*;

    fn request(include_terminal: Option<bool>, max_depth: Option<u32>) -> DesktopListSubagentTreeRequest {
        DesktopListSubagentTreeRequest {
            root_request_id: "req-root".to_string(),
            agent_did: None,
            include_terminal,
            max_depth,
        }
    }

    #[test]
    fn strips_graphql_path_and_appends_subagents_tree() {
        let url = subagent_tree_url(
            "http://127.0.0.1:9181/api/v0/graphql",
            "req-root",
            &request(None, None),
        )
        .unwrap();
        assert_eq!(url.path(), "/subagents/tree");
        assert!(url.query().unwrap().contains("root_request_id=req-root"));
    }

    #[test]
    fn accepts_bare_host_and_defaults_scheme() {
        let url = subagent_tree_url("127.0.0.1:9181", "req-root", &request(Some(true), Some(4)))
            .unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.path(), "/subagents/tree");
        let query = url.query().unwrap();
        assert!(query.contains("include_terminal=true"));
        assert!(query.contains("max_depth=4"));
    }

    #[test]
    fn preserves_remote_host_and_port() {
        let url = subagent_tree_url(
            "https://runtime.example.com:8443/api/v0/graphql",
            "req-root",
            &request(None, None),
        )
        .unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("runtime.example.com"));
        assert_eq!(url.port(), Some(8443));
        assert_eq!(url.path(), "/subagents/tree");
    }

    #[test]
    fn rejects_empty_graphql_url() {
        let err = subagent_tree_url("   ", "req-root", &request(None, None)).unwrap_err();
        assert!(err.contains("empty"));
    }
}

#[tauri::command]
pub(crate) async fn desktop_preview_interrupt_cascade(
    _state: State<'_, DesktopAppState>,
    _request: DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, String> {
    Err(
        "desktop_preview_interrupt_cascade not implemented yet; landing via panel #286 \
         (cascade cancel UX)"
            .to_string(),
    )
}

#[tauri::command]
pub(crate) async fn desktop_interrupt_request(
    _state: State<'_, DesktopAppState>,
    _request: DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    Err(
        "desktop_interrupt_request not implemented yet; landing via panel #283 \
         (interrupt button)"
            .to_string(),
    )
}

<<<<<<< HEAD
#[tauri::command]
pub(crate) async fn desktop_list_backends_with_health(
    state: State<'_, DesktopAppState>,
) -> Result<Vec<BackendHealthView>, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let node = core.node();
    let backends = list_all_backends(node).await.map_err(|err| err.to_string())?;

    let mut views = Vec::with_capacity(backends.len());
    for backend in backends {
        let recent_calls = fetch_recent_calls(node, &backend.backend_id)
            .await
            .map_err(|err| err.to_string())?;
        let display_state = derive_display_state(backend.enabled, &backend.probe_status).to_string();
        views.push(BackendHealthView {
            backend_id: backend.backend_id,
            name: backend.name,
            provider_kind: backend.provider_kind.as_str().to_string(),
            endpoint: backend.endpoint,
            enabled: backend.enabled,
            probe_status: backend.probe_status,
            display_state,
            // `last_probe` is a DateTime on the schema but not currently
            // surfaced via `InferenceBackend::from_value`. Returning None
            // keeps the wire shape stable for a follow-up that exposes
            // probe metadata; the panel renders "never" when absent.
            last_probe: None,
            max_concurrent: backend.max_concurrent,
            max_queue_depth: backend.max_queue_depth,
            models: backend.models,
            recent_calls,
        });
    }
    Ok(views)
}

async fn fetch_recent_calls(
    node: &EmbeddedNode,
    backend_id: &str,
) -> Result<Vec<InferenceCallSummaryView>, anyhow::Error> {
    let escaped_id = escape_graphql_string(backend_id);
    let query = format!(
        r#"query {{
            InferenceCall(
                filter: {{ backend_id: {{ _eq: "{escaped_id}" }} }},
                order: {{ queued_at: DESC }},
                limit: {limit}
            ) {{
                call_id
                call_seq
                call_kind
                call_state
                failure_reason
                queued_at
                started_at
                ended_at
                queue_depth_at_enqueue
                prompt_tokens
                completion_tokens
            }}
        }}"#,
        limit = RECENT_CALLS_PER_BACKEND,
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "list InferenceCall for backend {backend_id} failed: {:?}",
            resp.errors
        );
    }

    Ok(resp
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(|value| value.as_array())
        .map(|rows| rows.iter().map(parse_call_row).collect::<Vec<_>>())
        .unwrap_or_default())
}

fn parse_call_row(row: &serde_json::Value) -> InferenceCallSummaryView {
    InferenceCallSummaryView {
        call_id: row
            .get("call_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        call_seq: row.get("call_seq").and_then(|v| v.as_i64()).unwrap_or(0),
        call_kind: row
            .get("call_kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        call_state: row
            .get("call_state")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        failure_reason: row
            .get("failure_reason")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        queued_at: row
            .get("queued_at")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        started_at: row
            .get("started_at")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        ended_at: row
            .get("ended_at")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        queue_depth_at_enqueue: row.get("queue_depth_at_enqueue").and_then(|v| v.as_i64()),
        prompt_tokens: row.get("prompt_tokens").and_then(|v| v.as_i64()),
        completion_tokens: row.get("completion_tokens").and_then(|v| v.as_i64()),
    }
}

/// Panel #278: returns the persisted `ToolServiceHealthState` rows the
/// agent writes every health-check cycle. The desktop renders these into
/// the MCP health status panel — see `apps/desktop-tauri/src/components/mcpHealth/`.
#[tauri::command]
pub(crate) async fn desktop_list_mcp_services_with_health(
    state: State<'_, DesktopAppState>,
) -> Result<Vec<MCPServiceHealthView>, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };
    load_mcp_services_with_health(core.as_ref())
        .await
        .map_err(|error| error.to_string())
}

/// Panel #278: kicks off a one-shot probe of a single registered MCP
/// service and returns the resulting `ProbeResult`. K-state is not
/// updated by this call — the running agent's health checker remains the
/// authority for `failure_count` / `backoff_until` / persisted state.
#[tauri::command]
pub(crate) async fn desktop_probe_mcp_service(
    state: State<'_, DesktopAppState>,
    request: DesktopProbeMcpServiceRequest,
) -> Result<McpServiceProbeResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };
    probe_mcp_service(core.as_ref(), &request.service_id)
        .await
        .map_err(|error| error.to_string())
}
