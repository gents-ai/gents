use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};

use crate::http::fleet::load_fleet_snapshot;
use crate::http::fleet_slots::load_fleet_slot_snapshot;
use crate::http::healthz::render_healthz_payload;
use crate::http::mcp_pool::load_mcp_pool_snapshot;
use crate::http::prometheus::{
    load_metrics_query_data, render_prometheus_metrics, with_local_native_executors,
    MetricsRuntimeRow, P2pMetricsSnapshot,
};
use crate::http::self_view::{load_self_view, ContextBudget, SelfBehavior};
use crate::http::sessions::{load_session_history_snapshot, SessionHistoryParams};
use crate::http::version::version_response;
use crate::shared::P2pAdmissionState;
use gents::defra_query::CollectionScope;

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const P2P_METRICS_FETCH_BUDGET: Duration = Duration::from_millis(750);

#[derive(Clone)]
pub(crate) struct RuntimeHttpState {
    pub(crate) graphql: String,
    pub(crate) agent_name: String,
    pub(crate) agent_did: String,
    pub(crate) started_at: String,
    pub(crate) started_instant: Instant,
    pub(crate) backend_health: Option<gents::BackendHealthMap>,
    pub(crate) p2p_admission: Option<P2pAdmissionState>,
    pub(crate) p2p_metrics_cache: Arc<Mutex<Option<P2pMetricsSnapshot>>>,
    pub(crate) p2p_http_client: reqwest::Client,
    /// The Codex shim's live binding. Shared because the shim may bind after the
    /// HTTP surface is already serving (#699). `None` when the host does not run
    /// a shim at all (embedders, desktop).
    pub(crate) codex_shim_health: Option<crate::shared::CodexShimHealthHandle>,
}

pub(crate) fn runtime_contract_router(
    graphql: String,
    agent_name: String,
    agent_did: String,
    // `Some(scope)` mounts the read-only `defra_query` MCP tool at `/mcp`;
    // `None` leaves it off. It is opt-in because it is an unauthenticated read
    // surface (same listener exposure as the GraphQL endpoint).
    defra_query_mcp_scope: Option<CollectionScope>,
    backend_health: Option<gents::BackendHealthMap>,
    p2p_admission: Option<P2pAdmissionState>,
    codex_shim_health: Option<crate::shared::CodexShimHealthHandle>,
) -> Router {
    let graphql_for_mcp = graphql.clone();
    let p2p_http_client = crate::commands::p2p::p2p_http_client().unwrap_or_else(|_| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("fallback P2P HTTP client")
    });
    let state = RuntimeHttpState {
        graphql,
        agent_name,
        agent_did,
        started_at: chrono::Utc::now().to_rfc3339(),
        started_instant: Instant::now(),
        backend_health,
        p2p_admission,
        p2p_metrics_cache: Arc::new(Mutex::new(None)),
        p2p_http_client,
        codex_shim_health,
    };

    let mut router = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/version", get(version_handler))
        .route("/healthz", get(healthz_handler))
        .route("/status", get(status_handler))
        .route("/self", get(self_handler))
        .route("/sessions", get(sessions_handler))
        .route("/fleet", get(fleet_handler))
        .route("/fleet/slots", get(fleet_slots_handler))
        .route("/mcp/pool", get(mcp_pool_handler))
        .route(
            "/subagents/dispatches",
            get(crate::http::r5_dispatch::subagent_dispatches_handler),
        )
        .route(
            "/subagents/tree",
            get(crate::http::subagent_tree::subagent_tree_handler),
        )
        .route(
            "/identity/decide",
            post(crate::http::identity_decide::identity_decide_handler),
        );

    if let Some(scope) = defra_query_mcp_scope {
        router = router.nest_service(
            "/mcp",
            crate::http::mcp_server::defra_query_mcp_service(graphql_for_mcp, scope),
        );
    }

    router.with_state(state)
}

async fn metrics_handler(State(state): State<RuntimeHttpState>) -> Response {
    let measured_backend_health = match &state.backend_health {
        Some(map) => map.snapshot().await,
        None => Default::default(),
    };
    let p2p_metrics = load_p2p_metrics_for_scrape(&state).await;
    match render_prometheus_metrics(
        &state.graphql,
        &state.agent_did,
        &measured_backend_health,
        Some(&p2p_metrics),
    )
    .await
    {
        Ok(body) => ([(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)], body).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("metrics render failed: {error}"),
        )
            .into_response(),
    }
}

