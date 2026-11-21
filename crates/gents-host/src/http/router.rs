use gents_store::config_client::GraphqlEndpoint;
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
use crate::http::prometheus::{p2p::P2pMetricsSnapshot, StoreSizeCache};
use crate::http::version::version_response;
use accounts::{
    account_usage_read_handler, enrollment_decision_handler, enrollment_pending_handler,
};
use gents_service::state::P2pAdmissionState;
use metrics::{healthz_handler, metrics_handler};
use status::status_route;
pub use status::{BACKEND_PROBES_STATUS_FIELD, NATIVE_EXECUTORS_STATUS_FIELD};
use views::{fleet_handler, fleet_slots_handler, mcp_pool_handler, self_handler, sessions_handler};

mod accounts;
mod metrics;
mod status;
mod views;

#[derive(Clone)]
pub struct RuntimeHttpState {
    /// Address-only endpoint for public connection metadata.
    pub graphql: GraphqlEndpoint,
    /// Served principal; the node's P2P access control admits only it.
    pub p2p_graphql: GraphqlEndpoint,
    pub node_name: String,
    pub node_did: String,
    pub tool_ceiling: String,
    pub tool_root: Option<String>,
    pub tool_root_error: Option<String>,
    pub cli_tools: Vec<String>,
    pub home: Option<String>,
    pub started_at: String,
    pub started_instant: Instant,
    pub backend_health: Option<gents_inference::backends::health::BackendHealthMap>,
    pub p2p_admission: Option<P2pAdmissionState>,
    pub p2p_metrics_cache: Arc<Mutex<Option<P2pMetricsSnapshot>>>,
    pub store_size: Arc<StoreSizeCache>,
    pub enrollment_offer_issuer: EnrollmentOfferIssuerHandle,
    pub enrollment_decisions: EnrollmentDecisionServiceHandle,
    pub activation_runtime: Arc<OnceCell<gents::Gents>>,
    pub activation_observation: watch::Receiver<RuntimeActivationObservation>,
    pub replicated_schema: Arc<OnceCell<gents_protocol::peer_schema::ReplicatedSchema>>,
    pub serve_lifecycle: ServeLifecycleHandle,
    pub client_access_decisions: Arc<tokio::sync::Mutex<()>>,
    pub document_acp: gents_store::local::document_acp::DocumentAcp,
    /// The home's node key, which signs its status before the runtime that
    /// holds it too has started.
    pub node_identity: Option<Arc<dyn gents_store::identity::NodeIdentity>>,
    pub pack_watch_slots: Arc<tokio::sync::Semaphore>,
}

impl RuntimeHttpState {
    pub(crate) fn storage_observation(
        &self,
    ) -> gents_protocol::node_readiness::RuntimeStorageObservation {
        self.activation_runtime
            .get()
            .map(|runtime| runtime.storage_fault().observation())
            .unwrap_or_default()
    }
}

/// Ready only after serve writes `runtime.json` for this process; never
/// returns to starting.
#[derive(Clone, Debug, Default)]
pub struct ServeLifecycleHandle(Arc<std::sync::OnceLock<()>>);

impl ServeLifecycleHandle {
    pub fn mark_ready(&self) {
        let _ = self.0.set(());
    }

