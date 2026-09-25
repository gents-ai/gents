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
use tokio::sync::{watch, OnceCell};

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
/// Identity fields on `/status` have to come back before a caller's own
/// timeout. GraphQL and P2P probes can sit for much longer while the node
/// is still opening, which made desktop readiness look like a dead server.
const STATUS_PROBE_BUDGET: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct RuntimeHttpState {
    pub(crate) graphql: String,
    pub(crate) agent_name: String,
    pub(crate) agent_did: String,
    /// Live process ceiling advertised to desktop start/readiness checks.
    /// Lowercase `meta-only` / `readonly` / `readwrite`, matching `gents status`.
    pub(crate) tool_ceiling: String,
    pub(crate) tool_root: Option<String>,
    /// The home this runtime serves, so a client that finds a different
    /// runtime on its port can name it.
    pub(crate) home: Option<String>,
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
    pub(crate) activation_runtime: Arc<OnceCell<gents::Gents>>,
    pub(crate) activation_observation: watch::Receiver<RuntimeActivationObservation>,
    /// This node's `client` route collection versions, set once by serve after
    /// its migrations register every collection.
    pub(crate) replicated_schema: Arc<OnceCell<gents_protocol::peer_schema::ReplicatedSchema>>,
    pub(crate) serve_lifecycle: ServeLifecycleHandle,
}

/// Serve's `/status` lifecycle. It becomes ready only after serve writes
/// `runtime.json` for this process, and never returns to starting.
#[derive(Clone, Debug, Default)]
pub(crate) struct ServeLifecycleHandle(Arc<std::sync::OnceLock<()>>);

impl ServeLifecycleHandle {
    pub(crate) fn mark_ready(&self) {
        let _ = self.0.set(());
    }