async fn load_p2p_metrics_for_scrape(state: &RuntimeHttpState) -> P2pMetricsSnapshot {
    if state.p2p_admission.is_none() {
        return p2p_metrics_admission_only(state, false);
    }

    let cached = state
        .p2p_metrics_cache
        .lock()
        .ok()
        .and_then(|guard| guard.clone());

    let fetch = crate::commands::p2p::fetch_live_http_p2p_status_with_client(
        None,
        &state.graphql,
        &state.p2p_http_client,
    );
    match tokio::time::timeout(P2P_METRICS_FETCH_BUDGET, fetch).await {
        Ok(Ok(status)) => {
            let mut snap = p2p_metrics_from_status(&status, state.p2p_admission.as_ref());
            snap.stale = false;
            if let Ok(mut guard) = state.p2p_metrics_cache.lock() {
                *guard = Some(snap.clone());
            }
            snap
        }
        Ok(Err(_)) | Err(_) => {
            if let Some(mut snap) = cached {
                snap.admission = state.p2p_admission.clone();
                snap.enabled = state.p2p_admission.is_some() || snap.enabled;
                snap.stale = true;
                snap
            } else {
                p2p_metrics_admission_only(state, true)
            }
        }
    }
}

fn p2p_metrics_admission_only(state: &RuntimeHttpState, stale: bool) -> P2pMetricsSnapshot {
    P2pMetricsSnapshot {
        enabled: state.p2p_admission.is_some(),
        connected_peers: 0,
        replicators: 0,
        admission: state.p2p_admission.clone(),
        sync_status: None,
        stale,
    }
}

async fn version_handler() -> impl IntoResponse {
    axum::Json(version_response())
}

