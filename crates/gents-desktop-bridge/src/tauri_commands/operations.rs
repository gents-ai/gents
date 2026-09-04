use crate::error::BridgeError;

use std::sync::Arc;

use chrono::Utc;
use gents::backend_registry::{derive_display_state, list_all_backends};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::subagent_tree::{
    build_subagent_tree, effective_subagent_tree_max_depth, SubagentTree, SubagentTreeAccess,
};
use gents_desktop_core::client::ClientCore;
#[cfg(test)]
use reqwest::Url;
use tauri::State;

use crate::commands::mcp_health::{load_mcp_services_with_health, probe_mcp_service};
use crate::snapshot::operations_snapshot::{
    project_backgrounded_tools, stuck_diagnostics_from_tool_calls, ToolCallRow,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    BackendHealthView, CascadeCancelPreview, DesktopInterruptRequest, DesktopListHoldsRequest,
    DesktopListSubagentTreeRequest, DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest,
    DesktopPreviewInterruptCascadeRequest, DesktopProbeMcpServiceRequest,
    DesktopResolveHoldRequest, HeldToolCallView, InferenceCallSummaryView, InterruptRequestResult,
    MCPServiceHealthView, McpServiceProbeResult, NativeExecutorStatusView, ResolveHoldResult,
    RuntimeLivenessView, SubagentEdgeView, SubagentNodeView, SubagentTreeView,
};

const BACKGROUND_TOOL_CALL_LIMIT: usize = 256;

const RECENT_CALLS_PER_BACKEND: usize = 10;

#[tauri::command]
pub async fn desktop_operations_snapshot(
    state: State<'_, DesktopAppState>,
    request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge not initialized"))?;
    let agent_did = request
        .agent_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| core.selected_agent_did())
        .ok_or_else(|| BridgeError::untyped("no agent selected; pass agentDid explicitly"))?;

    let native_executors: Vec<NativeExecutorStatusView> =
        gents::native_executor_status::active_native_executors()
            .into_iter()
            .map(|ne| NativeExecutorStatusView {
                id: ne.id as i64,
                pid: ne.pid as u32,
                argv0: ne.argv0,
                tool_name: ne.tool_name,
                started_at: ne.started_at,
                age_ms: ne.age_ms,
            })
            .collect();

    let tool_call_rows = fetch_background_tool_calls(&core, &agent_did)
        .await
        .map_err(|e| format!("failed to query AgentToolCall: {e}"))?;

    let liveness = RuntimeLivenessView {
        expired_processing_count: 0,
        requests: Vec::new(),
        active_tool_calls: Vec::new(),
        active_native_executors_available: true,
        active_native_executors: native_executors,
    };

    let backgrounded_tools = project_backgrounded_tools(&tool_call_rows, &liveness);
    let stuck_diagnostics = stuck_diagnostics_from_tool_calls(&tool_call_rows);

    Ok(DesktopOperationsSnapshot {
        fetched_at: Utc::now().to_rfc3339(),
        agent_did: Some(agent_did),
        liveness: Some(liveness),
        liveness_unavailable_reason: None,
        backgrounded_tools,
        stuck_diagnostics,
        lineage: None,
    })
}

