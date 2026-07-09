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
use defra_agent::defra_query::CollectionScope;

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
/// Hard budget for the multi-hop P2P self-fetch on the scrape path. Must stay
/// well under Prometheus's typical 10s scrape_timeout so inference/backend
/// metrics still return during hub saturation (#630 review on #670).
const P2P_METRICS_FETCH_BUDGET: Duration = Duration::from_millis(750);

#[derive(Clone)]
pub(crate) struct RuntimeHttpState {
    pub(crate) graphql: String,
    pub(crate) agent_name: String,
    pub(crate) agent_did: String,
    pub(crate) started_at: String,
    pub(crate) started_instant: Instant,
    /// The in-process runtime's measured backend health (#640) — the serve
    /// command shares the same handle the prober writes, so the metrics
    /// endpoint reports measurement, not the stored document constant.
    /// `None` when the HTTP surface runs without an in-process runtime.
    pub(crate) backend_health: Option<defra_agent::BackendHealthMap>,
    /// P2P admission knobs resolved at serve start (`None` when P2P disabled).
    pub(crate) p2p_admission: Option<P2pAdmissionState>,
    /// Last successful live P2P peer/replicator snapshot for non-blocking
    /// `/metrics` scrapes. Shared across clones of this state.
    pub(crate) p2p_metrics_cache: Arc<Mutex<Option<P2pMetricsSnapshot>>>,
}

pub(crate) fn runtime_contract_router(
    graphql: String,
    agent_name: String,
    agent_did: String,
    // `Some(scope)` mounts the read-only `defra_query` MCP tool at `/mcp`;
    // `None` leaves it off. It is opt-in because it is an unauthenticated read
    // surface (same listener exposure as the GraphQL endpoint).
    defra_query_mcp_scope: Option<CollectionScope>,
    backend_health: Option<defra_agent::BackendHealthMap>,
    p2p_admission: Option<P2pAdmissionState>,
) -> Router {
    let graphql_for_mcp = graphql.clone();
    let state = RuntimeHttpState {
        graphql,
        agent_name,
        agent_did,
        started_at: chrono::Utc::now().to_rfc3339(),
        started_instant: Instant::now(),
        backend_health,
        p2p_admission,
        p2p_metrics_cache: Arc::new(Mutex::new(None)),
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
    // Never block the whole scrape on multi-hop P2P self-HTTP. Prefer a short
    // budget + last-known counts so inference/backend metrics still ship.
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

/// Refresh live P2P counts under a hard budget; on timeout/error hold the last
/// successful snapshot (or admission-only bootstrap) and mark `stale`.
async fn load_p2p_metrics_for_scrape(state: &RuntimeHttpState) -> P2pMetricsSnapshot {
    let cached = state
        .p2p_metrics_cache
        .lock()
        .ok()
        .and_then(|guard| guard.clone());

    let fetch = crate::commands::p2p::fetch_live_http_p2p_status(None, &state.graphql);
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
                // Hold last-known peer/replicator counts; refresh admission from
                // the serve-start snapshot so knobs stay accurate even when stale.
                snap.admission = state.p2p_admission.clone();
                snap.enabled = state.p2p_admission.is_some() || snap.enabled;
                snap.stale = true;
                snap
            } else {
                p2p_metrics_admission_only(state, /*stale=*/ true)
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
    let enabled = p2p
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
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
    // Prefer serve-start admission over anything embedded in the HTTP status
    // payload so knobs stay authoritative even if runtime.json is stale.
    let admission = admission
        .cloned()
        .or_else(|| {
            p2p.get("p2p_admission")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
        });
    P2pMetricsSnapshot {
        enabled,
        connected_peers,
        replicators,
        admission,
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
                "service": "defra-agent",
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
            "service": "defra-agent",
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

    // Best-effort surfacing of the agent's own behaviors (with backend/profile
    // joined) and a context-budget summary. Failures here must not fail /status.
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
        // `/self` already returns the full `context_budget`; the compact context
        // indicator (third tuple element, surfaced on `/status`) is redundant here.
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
                "service": "defra-agent",
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
        "service": "defra-agent",
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