async fn healthz_handler(State(state): State<RuntimeHttpState>) -> Response {
    match load_metrics_query_data(&state.graphql, &state.agent_did).await {
        Ok(data) => {
            let data = with_local_native_executors(data);
            let health = render_healthz_payload(&state, Some(&data), None);
            let status = if health.get("ok") == Some(&serde_json::Value::Bool(true)) {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (status, axum::Json(health)).into_response()
        }
        Err(error) => {
            let health = render_healthz_payload(&state, None, Some(error.to_string()));
            (StatusCode::SERVICE_UNAVAILABLE, axum::Json(health)).into_response()
        }
    }
}

fn p2p_metrics_from_status(
    p2p: &Value,
    admission: Option<&P2pAdmissionState>,
) -> P2pMetricsSnapshot {
    let enabled = p2p.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let connected_peers = p2p
        .get("p2p_connected_peers")
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    let replicators = p2p
        .get("p2p_replicator_count")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(0);
    let admission = admission.cloned().or_else(|| {
        p2p.get("p2p_admission")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
    });
    let sync_status = p2p
        .get("p2p_sync_status")
        .filter(|value| !value.is_null())
        .and_then(|value| {
            use gents::P2pSyncStatusAdapter;
            gents::JsonP2pSyncStatusAdapter.adapt(value).ok()
        });
    P2pMetricsSnapshot {
        enabled,
        connected_peers,
        replicators,
        admission,
        sync_status,
        stale: false,
    }
}

async fn status_handler(State(state): State<RuntimeHttpState>) -> Response {
    let mut p2p = crate::commands::p2p::load_live_http_p2p_status(None, &state.graphql).await;
    if let Some(admission) = state.p2p_admission.as_ref() {
        if let Some(map) = p2p.as_object_mut() {
            map.insert("p2p_admission".to_string(), admission.to_json());
        }
    }
    let mut body = match load_metrics_query_data(&state.graphql, &state.agent_did).await {
        Ok(data) => {
            let data = with_local_native_executors(data);
            let health = render_healthz_payload(&state, Some(&data), None);
            let runtime = data
                .agent_runtimes
                .iter()
                .find(|runtime| runtime.agent_did == state.agent_did)
                .or_else(|| data.agent_runtimes.first());
            json!({
                "status": health.get("status").cloned().unwrap_or(Value::String("unknown".to_string())),
                "ok": health.get("ok").cloned().unwrap_or(Value::Bool(false)),
                "service": "gents",
                "version": version_response().version,
                "started_at": state.started_at,
                "uptime_seconds": state.started_instant.elapsed().as_secs(),
                "graphql": state.graphql,
                "agent_name": state.agent_name,
                "agent_did": state.agent_did,
                "runtime": runtime,
                "runtimes": data.agent_runtimes,
                "backends": data.inference_backends,
                "liveness": data.liveness,
                "p2p": p2p.clone(),
            })
        }
        Err(error) => json!({
            "status": "unhealthy",
            "ok": false,
            "service": "gents",
            "version": version_response().version,
            "started_at": state.started_at,
            "uptime_seconds": state.started_instant.elapsed().as_secs(),
            "graphql": state.graphql,
            "agent_name": state.agent_name,
            "agent_did": state.agent_did,
            "runtime": Value::Null,
            "runtimes": [],
            "backends": [],
            "p2p": p2p.clone(),
            "error": error.to_string(),
        }),
    };

    if body.get("error").is_none() {
        if let Ok((behaviors, context_budget, context)) =
            load_self_view(&state.graphql, &state.agent_did).await
        {
            if let Some(map) = body.as_object_mut() {
                map.insert("behaviors".to_string(), json!(behaviors));
                map.insert("context".to_string(), json!(context));
                map.insert("context_budget".to_string(), json!(context_budget));
            }
        }
    }

    if let Some(map) = body.as_object_mut() {
        crate::commands::p2p::flatten_p2p_fields(map, &p2p);
    }

    (StatusCode::OK, axum::Json(body)).into_response()
}

async fn self_handler(State(state): State<RuntimeHttpState>) -> Response {
    let (health, runtime, status_code) =
        match load_metrics_query_data(&state.graphql, &state.agent_did).await {
            Ok(data) => {
                let data = with_local_native_executors(data);
                let health = render_healthz_payload(&state, Some(&data), None);
                let status_code = if health.get("ok") == Some(&Value::Bool(true)) {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                };
                let runtime = data
                    .agent_runtimes
                    .iter()
                    .find(|runtime| runtime.agent_did == state.agent_did)
                    .or_else(|| data.agent_runtimes.first())
                    .cloned();
                (health, runtime, status_code)
            }
            Err(error) => {
                let health = render_healthz_payload(&state, None, Some(error.to_string()));
                (health, None, StatusCode::SERVICE_UNAVAILABLE)
            }
        };

    match load_self_view(&state.graphql, &state.agent_did).await {
        Ok((behaviors, context_budget, _context_indicator)) => {
            let body = render_self_payload(
                &state,
                &health,
                runtime.as_ref(),
                &behaviors,
                &context_budget,
            );
            (status_code, axum::Json(body)).into_response()
        }
        Err(error) => {
            let mut body = json!({
                "status": "unhealthy",
                "ok": false,
                "service": "gents",
                "version": version_response().version,
                "started_at": state.started_at,
                "uptime_seconds": state.started_instant.elapsed().as_secs(),
                "graphql": state.graphql,
                "agent_name": state.agent_name,
                "agent_did": state.agent_did,
                "process_state": "unknown",
                "behavior": Value::Null,
                "behaviors": [],
                "context_budget": ContextBudget::default(),
                "error": format!("self view query failed: {error:#}"),
            });
            if let Some(map) = body.as_object_mut() {
                if let Some(health_status) = health.get("status").cloned() {
                    map.insert("runtime_status".to_string(), health_status);
                }
            }
            (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(body)).into_response()
        }
    }
}

async fn sessions_handler(
    State(state): State<RuntimeHttpState>,
    Query(query): Query<SessionHistoryParams>,
) -> Response {
    match load_session_history_snapshot(&state.graphql, &state.agent_did, query.limit).await {
        Ok(snapshot) => (StatusCode::OK, axum::Json(snapshot)).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("sessions snapshot failed: {error:#}"),
        )
            .into_response(),
    }
}

fn render_self_payload(
    state: &RuntimeHttpState,
    health: &Value,
    runtime: Option<&MetricsRuntimeRow>,
    behaviors: &[SelfBehavior],
    context_budget: &ContextBudget,
) -> Value {
    let process_state = runtime
        .map(|runtime| runtime.process_state.as_str())
        .filter(|state| !state.is_empty())
        .or_else(|| health.get("status").and_then(Value::as_str))
        .unwrap_or("unknown");
    let behavior = select_primary_behavior(behaviors)
        .map(render_self_behavior)
        .unwrap_or(Value::Null);
    let behaviors = behaviors
        .iter()
        .map(render_self_behavior)
        .collect::<Vec<_>>();

    json!({
        "status": health.get("status").cloned().unwrap_or(Value::String("unknown".to_string())),
        "ok": health.get("ok").cloned().unwrap_or(Value::Bool(false)),
        "service": "gents",
        "version": version_response().version,
        "started_at": &state.started_at,
        "uptime_seconds": state.started_instant.elapsed().as_secs(),
        "graphql": &state.graphql,
        "agent_name": &state.agent_name,
        "agent_did": &state.agent_did,
        "process_state": process_state,
        "runtime": runtime,
        "behavior": behavior,
        "behaviors": behaviors,
        "context_budget": context_budget,
    })
}

