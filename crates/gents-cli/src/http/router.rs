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

use crate::http::enrollment::{EnrollmentDecisionServiceHandle, EnrollmentOfferIssuerHandle};
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
    pub(crate) enrollment_offer_issuer: EnrollmentOfferIssuerHandle,
    pub(crate) enrollment_decisions: EnrollmentDecisionServiceHandle,
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
    enrollment_offer_issuer: EnrollmentOfferIssuerHandle,
    enrollment_decisions: EnrollmentDecisionServiceHandle,
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
        enrollment_offer_issuer,
        enrollment_decisions,
    };

    let mut router = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/version", get(version_handler))
        .route("/healthz", get(healthz_handler))
        .route("/status", get(status_handler))
        .route("/enrollment/decisions", post(enrollment_decision_handler))
        .route("/enrollment/pending", post(enrollment_pending_handler))
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

async fn enrollment_decision_handler(
    State(state): State<RuntimeHttpState>,
    axum::Json(command): axum::Json<gents_protocol::enrollment::EnrollmentOperatorDecisionCommand>,
) -> Response {
    let Some(service) = state.enrollment_decisions.read().await.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error": "enrollment authority is not ready"})),
        )
            .into_response();
    };
    match service.decide(&command).await {
        Ok(outcome) => (
            StatusCode::OK,
            axum::Json(json!({
                "request_id": outcome.request_id,
                "state": outcome.state,
                "decision_doc_id": outcome.decision_doc_id,
                "revision_doc_id": outcome.revision_doc_id,
                "delivery_pending": outcome.delivery_pending,
            })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn enrollment_pending_handler(
    State(state): State<RuntimeHttpState>,
    axum::Json(command): axum::Json<gents_protocol::enrollment::EnrollmentOperatorQueryCommand>,
) -> Response {
    let Some(service) = state.enrollment_decisions.read().await.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error": "enrollment authority is not ready"})),
        )
            .into_response();
    };
    match service.pending(&command).await {
        Ok(pending) => (StatusCode::OK, axum::Json(json!({"pending": pending}))).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
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
                .find(|runtime| runtime.agent_did == state.agent_did);
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
        let enrollment = match state.enrollment_offer_issuer.read().await.clone() {
            Some(issuer) => match issuer.mint().await {
                Ok(offer) => crate::http::enrollment::EnrollmentStatus::Available {
                    token: offer.token,
                    offer: offer.offer,
                },
                Err(error) => {
                    tracing::warn!(error = %error, "failed to mint authenticated enrollment offer");
                    crate::http::enrollment::EnrollmentStatus::Unavailable {
                        reason: "offer_mint_failed",
                    }
                }
            },
            None => crate::http::enrollment::EnrollmentStatus::Unavailable {
                reason: "runtime_not_ready",
            },
        };
        map.insert("enrollment".to_string(), json!(enrollment));
        crate::commands::p2p::flatten_p2p_fields(map, &p2p);
    }

    (StatusCode::OK, axum::Json(body)).into_response()
}

async fn self_handler(State(state): State<RuntimeHttpState>) -> Response {
    let (health, runtime, readiness, status_code) =
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
                    .cloned();
                let readiness = data
                    .behavior_readiness
                    .iter()
                    .find(|row| row.agent_did == state.agent_did)
                    .cloned();
                (health, runtime, readiness, status_code)
            }
            Err(error) => {
                let health = render_healthz_payload(&state, None, Some(error.to_string()));
                (health, None, None, StatusCode::SERVICE_UNAVAILABLE)
            }
        };

    match load_self_view(&state.graphql, &state.agent_did).await {
        Ok((behaviors, context_budget, _context_indicator)) => {
            let body = render_self_payload(
                &state,
                &health,
                runtime.as_ref(),
                readiness.as_ref(),
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
    readiness: Option<&gents_protocol::row::AgentBehaviorReadinessRow>,
    behaviors: &[SelfBehavior],
    context_budget: &ContextBudget,
) -> Value {
    let readiness = readiness.and_then(|row| {
        gents_protocol::row::decode_behavior_readiness_snapshot(row, &state.agent_did).ok()
    });
    let process_state = readiness
        .as_ref()
        .map(|snapshot| snapshot.process_state.as_str())
        .unwrap_or("unknown");
    let behavior = readiness
        .as_ref()
        .and_then(|snapshot| {
            behaviors
                .iter()
                .find(|behavior| behavior.behavior_id == snapshot.default_behavior_id)
        })
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

    use gents_protocol::row::{
        AgentBehaviorReadinessRow, BehaviorReadinessEntry, BehaviorReadinessProcessState,
        BehaviorReadinessSnapshot, BehaviorReadinessState, BEHAVIOR_READINESS_FORMAT_VERSION,
    };
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
            enrollment_offer_issuer: crate::http::enrollment::empty_issuer_handle(),
            enrollment_decisions: crate::http::enrollment::empty_decision_service_handle(),
        }
    }

    fn behavior(id: &str, enabled: bool, model_name: &str) -> SelfBehavior {
        SelfBehavior {
            behavior_id: id.to_string(),
            display_name: id.to_string(),
            model_name: model_name.to_string(),
            enabled,
            backend_id: format!("{id}-backend"),
            provider_kind: "OpenAiCompatible".to_string(),
            endpoint: "https://api.example.test/v1".to_string(),
            inference_profile_id: format!("{id}-profile"),
            context_window: Some(128_000),
        }
    }

    fn runtime() -> MetricsRuntimeRow {
        MetricsRuntimeRow {
            agent_did: "did:key:zAgent".to_string(),
            reconcile_phase: "idle".to_string(),
            last_reconcile_result: "applied".to_string(),
            last_reconcile_completed_at: "2026-06-04T00:00:00Z".to_string(),
        }
    }

    fn readiness(default_behavior_id: &str) -> AgentBehaviorReadinessRow {
        AgentBehaviorReadinessRow {
            agent_did: "did:key:zAgent".to_string(),
            snapshot_json: serde_json::to_string(&BehaviorReadinessSnapshot {
                format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
                process_state: BehaviorReadinessProcessState::Ready,
                active_generation: 1,
                router_generation: 1,
                default_behavior_id: default_behavior_id.to_string(),
                behaviors: vec![BehaviorReadinessEntry {
                    behavior_id: default_behavior_id.to_string(),
                    state: BehaviorReadinessState::Ready,
                    reason: None,
                }],
            })
            .unwrap(),
            updated_at: "2026-06-04T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn p2p_metrics_snapshot_decodes_pinned_live_sync_status() {
        let mut sync_status = gents::P2pSyncStatusSnapshot::default();
        sync_status.push_backlog.queued_items = 7;
        sync_status.push_backlog.stale_head_retirements_total = 17;
        sync_status.push_backlog.peer_capacity_parks_total = 13;
        sync_status.push_backlog.per_peer = vec![gents::P2pPeerBacklogSnapshot {
            peer_id: "peer-a".to_string(),
            consecutive_failures: 3,
            ..Default::default()
        }];
        sync_status.push_updates_coalesced_total = 43;
        sync_status.persisted_pending_dags = 17;
        sync_status.missing_link_retries = 23;
        sync_status.gossip_direction_filtered_total = 47;
        sync_status.pending_dag_capacity_shed = 59;
        sync_status.next_pending_retry_in_ms = Some(71);
        sync_status.pending_dag_terminal_quarantined = 73;
        sync_status.quarantined_pending_dags = 79;
        let snapshot = p2p_metrics_from_status(
            &json!({
                "enabled": true,
                "p2p_connected_peers": ["peer-a", "peer-b"],
                "p2p_replicator_count": 2,
                "p2p_sync_status": serde_json::to_value(sync_status)
                    .expect("serialize pinned sync status")
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
            Some(&runtime()),
            Some(&readiness("default")),
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
            Some("OpenAiCompatible")
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
    fn self_payload_does_not_invent_process_or_default_without_readiness() {
        let behaviors = vec![behavior("fallback", false, "minimax")];
        let payload = render_self_payload(
            &state(),
            &json!({ "status": "degraded", "ok": true }),
            None,
            None,
            &behaviors,
            &ContextBudget::default(),
        );

        assert_eq!(
            payload.get("process_state").and_then(Value::as_str),
            Some("unknown")
        );
        assert_eq!(payload.get("behavior"), Some(&Value::Null));
    }
}
