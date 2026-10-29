use gents::config_client::GraphqlEndpoint;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    middleware,
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
    load_metrics_core_data, load_metrics_query_data, render_prometheus_metrics,
    with_local_native_executors, MetricsRuntimeRow, P2pMetricsSnapshot,
};
use crate::http::self_view::{load_self_view, ContextBudget, SelfAgent};
use crate::http::sessions::{load_session_history_snapshot, SessionHistoryParams};
use crate::http::version::version_response;
use crate::shared::P2pAdmissionState;

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const P2P_METRICS_FETCH_BUDGET: Duration = Duration::from_millis(750);
/// Bounds core health so `/status` identity fields return before a caller's
/// own timeout while the node is still opening.
const STATUS_PROBE_BUDGET: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct RuntimeHttpState {
    /// Address-only endpoint for public connection metadata.
    pub(crate) graphql: GraphqlEndpoint,
    /// Served principal; the node's P2P access control admits only it.
    pub(crate) p2p_graphql: GraphqlEndpoint,
    pub(crate) node_name: String,
    pub(crate) node_did: String,
    pub(crate) tool_ceiling: String,
    pub(crate) tool_root: Option<String>,
    pub(crate) tool_root_error: Option<String>,
    pub(crate) cli_tools: Vec<String>,
    pub(crate) home: Option<String>,
    pub(crate) started_at: String,
    pub(crate) started_instant: Instant,
    pub(crate) backend_health: Option<gents::BackendHealthMap>,
    pub(crate) p2p_admission: Option<P2pAdmissionState>,
    pub(crate) p2p_metrics_cache: Arc<Mutex<Option<P2pMetricsSnapshot>>>,
    /// Shared because the shim may bind after HTTP serving starts.
    pub(crate) codex_shim_health: Option<crate::shared::CodexShimHealthHandle>,
    pub(crate) enrollment_offer_issuer: EnrollmentOfferIssuerHandle,
    pub(crate) enrollment_decisions: EnrollmentDecisionServiceHandle,
    pub(crate) activation_runtime: Arc<OnceCell<gents::Gents>>,
    pub(crate) activation_observation: watch::Receiver<RuntimeActivationObservation>,
    pub(crate) replicated_schema: Arc<OnceCell<gents_protocol::peer_schema::ReplicatedSchema>>,
    pub(crate) serve_lifecycle: ServeLifecycleHandle,
}

/// Ready only after serve writes `runtime.json` for this process; never
/// returns to starting.
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