fn select_primary_behavior(behaviors: &[SelfBehavior]) -> Option<&SelfBehavior> {
    behaviors
        .iter()
        .find(|behavior| behavior.enabled)
        .or_else(|| behaviors.first())
}

fn render_self_behavior(behavior: &SelfBehavior) -> Value {
    json!({
        "behavior_id": &behavior.behavior_id,
        "display_name": &behavior.display_name,
        "model_name": &behavior.model_name,
        "enabled": behavior.enabled,
        "backend_id": &behavior.backend_id,
        "backend_provider": &behavior.provider_kind,
        "backend_endpoint": &behavior.endpoint,
        "inference_profile_id": &behavior.inference_profile_id,
        "context_window": behavior.context_window,
    })
}

async fn fleet_handler(State(state): State<RuntimeHttpState>) -> Response {
    match load_fleet_snapshot(&state.graphql).await {
        Ok(snapshot) => (StatusCode::OK, axum::Json(snapshot)).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("fleet snapshot failed: {error:#}"),
        )
            .into_response(),
    }
}

async fn fleet_slots_handler(State(state): State<RuntimeHttpState>) -> Response {
    match load_fleet_slot_snapshot(&state.graphql).await {
        Ok(snapshot) => (StatusCode::OK, axum::Json(snapshot)).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("fleet slot snapshot failed: {error:#}"),
        )
            .into_response(),
    }
}