async fn fetch_background_tool_calls(
    core: &Arc<ClientCore>,
    agent_did: &str,
) -> Result<Vec<ToolCallRow>, BridgeError> {
    let query = background_tool_calls_query(agent_did);

    let response = core.node().execute(&query).await;
    if response.has_errors() {
        return Err(response
            .errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("; ")
            .into());
    }

    let data = response
        .data
        .ok_or_else(|| BridgeError::untyped("AgentToolCall query returned no data"))?;
    let rows = data
        .get("AgentToolCall")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .map(|row| ToolCallRow {
            request_id: row
                .get("request_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tool_call_id: row
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tool_name: row
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            lifecycle_state: row
                .get("lifecycle_state")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            status: row
                .get("status")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            started_at: row
                .get("started_at")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            deadline_at: row
                .get("deadline_at")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            await_mode: row
                .get("await_mode")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            cancel_policy: row
                .get("cancel_policy")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            child_request_id: row
                .get("child_request_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            stuck_since: row
                .get("stuck_since")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            cancel_pending_remote_ack: row
                .get("cancel_pending_remote_ack")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
        .collect())
}

fn background_tool_calls_query(agent_did: &str) -> String {
    let escaped_agent_did = escape_graphql_string(agent_did);

    format!(
        r#"
        query {{
            AgentToolCall(
                filter: {{
                    await_mode: {{ _eq: "background" }},
                    lifecycle_state: {{ _in: ["pending", "running"] }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }}
                }},
                limit: {BACKGROUND_TOOL_CALL_LIMIT}
            ) {{
                request_id
                tool_call_id
                tool_name
                lifecycle_state
                status
                started_at
                deadline_at
                await_mode
                cancel_policy
                child_request_id
                stuck_since
                cancel_pending_remote_ack
            }}
        }}
    "#
    )
}

#[cfg(test)]
mod background_tool_query_tests {
    use super::*;

    #[test]
    fn scopes_live_rows_to_the_selected_agent_and_caps_the_scan() {
        let query = background_tool_calls_query("did:key:z6Mk\"selected");

        assert!(query.contains(r#"await_mode: { _eq: "background" }"#));
        assert!(query.contains(r#"lifecycle_state: { _in: ["pending", "running"] }"#));
        assert!(query.contains(r#"agent_did: { _eq: "did:key:z6Mk\"selected" }"#));
        assert!(query.contains(&format!("limit: {BACKGROUND_TOOL_CALL_LIMIT}")));
    }
}

#[tauri::command]
pub async fn desktop_list_subagent_tree(
    state: State<'_, DesktopAppState>,
    request: DesktopListSubagentTreeRequest,
) -> Result<SubagentTreeView, BridgeError> {
    let root_request_id = request.root_request_id.trim();
    if root_request_id.is_empty() {
        return Err(BridgeError::untyped("rootRequestId is required"));
    }
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge has not finished bootstrapping"))?;

    let mut accesses = vec![SubagentTreeAccess {
        label: None,
        access: ConfigAccess::Local(core.node_arc()),
    }];
    for record in core.peer_records().await {
        let Some(graphql) = record
            .graphql
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        accesses.push(SubagentTreeAccess {
            label: Some(record.label.clone()),
            access: ConfigAccess::Graphql(graphql.to_string()),
        });
    }

    let tree = build_subagent_tree(
        &accesses,
        root_request_id,
        request.include_terminal.unwrap_or(false),
        effective_subagent_tree_max_depth(request.max_depth),
    )
    .await
    .map_err(|error| BridgeError::untyped(format!("subagent tree query failed: {error:#}")))?;

    Ok(subagent_tree_view_from_gents(tree))
}

/// The bridge's TS-bound presentation shape, built from the owned
/// `gents::subagent_tree` projection (#1334).
fn subagent_tree_view_from_gents(tree: SubagentTree) -> SubagentTreeView {
    SubagentTreeView {
        root_request_id: tree.root_request_id,
        nodes: tree
            .nodes
            .into_iter()
            .map(|node| SubagentNodeView {
                request_id: node.request_id,
                resolved_via: node.resolved_via,
                session_id: node.session_id,
                agent_did: node.agent_did,
                behavior_id: node.behavior_id,
                lifecycle_state: node.lifecycle_state,
                subagent_depth: node.subagent_depth,
                caused_by_parent_request_id: node.caused_by_parent_request_id,
                caused_by_parent_tool_call_id: node.caused_by_parent_tool_call_id,
                backend_id: node.backend_id,
            })
            .collect(),
        edges: tree
            .edges
            .into_iter()
            .map(|edge| SubagentEdgeView {
                parent_request_id: edge.parent_request_id,
                child_request_id: edge.child_request_id,
                parent_tool_call_id: edge.parent_tool_call_id,
                tool_name: edge.tool_name,
                await_mode: edge.await_mode,
                cancel_policy: edge.cancel_policy,
                lifecycle_state: edge.lifecycle_state,
            })
            .collect(),
        truncated: tree.truncated,
        partial_errors: tree.partial_errors,
    }
}

#[cfg(test)]
fn subagent_tree_url(
    graphql: &str,
    root_request_id: &str,
    request: &DesktopListSubagentTreeRequest,
) -> Result<Url, BridgeError> {
    let trimmed = graphql.trim();
    if trimmed.is_empty() {
        return Err(BridgeError::untyped("agent graphql URL is empty"));
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme).map_err(|error| {
        BridgeError::untyped(format!("agent graphql URL is not a valid URL: {error}"))
    })?;
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

    fn request(
        include_terminal: Option<bool>,
        max_depth: Option<u32>,
    ) -> DesktopListSubagentTreeRequest {
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
        let url =
            subagent_tree_url("127.0.0.1:9181", "req-root", &request(Some(true), Some(4))).unwrap();
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
        assert!(err.message.contains("empty"));
    }
}

#[tauri::command]
pub async fn desktop_preview_interrupt_cascade(
    state: State<'_, DesktopAppState>,
    request: DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, BridgeError> {
    let core = crate::state::current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge core not initialized"))?;
    crate::cascade::build_cascade_preview(&core, &request)
        .await
        .map_err(BridgeError::untyped)
}

#[tauri::command]
pub async fn desktop_interrupt_request(
    state: State<'_, DesktopAppState>,
    request: DesktopInterruptRequest,
) -> Result<InterruptRequestResult, BridgeError> {
    let core = crate::state::current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge core not initialized"))?;
    tracing::info!(
        target: "gents_desktop::interrupt",
        request_id = %request.request_id,
        agent_did = %request.agent_did.as_deref().unwrap_or(""),
        cascade = request.cascade,
        "desktop interrupt action received"
    );
    let result = crate::cascade::interrupt_request(&core, &request).await;
    match &result {
        Ok(result) => tracing::info!(
            target: "gents_desktop::interrupt",
            request_id = %result.request_id,
            accepted = result.accepted,
            already_interrupted = result.already_interrupted,
            stale_preview = result.stale_preview,
            interrupt_requested_at = %result.interrupt_requested_at.as_deref().unwrap_or(""),
            "desktop interrupt action completed"
        ),
        Err(error) => tracing::warn!(
            target: "gents_desktop::interrupt",
            request_id = %request.request_id,
            agent_did = %request.agent_did.as_deref().unwrap_or(""),
            cascade = request.cascade,
            error,
            "desktop interrupt action failed"
        ),
    }
    result.map_err(BridgeError::untyped)
}

#[tauri::command]
pub async fn desktop_list_backends_with_health(
    state: State<'_, DesktopAppState>,
) -> Result<Vec<BackendHealthView>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    list_backends_with_health_for_core(core).await
}

pub async fn list_backends_with_health_for_core(
    core: Arc<ClientCore>,
) -> Result<Vec<BackendHealthView>, BridgeError> {
    let node = core.node();
    let backends = list_all_backends(node)
        .await
        .map_err(|err| err.to_string())?;

    let mut views = Vec::with_capacity(backends.len());
    for backend in backends {
        let recent_calls = fetch_recent_calls(node, &backend.backend_id)
            .await
            .map_err(|err| err.to_string())?;
        let display_state =
            derive_display_state(backend.enabled, &backend.probe_status).to_string();
        views.push(BackendHealthView {
            backend_id: backend.backend_id,
            name: backend.name,
            provider_kind: backend.provider_kind.as_str().to_string(),
            endpoint: backend.endpoint,
            enabled: backend.enabled,
            probe_status: backend.probe_status,
            display_state,
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

#[tauri::command]
pub async fn desktop_list_mcp_services_with_health(
    state: State<'_, DesktopAppState>,
) -> Result<Vec<MCPServiceHealthView>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    list_mcp_services_with_health_for_core(core).await
}

pub async fn list_mcp_services_with_health_for_core(
    core: Arc<ClientCore>,
) -> Result<Vec<MCPServiceHealthView>, BridgeError> {
    load_mcp_services_with_health(core.as_ref())
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_probe_mcp_service(
    state: State<'_, DesktopAppState>,
    request: DesktopProbeMcpServiceRequest,
) -> Result<McpServiceProbeResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    probe_mcp_service_for_core(core, request).await
}

pub(crate) async fn probe_mcp_service_for_core(
    core: Arc<ClientCore>,
    request: DesktopProbeMcpServiceRequest,
) -> Result<McpServiceProbeResult, BridgeError> {
    probe_mcp_service(core.as_ref(), &request.service_id)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_list_tool_call_holds(
    state: State<'_, DesktopAppState>,
    request: DesktopListHoldsRequest,
) -> Result<Vec<HeldToolCallView>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    list_tool_call_holds_for_core(core, request).await
}

pub async fn list_tool_call_holds_for_core(
    core: Arc<ClientCore>,
    request: DesktopListHoldsRequest,
) -> Result<Vec<HeldToolCallView>, BridgeError> {
    let held = core
        .list_tool_call_holds(&request.agent_did)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    Ok(held
        .into_iter()
        .map(|call| HeldToolCallView {
            tool_call_doc_id: call.tool_call_doc_id,
            tool_call_id: call.tool_call_id,
            request_id: call.request_id,
            session_id: call.session_id,
            agent_did: call.agent_did,
            tool_name: call.tool_name,
            args: call.args,
            deadline_at: call.deadline_at,
        })
        .collect())
}

#[tauri::command]
pub async fn desktop_resolve_tool_call_hold(
    state: State<'_, DesktopAppState>,
    request: DesktopResolveHoldRequest,
) -> Result<ResolveHoldResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    resolve_tool_call_hold_for_core(core, request).await
}

pub async fn resolve_tool_call_hold_for_core(
    core: Arc<ClientCore>,
    request: DesktopResolveHoldRequest,
) -> Result<ResolveHoldResult, BridgeError> {
    let approval_id = core
        .resolve_tool_call_hold(
            &request.agent_did,
            &request.tool_call_id,
            request.approve,
            request.reason.clone(),
        )
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    Ok(ResolveHoldResult {
        approval_id,
        tool_call_id: request.tool_call_id,
        decision: if request.approve {
            "approved".to_string()
        } else {
            "denied".to_string()
        },
    })
}