    pub(crate) fn current(&self) -> gents_protocol::serve_lifecycle::ServeLifecycle {
        if self.0.get().is_some() {
            gents_protocol::serve_lifecycle::ServeLifecycle::Ready
        } else {
            gents_protocol::serve_lifecycle::ServeLifecycle::Starting
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeActivationObservation {
    pub(crate) event: Option<(u64, String, Result<(), String>)>,
    pub(crate) router: Option<(u64, String)>,
}

impl RuntimeActivationObservation {
    pub(crate) fn successful_for(&self, generation: u64, fingerprint: &str) -> bool {
        self.event
            .as_ref()
            .is_some_and(|(event_generation, event_fingerprint, result)| {
                *event_generation == generation
                    && event_fingerprint == fingerprint
                    && result.is_ok()
            })
            && self
                .router
                .as_ref()
                .is_some_and(|(router_generation, router_fingerprint)| {
                    *router_generation == generation && router_fingerprint == fingerprint
                })
    }
}

#[cfg(test)]
pub(crate) fn empty_activation_state() -> (
    Arc<OnceCell<gents::Gents>>,
    watch::Receiver<RuntimeActivationObservation>,
) {
    let (sender, receiver) = watch::channel(RuntimeActivationObservation::default());
    drop(sender);
    (Arc::new(OnceCell::new()), receiver)
}

pub(crate) fn runtime_contract_router(
    graphql: String,
    agent_name: String,
    agent_did: String,
    tool_ceiling: String,
    tool_root: Option<String>,
    home: Option<String>,
    // `Some(scope)` mounts the read-only `defra_query` MCP tool at `/mcp`;
    // `None` leaves it off. It is opt-in because it is an unauthenticated read
    // surface (same listener exposure as the GraphQL endpoint).
    defra_query_mcp_scope: Option<CollectionScope>,
    backend_health: Option<gents::BackendHealthMap>,
    p2p_admission: Option<P2pAdmissionState>,
    codex_shim_health: Option<crate::shared::CodexShimHealthHandle>,
    enrollment_offer_issuer: EnrollmentOfferIssuerHandle,
    enrollment_decisions: EnrollmentDecisionServiceHandle,
    activation_runtime: Arc<OnceCell<gents::Gents>>,
    activation_observation: watch::Receiver<RuntimeActivationObservation>,
    replicated_schema: Arc<OnceCell<gents_protocol::peer_schema::ReplicatedSchema>>,
    serve_lifecycle: ServeLifecycleHandle,
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
        tool_ceiling,
        tool_root,
        home,
        started_at: chrono::Utc::now().to_rfc3339(),
        started_instant: Instant::now(),
        backend_health,
        p2p_admission,
        p2p_metrics_cache: Arc::new(Mutex::new(None)),
        p2p_http_client,
        codex_shim_health,
        enrollment_offer_issuer,
        enrollment_decisions,
        activation_runtime,
        activation_observation,
        replicated_schema,
        serve_lifecycle,
    };

    let mut router = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/version", get(version_handler))
        .route("/healthz", get(healthz_handler))
        .route("/status", get(status_handler))
        .route("/activation", get(activation_handler))
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

async fn activation_handler(State(state): State<RuntimeHttpState>) -> Response {
    match tokio::time::timeout(Duration::from_secs(30), wait_for_activation(state)).await {
        Ok(response) => response,
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            axum::Json(json!({"error":"runtime did not activate the exact desired configuration"})),
        )
            .into_response(),
    }
}

async fn wait_for_activation(state: RuntimeHttpState) -> Response {
    let Some(runtime) = state.activation_runtime.get() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error":"runtime activation probe is not ready"})),
        )
            .into_response();
    };
    let mut observed = state.activation_observation;
    loop {
        // Mark before resolving: an acknowledgement that arrives while the
        // canonical resolver awaits remains visible to this iteration.
        drop(observed.borrow_and_update());
        let expected = match runtime.document_runtime_configuration_fingerprint().await {
            Ok(value) => value,
            Err(error) => return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(
                    json!({"error":format!("resolving desired runtime configuration: {error:#}")}),
                ),
            )
                .into_response(),
        };
        let current = observed.borrow().clone();
        if let Some((generation, fingerprint, result)) = current
            .event
            .as_ref()
            .filter(|(_, fingerprint, _)| fingerprint == &expected)
        {
            if let Err(error) = result {
                return (StatusCode::CONFLICT, axum::Json(json!({"error":"event-source activation failed", "generation":generation, "fingerprint":fingerprint, "detail":error}))).into_response();
            }
            if current.successful_for(*generation, fingerprint) {
                let readiness = match crate::commands::status::load_live_behavior_readiness(
                    &state.graphql,
                    &state.agent_did,
                )
                .await
                {
                    Ok(Some(row)) => gents_protocol::row::decode_behavior_readiness_snapshot(
                        &row,
                        &state.agent_did,
                    )
                    .ok(),
                    Ok(None) | Err(_) => None,
                };
                let ready = readiness.is_some_and(|snapshot| {
                    snapshot.process_state.accepts_work()
                        && snapshot.active_generation == *generation
                        && snapshot.router_generation == *generation
                });
                if !ready {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        axum::Json(json!({"error":"runtime readiness is not current for activated generation"})),
                    )
                        .into_response();
                }
                // Re-resolve after observing the acknowledgement: a concurrent
                // writer must never receive a fence for its predecessor.
                match runtime.document_runtime_configuration_fingerprint().await {
                    Ok(actual) if actual == expected => return (StatusCode::OK, axum::Json(json!({"generation":generation,"fingerprint":fingerprint}))).into_response(),
                    Ok(_) => continue,
                    Err(error) => return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({"error":format!("resolving desired runtime configuration: {error:#}")}))).into_response(),
                }
            }
        }
        if observed.changed().await.is_err() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({"error":"runtime activation observer stopped"})),
            )
                .into_response();
        }
    }
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
    let mut p2p = match tokio::time::timeout(
        P2P_METRICS_FETCH_BUDGET,
        crate::commands::p2p::load_live_http_p2p_status(None, &state.graphql),
    )
    .await
    {
        Ok(value) => value,
        Err(_) => json!({ "p2p_error": "timed out while the runtime was still starting" }),
    };
    if let Some(admission) = state.p2p_admission.as_ref() {
        if let Some(map) = p2p.as_object_mut() {
            map.insert("p2p_admission".to_string(), admission.to_json());
        }
    }
    let metrics = tokio::time::timeout(
        STATUS_PROBE_BUDGET,
        load_metrics_query_data(&state.graphql, &state.agent_did),
    )
    .await;
    let mut body = match metrics {
        Ok(Ok(data)) => {
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
                "tool_ceiling": state.tool_ceiling,
                "tool_root": state.tool_root,
                "home": state.home,
                "runtime": runtime,
                "runtimes": data.agent_runtimes,
                "backends": data.inference_backends,
                "liveness": data.liveness,
                "p2p": p2p.clone(),
            })
        }
        Ok(Err(error)) => json!({
            "status": "unhealthy",
            "ok": false,
            "service": "gents",
            "version": version_response().version,
            "started_at": state.started_at,
            "uptime_seconds": state.started_instant.elapsed().as_secs(),
            "graphql": state.graphql,
            "agent_name": state.agent_name,
            "agent_did": state.agent_did,
            "tool_ceiling": state.tool_ceiling,
            "tool_root": state.tool_root,
            "home": state.home,
            "runtime": Value::Null,
            "runtimes": [],
            "backends": [],
            "p2p": p2p.clone(),
            "error": error.to_string(),
        }),
        Err(_) => json!({
            "status": "starting",
            "ok": false,
            "service": "gents",
            "version": version_response().version,
            "started_at": state.started_at,
            "uptime_seconds": state.started_instant.elapsed().as_secs(),
            "graphql": state.graphql,
            "agent_name": state.agent_name,
            "agent_did": state.agent_did,
            "tool_ceiling": state.tool_ceiling,
            "tool_root": state.tool_root,
            "home": state.home,
            "runtime": Value::Null,
            "runtimes": [],
            "backends": [],
            "p2p": p2p.clone(),
            "error": "runtime metrics were not ready",
        }),
    };

    if body.get("error").is_none() {
        if let Ok(Ok((behaviors, context_budget, context))) = tokio::time::timeout(
            STATUS_PROBE_BUDGET,
            load_self_view(&state.graphql, &state.agent_did),
        )
        .await
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
        map.insert(
            gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD.to_string(),
            replicated_schema_status(&state.replicated_schema),
        );
        map.insert(
            gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD.to_string(),
            json!(state.serve_lifecycle.current()),
        );
        crate::commands::p2p::flatten_p2p_fields(map, &p2p);
    }

    (StatusCode::OK, axum::Json(body)).into_response()
}