async fn mcp_pool_handler(State(state): State<RuntimeHttpState>) -> Response {
    match load_mcp_pool_snapshot(&state.graphql, &state.agent_did).await {
        Ok(snapshot) => (StatusCode::OK, axum::Json(snapshot)).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("mcp pool snapshot failed: {error:#}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::json;

    use super::*;

    fn state() -> RuntimeHttpState {
        RuntimeHttpState {
            graphql: "http://127.0.0.1:9181/api/v0/graphql".to_string(),
            agent_name: "amy".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            started_at: "2026-06-04T00:00:00Z".to_string(),
            started_instant: Instant::now(),
            backend_health: None,
            p2p_admission: None,
            p2p_metrics_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
            p2p_http_client: reqwest::Client::new(),
            codex_shim_health: None,
        }
    }

    fn behavior(id: &str, enabled: bool, model_name: &str) -> SelfBehavior {
        SelfBehavior {
            behavior_id: id.to_string(),
            display_name: id.to_string(),
            model_name: model_name.to_string(),
            enabled,
            backend_id: format!("{id}-backend"),
            provider_kind: "openai-compatible".to_string(),
            endpoint: "https://api.example.test/v1".to_string(),
            inference_profile_id: format!("{id}-profile"),
            context_window: Some(128_000),
        }
    }

    fn runtime(process_state: &str) -> MetricsRuntimeRow {
        MetricsRuntimeRow {
            agent_did: "did:key:zAgent".to_string(),
            process_state: process_state.to_string(),
            reconcile_phase: "idle".to_string(),
            active_generation: 1,
            router_generation: 1,
            runnable_behavior_count: 1,
            unavailable_behavior_count: 0,
            last_reconcile_result: "applied".to_string(),
            last_reconcile_completed_at: "2026-06-04T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn p2p_metrics_snapshot_decodes_pinned_live_sync_status() {
        let snapshot = p2p_metrics_from_status(
            &json!({
                "enabled": true,
                "p2p_connected_peers": ["peer-a", "peer-b"],
                "p2p_replicator_count": 2,
                "p2p_sync_status": {
                    "push_backlog": {
                        "queue_item_capacity": 128,
                        "queue_byte_capacity": 1048576,
                        "per_peer_active_cap": 2,
                        "worker_count": 8,
                        "queued_items": 7,
                        "queued_bytes": 4096,
                        "active_jobs": 3,
                        "enqueued_total": 101,
                        "coalesced_total": 11,
                        "rejected_items_total": 5,
                        "rejected_bytes_total": 2,
                        "completed_total": 79,
                        "failed_total": 4,
                        "stale_head_retirements_total": 17,
                        "peer_capacity_parks_total": 13,
                        "per_peer": [{
                            "peer_id": "peer-a",
                            "queued_items": 4,
                            "queued_bytes": 2048,
                            "active_jobs": 1,
                            "consecutive_failures": 3,
                            "cooldown_remaining_ms": 750
                        }]
                    },
                    "broadcast_coalesced_total": 41,
                    "push_updates_coalesced_total": 43,
                    "gossip_direction_filtered_total": 47,
                    "pending_dags": 13,
                    "pending_dag_capacity": 1000,
                    "persisted_pending_dags": 17,
                    "persisted_pending_dag_capacity": 4000,
                    "pending_resync_in_flight": true,
                    "retained_background_tasks": 6,
                    "missing_link_retries": 23,
                    "pending_dag_resolved": 29,
                    "pending_dag_expired": 31,
                    "single_flight_suppressed": 37,
                    "already_merged_fast_path": 53,
                    "pending_dag_capacity_shed": 59,
                    "pending_dag_retry_dispatched": 61,
                    "pending_dag_retry_suppressed": 67,
                    "next_pending_retry_in_ms": 71,
                    "pending_dag_terminal_quarantined": 73,
                    "quarantined_pending_dags": 79
                }
            }),
            None,
        );

        assert_eq!(snapshot.connected_peers, 2);
        assert_eq!(snapshot.replicators, 2);
        let sync = snapshot.sync_status.expect("valid pinned sync status");
        assert_eq!(sync.push_backlog.queued_items, 7);
        assert_eq!(sync.push_backlog.stale_head_retirements_total, 17);
        assert_eq!(sync.push_backlog.peer_capacity_parks_total, 13);
        assert_eq!(sync.push_backlog.per_peer[0].consecutive_failures, 3);
        assert_eq!(sync.push_updates_coalesced_total, 43);
        assert_eq!(sync.persisted_pending_dags, 17);
        assert_eq!(sync.missing_link_retries, 23);
        assert_eq!(sync.gossip_direction_filtered_total, 47);
        assert_eq!(sync.pending_dag_capacity_shed, 59);
        assert_eq!(sync.next_pending_retry_in_ms, Some(71));
        assert_eq!(sync.pending_dag_terminal_quarantined, 73);
        assert_eq!(sync.quarantined_pending_dags, 79);
    }

    #[tokio::test]
    async fn disabled_p2p_metrics_skip_live_status_fetch() {
        let mut state = state();
        state.graphql = "http://127.0.0.1:9/api/v0/graphql".to_string();

        let snapshot = load_p2p_metrics_for_scrape(&state).await;

        assert!(!snapshot.enabled);
        assert!(!snapshot.stale);
        assert_eq!(snapshot.connected_peers, 0);
        assert_eq!(snapshot.replicators, 0);
        assert!(snapshot.admission.is_none());
        assert!(snapshot.sync_status.is_none());
    }

    #[test]
    fn self_payload_uses_acceptance_field_names_and_primary_behavior() {
        let behaviors = vec![
            behavior("disabled", false, "llama-local"),
            behavior("default", true, "gpt-4.1"),
        ];
        let payload = render_self_payload(
            &state(),
            &json!({ "status": "ok", "ok": true }),
            Some(&runtime("ready")),
            &behaviors,
            &ContextBudget::default(),
        );

        assert_eq!(
            payload.get("agent_name").and_then(Value::as_str),
            Some("amy")
        );
        assert_eq!(
            payload.get("agent_did").and_then(Value::as_str),
            Some("did:key:zAgent")
        );
        assert_eq!(
            payload.get("process_state").and_then(Value::as_str),
            Some("ready")
        );
        assert_eq!(
            payload
                .pointer("/behavior/model_name")
                .and_then(Value::as_str),
            Some("gpt-4.1")
        );
        assert_eq!(
            payload
                .pointer("/behavior/backend_endpoint")
                .and_then(Value::as_str),
            Some("https://api.example.test/v1")
        );
        assert_eq!(
            payload
                .pointer("/behavior/backend_provider")
                .and_then(Value::as_str),
            Some("openai-compatible")
        );
        assert_eq!(
            payload
                .get("behaviors")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn self_payload_falls_back_to_first_behavior_when_none_enabled() {
        let behaviors = vec![behavior("fallback", false, "minimax")];
        let payload = render_self_payload(
            &state(),
            &json!({ "status": "degraded", "ok": true }),
            None,
            &behaviors,
            &ContextBudget::default(),
        );

        assert_eq!(
            payload.get("process_state").and_then(Value::as_str),
            Some("degraded")
        );
        assert_eq!(
            payload
                .pointer("/behavior/behavior_id")
                .and_then(Value::as_str),
            Some("fallback")
        );
    }
}
