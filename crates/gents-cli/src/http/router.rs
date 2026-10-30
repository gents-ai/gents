use gents::config_client::GraphqlEndpoint;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde_json::json;
use tokio::sync::{watch, OnceCell};

use crate::http::enrollment::{EnrollmentDecisionServiceHandle, EnrollmentOfferIssuerHandle};
use crate::http::prometheus::P2pMetricsSnapshot;
use crate::http::version::version_response;
use crate::shared::P2pAdmissionState;
use accounts::{
    account_usage_read_handler, enrollment_decision_handler, enrollment_pending_handler,
};
use metrics::{healthz_handler, metrics_handler};
use status::status_route;
use views::{fleet_handler, fleet_slots_handler, mcp_pool_handler, self_handler, sessions_handler};

mod accounts;
mod metrics;
mod status;
mod views;

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

async fn version_handler() -> impl IntoResponse {
    axum::Json(version_response())
}

#[cfg(test)]
mod tests;