#[expect(
    clippy::too_many_arguments,
    reason = "each argument is a separate shared handle owned by the router state"
)]
pub(crate) fn runtime_contract_router(
    graphql: String,
    node_name: String,
    node_did: String,
    tool_ceiling: String,
    tool_root: Result<Option<String>, String>,
    cli_tools: Vec<String>,
    home: Option<String>,
    mcp: Option<crate::http::mcp_server::McpServiceOptions>,
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
    let operator_did = node_did.clone();
    let graphql_for_mcp = graphql.clone();
    let state = RuntimeHttpState {
        p2p_graphql: GraphqlEndpoint::as_principal(graphql.clone(), node_did.clone()),
        graphql: GraphqlEndpoint::anonymous(graphql),
        node_name,
        node_did,
        tool_ceiling,
        tool_root_error: tool_root.as_ref().err().cloned(),
        tool_root: tool_root.ok().flatten(),
        cli_tools,
        home,
        started_at: chrono::Utc::now().to_rfc3339(),
        started_instant: Instant::now(),
        backend_health,
        p2p_admission,
        p2p_metrics_cache: Arc::new(Mutex::new(None)),
        codex_shim_health,
        enrollment_offer_issuer,
        enrollment_decisions,
        activation_runtime,
        activation_observation,
        replicated_schema,
        serve_lifecycle,
    };

    let operator_routes = Router::new()
        .route(
            "/packs/install",
            post(crate::http::pack_install::install_handler).layer(
                axum::extract::DefaultBodyLimit::max(crate::http::pack_install::MAX_BODY),
            ),
        )
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .route("/activation", get(activation_handler))
        .route("/self", get(self_handler))
        .route("/sessions", get(sessions_handler))
        .route("/fleet", get(fleet_handler))
        .route("/fleet/slots", get(fleet_slots_handler))
        .route("/mcp/pool", get(mcp_pool_handler))
        .route(
            "/identity/decide",
            post(crate::http::identity_decide::identity_decide_handler),
        )
        .route_layer(middleware::from_fn_with_state(
            operator_did,
            super::access::require_operator,
        ));
    let mut router = Router::new()
        .route("/version", get(version_handler))
        .route("/status", get(status_route))
        .route(
            gents_protocol::serve_lifecycle::control::RUNTIME_CONTROL_PATH,
            post(crate::http::runtime_control::runtime_control_handler),
        )
        .route(
            gents_protocol::job::JOB_CONTROL_PATH,
            post(crate::http::job_control::job_control_handler),
        )
        .route(
            gents_protocol::process_control::PROCESS_CONTROL_PATH,
            post(crate::http::process_control::process_control_handler),
        )
        .route(
            gents::inference_observation::INFERENCE_ACTIVITY_PATH,
            post(crate::http::inference_activity::read),
        )
        .route("/enrollment/decisions", post(enrollment_decision_handler))
        .route("/enrollment/pending", post(enrollment_pending_handler))
        .route("/accounts/usage/read", post(account_usage_read_handler))
        .route(
            gents_desktop_core::inference::INFERENCE_SETTINGS_PATH,
            get(crate::http::inference::inference_settings_handler),
        )
        .route(
            gents_protocol::file_inventory::FILE_INVENTORY_OPERATOR_PATH,
            post(crate::http::file_inventory::operator_inventory_handler),
        )
        .merge(operator_routes);

    if let Some(options) = mcp {
        router = router.nest_service(
            "/mcp",
            crate::http::mcp_server::defra_query_mcp_service(graphql_for_mcp, options),
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
                let readiness = match crate::commands::status::load_live_node_readiness(
                    &state.p2p_graphql,
                    &state.node_did,
                )
                .await
                {
                    Ok(Some(row)) => {
                        gents_protocol::row::decode_node_readiness_snapshot(&row, &state.node_did)
                            .ok()
                    }
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

/// Only the runtime's own operator may ask.
async fn account_usage_read_handler(
    State(state): State<RuntimeHttpState>,
    axum::Json(command): axum::Json<crate::commands::accounts::UsageReadCommand>,
) -> Response {
    let error = |status: StatusCode, error: String| {
        (status, axum::Json(json!({ "error": error }))).into_response()
    };
    let Some(service) = state.enrollment_decisions.read().await.clone() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime is not ready".to_string(),
        );
    };
    let authenticated = match command.validate_at(chrono::Utc::now()) {
        Ok(()) => {
            service
                .authenticate_signed(
                    &command.signer_did,
                    &command.signing_payload(),
                    &command.sig,
                    &command.nonce,
                )
                .await
        }
        Err(invalid) => Err(invalid),
    };
    if let Err(refused) = authenticated {
        return error(StatusCode::FORBIDDEN, format!("{refused:#}"));
    }
    let Some(runtime) = state.activation_runtime.get() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime is not activated yet".to_string(),
        );
    };
    match runtime
        .read_usage(command.trigger, command.provider.as_deref())
        .await
    {
        Ok(reads) => (
            StatusCode::OK,
            axum::Json(json!({ "node_did": runtime.node_did(), "reads": reads })),
        )
            .into_response(),
        Err(failed) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{failed:#}")),
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
            axum::Json(json!({
                "error": error.to_string(),
                "code": gents_protocol::error_chain::find_source::<
                    gents::agent::p2p_reconcile::EnrollmentDecisionRefusal,
                >(error.as_ref())
                .map(gents::agent::p2p_reconcile::EnrollmentDecisionRefusal::code),
            })),
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
        &state.p2p_graphql,
        &state.node_did,
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

    let fetch = crate::commands::p2p::fetch_live_http_p2p_status(None, &state.p2p_graphql);
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
    match load_metrics_query_data(&state.p2p_graphql, &state.node_did).await {
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
        crate::commands::p2p::load_live_http_p2p_status(None, &state.p2p_graphql),
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
    let probe_deadline = tokio::time::Instant::now() + STATUS_PROBE_BUDGET;
    let metrics = tokio::time::timeout_at(
        probe_deadline,
        load_metrics_core_data(&state.p2p_graphql, &state.node_did),
    )
    .await;
    let metrics = match metrics {
        Ok(Ok(core)) => Ok(Ok(core
            .with_liveness_activity(&state.p2p_graphql, probe_deadline)
            .await)),
        Ok(Err(error)) => Ok(Err(error)),
        Err(elapsed) => Err(elapsed),
    };
    let mut body = match metrics {
        Ok(Ok(data)) => {
            let data = with_local_native_executors(data);
            let health = render_healthz_payload(&state, Some(&data), None);
            let runtime = data
                .node_runtimes
                .iter()
                .find(|runtime| runtime.node_did == state.node_did);
            json!({
                "status": health.get("status").cloned().unwrap_or(Value::String("unknown".to_string())),
                "ok": health.get("ok").cloned().unwrap_or(Value::Bool(false)),
                "service": "gents",
                "version": version_response().version,
                "started_at": state.started_at,
                "uptime_seconds": state.started_instant.elapsed().as_secs(),
                "graphql": state.graphql,
                "node_name": state.node_name,
                "node_did": state.node_did,
                "tool_ceiling": state.tool_ceiling,
                "tool_root": state.tool_root,
            "tool_root_error": state.tool_root_error,
            "cli_tools": state.cli_tools,
                "home": state.home,
                "runtime": runtime,
                "runtimes": data.node_runtimes,
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
            "node_name": state.node_name,
            "node_did": state.node_did,
            "tool_ceiling": state.tool_ceiling,
            "tool_root": state.tool_root,
            "tool_root_error": state.tool_root_error,
            "cli_tools": state.cli_tools,
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
            "node_name": state.node_name,
            "node_did": state.node_did,
            "tool_ceiling": state.tool_ceiling,
            "tool_root": state.tool_root,
            "tool_root_error": state.tool_root_error,
            "cli_tools": state.cli_tools,
            "home": state.home,
            "runtime": Value::Null,
            "runtimes": [],
            "backends": [],
            "p2p": p2p.clone(),
            "error": "runtime metrics were not ready",
        }),
    };

    if body.get("error").is_none() {
        if let Ok(Ok((agents, context_budget, context))) = tokio::time::timeout(
            STATUS_PROBE_BUDGET,
            load_self_view(&state.p2p_graphql, &state.node_did),
        )
        .await
        {
            if let Some(map) = body.as_object_mut() {
                map.insert("agents".to_string(), json!(agents));
                map.insert("context".to_string(), json!(context));
                map.insert("context_budget".to_string(), json!(context_budget));
            }
        }
    }

    if let Some(map) = body.as_object_mut() {
        crate::commands::p2p::flatten_p2p_fields(map, &p2p);
    }
    append_status_contracts(&state, &mut body).await;
    (StatusCode::OK, axum::Json(body)).into_response()
}

async fn status_route(
    State(state): State<RuntimeHttpState>,
    identity: defra_http::ExtractIdentity,
) -> Response {
    let mut response = if identity
        .did()
        .is_some_and(|actor| actor.as_ref() == state.node_did)
    {
        status_handler(State(state)).await
    } else {
        let mut body = json!({
            "service": "gents",
            "version": version_response().version,
            "node_name": state.node_name,
            "node_did": state.node_did,
            "graphql": state.graphql,
        });
        append_status_contracts(&state, &mut body).await;
        axum::Json(body).into_response()
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn append_status_contracts(state: &RuntimeHttpState, body: &mut Value) {
    if let Some(map) = body.as_object_mut() {
        let enrollment = match state.enrollment_offer_issuer.read().await.clone() {
            Some(issuer) => match issuer.mint().await {
                Ok(offer) => crate::http::enrollment::EnrollmentStatus::Available {
                    token: offer.token,
                    offer: Box::new(offer.offer),
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
            gents_protocol::request_input::STATUS_INPUT_CONTRACT_FIELD.to_owned(),
            json!(gents_protocol::request_input::REQUEST_INPUT_CONTRACT_VERSION),
        );
        map.insert(
            gents_protocol::session_input_edit::STATUS_AUTHORED_QUEUE_EDIT_FIELD.to_owned(),
            json!(gents_protocol::session_input_edit::AUTHORED_QUEUE_EDIT_VERSION),
        );
        map.insert(
            gents_protocol::mailbox_question::STATUS_QUESTION_CONTRACT_FIELD.to_owned(),
            json!(gents_protocol::mailbox_question::MAILBOX_QUESTION_VERSION),
        );
        map.insert(
            gents_protocol::goal_control::STATUS_GOAL_STOP_CONTRACT_FIELD.to_owned(),
            json!(gents_protocol::goal_control::GOAL_STOP_CONTRACT_VERSION),
        );
        map.insert(
            gents::plugin::approval::workspace::STATUS_SUPPORT_FIELD.to_owned(),
            json!(gents::plugin::approval::workspace::support()),
        );
        map.insert(
            gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD.to_string(),
            replicated_schema_status(&state.replicated_schema),
        );
        map.insert(
            gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD.to_string(),
            json!(state.serve_lifecycle.current()),
        );
    }
}

fn replicated_schema_status(
    replicated_schema: &OnceCell<gents_protocol::peer_schema::ReplicatedSchema>,
) -> Value {
    replicated_schema
        .get()
        .map_or(Value::Null, |schema| json!(schema))
}

async fn self_handler(State(state): State<RuntimeHttpState>) -> Response {
    let (health, runtime, readiness, status_code) =
        match load_metrics_query_data(&state.p2p_graphql, &state.node_did).await {
            Ok(data) => {
                let data = with_local_native_executors(data);
                let health = render_healthz_payload(&state, Some(&data), None);
                let status_code = if health.get("ok") == Some(&Value::Bool(true)) {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                };
                let runtime = data
                    .node_runtimes
                    .iter()
                    .find(|runtime| runtime.node_did == state.node_did)
                    .cloned();
                let readiness = data
                    .node_readiness
                    .iter()
                    .find(|row| row.node_did == state.node_did)
                    .cloned();
                (health, runtime, readiness, status_code)
            }
            Err(error) => {
                let health = render_healthz_payload(&state, None, Some(error.to_string()));
                (health, None, None, StatusCode::SERVICE_UNAVAILABLE)
            }
        };

    match load_self_view(&state.p2p_graphql, &state.node_did).await {
        Ok((agents, context_budget, _context_indicator)) => {
            let body = render_self_payload(
                &state,
                &health,
                runtime.as_ref(),
                readiness.as_ref(),
                &agents,
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
                "node_name": state.node_name,
                "node_did": state.node_did,
                "process_state": "unknown",
                "agent": Value::Null,
                "agents": [],
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
    match load_session_history_snapshot(&state.p2p_graphql, &state.node_did, query.limit).await {
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
    readiness: Option<&gents_protocol::row::NodeReadinessRow>,
    agents: &[SelfAgent],
    context_budget: &ContextBudget,
) -> Value {
    let readiness = readiness.and_then(|row| {
        gents_protocol::row::decode_node_readiness_snapshot(row, &state.node_did).ok()
    });
    let process_state = readiness
        .as_ref()
        .map(|snapshot| snapshot.process_state.as_str())
        .unwrap_or("unknown");
    let agent_config = readiness
        .as_ref()
        .and_then(|snapshot| {
            agents
                .iter()
                .find(|agent_config| agent_config.agent_id == snapshot.default_agent_id)
        })
        .map(render_self_agent)
        .unwrap_or(Value::Null);
    let agents = agents.iter().map(render_self_agent).collect::<Vec<_>>();

    json!({
        "status": health.get("status").cloned().unwrap_or(Value::String("unknown".to_string())),
        "ok": health.get("ok").cloned().unwrap_or(Value::Bool(false)),
        "service": "gents",
        "version": version_response().version,
        "started_at": &state.started_at,
        "uptime_seconds": state.started_instant.elapsed().as_secs(),
        "graphql": &state.graphql,
        "node_name": &state.node_name,
        "node_did": &state.node_did,
        "process_state": process_state,
        "runtime": runtime,
        "agent": agent_config,
        "agents": agents,
        "context_budget": context_budget,
    })
}

fn render_self_agent(agent_config: &SelfAgent) -> Value {
    json!({
        "agent_id": &agent_config.agent_id,
        "display_name": &agent_config.display_name,
        "model_name": &agent_config.model_name,
        "enabled": agent_config.enabled,
        "backend_id": &agent_config.backend_id,
        "backend_provider": &agent_config.provider_kind,
        "backend_endpoint": &agent_config.endpoint,
        "inference_profile_id": &agent_config.inference_profile_id,
        "context_window": agent_config.context_window,
    })
}

async fn fleet_handler(State(state): State<RuntimeHttpState>) -> Response {
    match load_fleet_snapshot(&state.p2p_graphql).await {
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
    match load_fleet_slot_snapshot(&state.p2p_graphql).await {
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
    match load_mcp_pool_snapshot(&state.p2p_graphql, &state.node_did).await {
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
mod tests;