/// `null` until serve publishes the schema after its migrations complete.
fn replicated_schema_status(
    replicated_schema: &OnceCell<gents_protocol::peer_schema::ReplicatedSchema>,
) -> Value {
    replicated_schema
        .get()
        .map_or(Value::Null, |schema| json!(schema))
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

    #[test]
    fn replicated_schema_is_null_until_published_then_complete() {
        let cell = OnceCell::new();
        assert_eq!(replicated_schema_status(&cell), Value::Null);

        let schema: gents_protocol::peer_schema::ReplicatedSchema =
            gents::agent::p2p_reconcile::CLIENT_COLLECTIONS
                .iter()
                .map(|name| {
                    (
                        (*name).to_string(),
                        gents_protocol::peer_schema::ReplicatedCollectionIdentity {
                            version_id: format!("bafy-{name}"),
                            branchable: true,
                            policy_resource: None,
                        },
                    )
                })
                .collect();
        cell.set(schema.clone()).unwrap();
        let published: gents_protocol::peer_schema::ReplicatedSchema =
            serde_json::from_value(replicated_schema_status(&cell)).unwrap();
        assert_eq!(published, schema);
    }

    fn state() -> RuntimeHttpState {
        let (activation_runtime, activation_observation) = empty_activation_state();
        RuntimeHttpState {
            graphql: "http://127.0.0.1:9181/api/v0/graphql".to_string(),
            agent_name: "amy".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            tool_ceiling: "readwrite".to_string(),
            tool_root: Some("/Users/test".to_string()),
            home: None,
            started_at: "2026-06-04T00:00:00Z".to_string(),
            started_instant: Instant::now(),
            backend_health: None,
            p2p_admission: None,
            p2p_metrics_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
            p2p_http_client: reqwest::Client::new(),
            codex_shim_health: None,
            enrollment_offer_issuer: crate::http::enrollment::empty_issuer_handle(),
            enrollment_decisions: crate::http::enrollment::empty_decision_service_handle(),
            activation_runtime,
            activation_observation,
            replicated_schema: Default::default(),
            serve_lifecycle: Default::default(),
        }
    }

    #[test]
    fn activation_observation_requires_one_successful_exact_tuple() {
        let observed = RuntimeActivationObservation {
            event: Some((4, "desired".into(), Ok(()))),
            router: Some((4, "desired".into())),
        };
        assert!(observed.successful_for(4, "desired"));
        assert!(!observed.successful_for(5, "desired"));
        assert!(!observed.successful_for(4, "other"));

        let failed = RuntimeActivationObservation {
            event: Some((4, "desired".into(), Err("seed failed".into()))),
            router: Some((4, "desired".into())),
        };
        assert!(!failed.successful_for(4, "desired"));

        let torn = RuntimeActivationObservation {
            event: Some((4, "desired".into(), Ok(()))),
            router: Some((5, "desired".into())),
        };
        assert!(!torn.successful_for(4, "desired"));
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
            compaction_threshold: gents::config::DEFAULT_COMPACTION_THRESHOLD,
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
    async fn status_lifecycle_is_starting_until_serve_marks_it_ready() {
        async fn lifecycle(state: &RuntimeHttpState) -> Value {
            let response = status_handler(State(state.clone())).await;
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("status body");
            serde_json::from_slice::<Value>(&body).expect("status json")
                [gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD]
                .clone()
        }
        let mut state = state();
        state.graphql = "http://127.0.0.1:9/api/v0/graphql".to_string();

        assert_eq!(lifecycle(&state).await, json!("starting"));
        state.serve_lifecycle.mark_ready();
        assert_eq!(lifecycle(&state).await, json!("ready"));
    }

    /// A stalled optional activity read must not delay or fail core health:
    /// `/healthz` and `/status` stay ok and on time, and progress falls back
    /// to `claimed_at`.
    #[tokio::test]
    async fn stalled_liveness_activity_read_does_not_hold_health() {
        use axum::{routing::post, Json, Router};

        let claimed_at = chrono::Utc::now() - chrono::Duration::seconds(120);
        let core = json!({
            "data": {
                "AgentRuntime": [serde_json::to_value(runtime()).unwrap()],
                "AgentBehaviorReadiness": [serde_json::to_value(readiness("default")).unwrap()],
                "InferenceBackend": [],
                "AgentRequest": [{
                    "_docID": "doc-req-1",
                    "request_id": "req-1",
                    "agent_did": "did:key:zAgent",
                    "claimed_at": claimed_at.to_rfc3339(),
                    "deadline": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                }],
                "AgentToolCall": []
            }
        });
        let mock = Router::new().route(
            "/api/v0/graphql",
            post(move |Json(body): Json<Value>| {
                let core = core.clone();
                async move {
                    let query = body["query"].as_str().unwrap_or_default().to_string();
                    if query.contains("AgentRuntime") {
                        return Ok(Json(core));
                    }
                    if query.contains("InferenceCall(") {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return Ok(Json(json!({ "data": {} })));
                    }
                    Err(StatusCode::NOT_FOUND)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock graphql");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, mock).await;
        });
        let mut state = state();
        state.graphql = format!("http://{addr}/api/v0/graphql");

        async fn body(response: Response) -> (StatusCode, Value) {
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("health body");
            (status, serde_json::from_slice(&bytes).expect("health json"))
        }
        fn progress_age_ms(body: &Value) -> i64 {
            body.pointer("/liveness/requests/0/last_progress_age_ms")
                .and_then(Value::as_i64)
                .expect("liveness request row")
        }

        let started = std::time::Instant::now();
        let (status, healthz) = body(healthz_handler(State(state.clone())).await).await;
        assert!(
            started.elapsed() < Duration::from_millis(1_500),
            "/healthz waited {:?} on a stalled activity read",
            started.elapsed()
        );
        assert_eq!(status, StatusCode::OK, "{healthz}");
        assert_eq!(healthz["ok"], json!(true), "{healthz}");
        assert!((120_000..180_000).contains(&progress_age_ms(&healthz)));

        let started = std::time::Instant::now();
        let (_, status_body) = body(status_handler(State(state)).await).await;
        assert!(
            started.elapsed() < STATUS_PROBE_BUDGET + P2P_METRICS_FETCH_BUDGET,
            "/status waited {:?}",
            started.elapsed()
        );
        assert_eq!(status_body["ok"], json!(true), "{status_body}");
        assert!((120_000..180_000).contains(&progress_age_ms(&status_body)));
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