    pub fn current(&self) -> gents_protocol::serve_lifecycle::ServeLifecycle {
        if self.0.get().is_some() {
            gents_protocol::serve_lifecycle::ServeLifecycle::Ready
        } else {
            gents_protocol::serve_lifecycle::ServeLifecycle::Starting
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeActivationObservation {
    pub event: Option<(u64, String, Result<(), String>)>,
    pub router: Option<(u64, String)>,
}

impl RuntimeActivationObservation {
    pub fn successful_for(&self, generation: u64, fingerprint: &str) -> bool {
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
pub fn empty_activation_state() -> (
    Arc<OnceCell<gents::Gents>>,
    watch::Receiver<RuntimeActivationObservation>,
) {
    let (sender, receiver) = watch::channel(RuntimeActivationObservation::default());
    drop(sender);
    (Arc::new(OnceCell::new()), receiver)
}

pub struct RouterIdentity {
    pub graphql: String,
    pub p2p_graphql: GraphqlEndpoint,
    pub node_name: String,
    pub node_did: String,
    pub node_identity: Option<Arc<dyn gents_store::identity::NodeIdentity>>,
    pub document_acp: gents_store::local::document_acp::DocumentAcp,
}

pub struct ToolSurface {
    pub tool_ceiling: String,
    pub tool_root: Result<Option<String>, String>,
    pub cli_tools: Vec<String>,
    pub home: Option<String>,
    pub store_dir: Option<std::path::PathBuf>,
    pub mcp: Option<crate::http::mcp_server::McpServiceOptions>,
}

pub struct EnrollmentHandles {
    pub offer_issuer: EnrollmentOfferIssuerHandle,
    pub decisions: EnrollmentDecisionServiceHandle,
    pub p2p_admission: Option<P2pAdmissionState>,
}

pub struct RuntimeActivation {
    pub runtime: Arc<OnceCell<gents::Gents>>,
    pub observation: watch::Receiver<RuntimeActivationObservation>,
    pub replicated_schema: Arc<OnceCell<gents_protocol::peer_schema::ReplicatedSchema>>,
    pub serve_lifecycle: ServeLifecycleHandle,
}

pub fn runtime_contract_router(
    identity: RouterIdentity,
    tools: ToolSurface,
    enrollment: EnrollmentHandles,
    activation: RuntimeActivation,
    backend_health: Option<gents_inference::backends::health::BackendHealthMap>,
    pack_watch_limit: u32,
) -> RuntimeRouters {
    let RouterIdentity {
        graphql,
        p2p_graphql,
        node_name,
        node_did,
        node_identity,
        document_acp,
    } = identity;
    let ToolSurface {
        tool_ceiling,
        tool_root,
        cli_tools,
        home,
        store_dir,
        mcp,
    } = tools;
    let EnrollmentHandles {
        offer_issuer: enrollment_offer_issuer,
        decisions: enrollment_decisions,
        p2p_admission,
    } = enrollment;
    let RuntimeActivation {
        runtime: activation_runtime,
        observation: activation_observation,
        replicated_schema,
        serve_lifecycle,
    } = activation;
    let operator_did = node_did.clone();
    let graphql_for_mcp = graphql.clone();
    let state = RuntimeHttpState {
        client_access_decisions: Arc::new(tokio::sync::Mutex::new(())),
        p2p_graphql,
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
        store_size: StoreSizeCache::new(store_dir),
        enrollment_offer_issuer,
        enrollment_decisions,
        activation_runtime,
        activation_observation,
        replicated_schema,
        serve_lifecycle,
        document_acp,
        node_identity,
        pack_watch_slots: Arc::new(tokio::sync::Semaphore::new(pack_watch_limit as usize)),
    };

    let operator_routes = Router::new()
        .route("/packs/ux/watch", post(crate::http::pack_watch::watch))
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .route("/activation", get(activation_handler))
        .route("/self", get(self_handler))
        .route("/sessions", get(sessions_handler))
        .route("/fleet", get(fleet_handler))
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
        .route(
            "/packs/install",
            post(crate::http::pack_install::install_handler).layer(
                axum::extract::DefaultBodyLimit::max(crate::http::pack_install::MAX_BODY),
            ),
        )
        .route(
            "/packs/install/session",
            get(crate::http::pack_install::session::install_session),
        )
        .route("/version", get(version_handler))
        .route(
            gents_pack::pack::plugin_wire::PLUGIN_RECORD_PATH,
            get(crate::http::pack_record::plugin_record),
        )
        .route(
            gents_protocol::agent_identity::AGENT_IDENTITY_PATH,
            post(crate::http::agent_identity::provision),
        )
        .route(
            gents_protocol::agent_identity::AGENT_ADMISSION_PATH,
            post(crate::http::agent_identity::admission),
        )
        .route(
            gents_protocol::goal_control::APPLY_GOAL_CONTROL_PATH,
            post(crate::http::goal_control::apply),
        )
        .route(
            gents_protocol::client_access::REQUEST_PATH,
            post(crate::http::client_access::request),
        )
        .route(
            gents_protocol::client_access::DECIDE_PATH,
            post(crate::http::client_access::decide),
        )
        .route("/status", get(status_route))
        .route(
            gents_protocol::owed_settlement::OWED_SETTLEMENT_PATH,
            post(crate::http::owed_settlement::settle),
        )
        .route(
            gents_protocol::owed_settlement::OWED_REMOVALS_PATH,
            post(crate::http::owed_settlement::removals),
        )
        .route(
            gents_protocol::owed_settlement::OWED_AWAIT_PATH,
            post(crate::http::owed_settlement::await_owed),
        )
        .route("/fleet/slots", get(fleet_slots_handler))
        .route(
            gents_protocol::serve_lifecycle::control::RUNTIME_CONTROL_PATH,
            post(crate::http::runtime_control::runtime_control_handler),
        )
        .route(
            gents_inference::inference_observation::INFERENCE_ACTIVITY_PATH,
            post(crate::http::inference_activity::read),
        )
        .route(
            crate::http::inference_activity::BACKEND_DISCOVERY_PATH,
            post(crate::http::inference_activity::discover_backend),
        )
        .route("/enrollment/decisions", post(enrollment_decision_handler))
        .route("/enrollment/pending", post(enrollment_pending_handler))
        .route("/accounts/usage/read", post(account_usage_read_handler))
        .route(
            gents_observe::client_inference::INFERENCE_SETTINGS_PATH,
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

    RuntimeRouters {
        contract: router.with_state(state.clone()),
        scrape_state: state,
    }
}

/// The runtime API and the state a dedicated metrics listener serves from.
/// The scrape state is never routed on the runtime API listener.
pub struct RuntimeRouters {
    pub contract: Router,
    pub scrape_state: RuntimeHttpState,
}

/// Serves only the exposition; operator routes stay on the runtime API.
pub fn metrics_scrape_router(state: RuntimeHttpState) -> Router {
    Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(state)
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
                let readiness = match crate::readiness::load_live_node_readiness(
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
pub(crate) mod tests;
