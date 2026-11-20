use std::time::Duration;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use gents_store::config_client::GraphqlEndpoint;
use serde_json::{json, Value};
use tokio::sync::OnceCell;

use super::metrics::P2P_METRICS_FETCH_BUDGET;
use super::RuntimeHttpState;
use crate::http::healthz::render_healthz_payload;
use crate::http::prometheus::{load_metrics_core_data, with_local_native_executors};
use crate::http::self_view::load_self_view;
use crate::http::version::version_response;

/// Bounds core health so `/status` identity fields return before a caller's
/// own timeout while the node is still opening.
pub(super) const STATUS_PROBE_BUDGET: Duration = Duration::from_secs(2);

/// Private status, read as `reader`: the authorized caller's own forwarded
/// bearer (HttpAccess.lean `readsAs`).
pub(super) async fn status_handler(
    State(state): State<RuntimeHttpState>,
    reader: &GraphqlEndpoint,
    nonce: Option<&gents_protocol::home_attestation::HomeNonce>,
) -> Response {
    let mut p2p = match tokio::time::timeout(
        P2P_METRICS_FETCH_BUDGET,
        crate::p2p_status::load_live_http_p2p_status(
            state.home.as_deref().map(std::path::Path::new),
            reader,
        ),
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
        load_metrics_core_data(reader, &state.node_did),
    )
    .await;
    let metrics = match metrics {
        Ok(Ok(core)) => Ok(Ok(core
            .with_liveness_activity(reader, probe_deadline)
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
                "storage": health.pointer("/checks/storage").cloned().unwrap_or(Value::Null),
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
        if let Ok(Ok((agents, context_budget, context))) =
            tokio::time::timeout(STATUS_PROBE_BUDGET, load_self_view(reader, &state.node_did)).await
        {
            if let Some(map) = body.as_object_mut() {
                map.insert("agents".to_string(), json!(agents));
                map.insert("context".to_string(), json!(context));
                map.insert("context_budget".to_string(), json!(context_budget));
            }
        }
    }

    if let Some(map) = body.as_object_mut() {
        crate::p2p_status::flatten_p2p_fields(map, &p2p);
        if let Some(health) = &state.backend_health {
            map.insert(
                BACKEND_PROBES_STATUS_FIELD.to_string(),
                backend_probe_reports(&health.snapshot().await),
            );
        }
    }
    append_status_contracts(&state, &mut body).await;
    attest_status(&state, nonce, &mut body).await;
    (StatusCode::OK, axum::Json(body)).into_response()
}

async fn attest_status(
    state: &RuntimeHttpState,
    nonce: Option<&gents_protocol::home_attestation::HomeNonce>,
    body: &mut Value,
) {
    let Some(nonce) = nonce else {
        return;
    };
    let identity = state.node_identity.clone().or_else(|| {
        state
            .activation_runtime
            .get()
            .map(|runtime| runtime.runtime_node().identity.clone())
    });
    gents_store::identity::home_status::attest(identity.as_deref(), nonce, body).await;
}

/// This node's measured backend probes; the replicated `InferenceBackend`
/// document never carries them (#640), so a failed probe's reason is read here.
pub const BACKEND_PROBES_STATUS_FIELD: &str = "backend_probes";

fn backend_probe_reports(
    snapshot: &rapidhash::RapidHashMap<
        String,
        gents_inference::backends::health::BackendHealthSnapshot,
    >,
) -> Value {
    let mut reports = snapshot.values().collect::<Vec<_>>();
    reports.sort_by(|a, b| a.backend_id.cmp(&b.backend_id));
    Value::Array(
        reports
            .into_iter()
            .map(|probe| probe.probe_report())
            .collect(),
    )
}

/// The status, signed by the node key over the nonce a client sent in
/// `home_attestation::HOME_NONCE_QUERY` when it sent one
/// (`Identity.HeldAudience.authentic`); without one it is for display only.
pub(super) async fn status_route(
    State(state): State<RuntimeHttpState>,
    identity: defra_http::ExtractIdentity,
    headers: axum::http::HeaderMap,
    query: axum::extract::Query<rapidhash::RapidHashMap<String, String>>,
) -> Response {
    let nonce = match query
        .get(gents_protocol::home_attestation::HOME_NONCE_QUERY)
        .map(|nonce| gents_protocol::home_attestation::HomeNonce::parse(nonce))
        .transpose()
    {
        Ok(nonce) => nonce,
        Err(refusal) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({ "error": format!("invalid home nonce: {refusal}") })),
            )
                .into_response()
        }
    };
    let reader =
        crate::http::private_reader::authorize_private_reader(&state, &identity, &headers).await;
    let mut response = if let Ok(reader) = reader {
        status_handler(State(state), &reader, nonce.as_ref()).await
    } else {
        let mut body = json!({
            "service": "gents",
            "version": version_response().version,
            "node_name": state.node_name,
            "node_did": state.node_did,
            "graphql": state.graphql,
        });
        append_status_contracts(&state, &mut body).await;
        attest_status(&state, nonce.as_ref(), &mut body).await;
        axum::Json(body).into_response()
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn append_status_contracts(state: &RuntimeHttpState, body: &mut Value) {
    let authentication = match state.activation_runtime.get() {
        Some(runtime) => gents_store::config_client::client_authentication::runtime_authentication(
            &runtime.operator_config_access(),
        )
        .await
        .unwrap_or(gents_protocol::client_authentication::ClientAuthentication::Unavailable),
        None => match state.document_acp {
            gents_store::local::document_acp::DocumentAcp::Local => {
                gents_protocol::client_authentication::ClientAuthentication::Local
            }
            gents_store::local::document_acp::DocumentAcp::Vera => {
                gents_protocol::client_authentication::ClientAuthentication::Unavailable
            }
        },
    };
    if let Some(map) = body.as_object_mut() {
        map.insert(
            gents_protocol::client_authentication::STATUS_FIELD.to_owned(),
            json!(authentication),
        );
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
                reason: crate::http::enrollment::unavailable_reason(state.document_acp),
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
            gents_tools::plugin::approval::workspace::STATUS_SUPPORT_FIELD.to_owned(),
            json!(gents_tools::plugin::approval::workspace::support()),
        );
        map.insert(
            gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD.to_string(),
            replicated_schema_status(&state.replicated_schema),
        );
        map.insert(
            "non_canonical_lineages".to_string(),
            non_canonical_lineages_status(&state.replicated_schema),
        );
        map.insert(
            gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD.to_string(),
            json!(state.serve_lifecycle.current()),
        );
        let audience = state.activation_runtime.get().and_then(|runtime| {
            gents_store::local::document_acp::node_delegation_audience(runtime.observation_node())
        });
        if let Some(provider_time) = state.activation_runtime.get().and_then(|runtime| {
            gents_store::local::document_acp::node_provider_time(runtime.observation_node())
        }) {
            map.insert(
                gents_protocol::delegation_audience::STATUS_PROVIDER_TIME_FIELD.to_owned(),
                json!(provider_time),
            );
        }
        if let Some(reported) =
            gents_store::local::document_acp::status_reports(state.document_acp, audience)
                .status_value()
        {
            map.insert(
                gents_protocol::delegation_audience::STATUS_DELEGATION_AUDIENCE_FIELD.to_owned(),
                reported,
            );
        }
    }
}

/// Replicated collections whose active version differs from the canonical one, so peers
/// and apps on the canonical lineage cannot sync them with this node.
fn non_canonical_lineages_status(
    replicated_schema: &OnceCell<gents_protocol::peer_schema::ReplicatedSchema>,
) -> Value {
    let Some(schema) = replicated_schema.get() else {
        return Value::Null;
    };
    gents_store::schema::migration::non_canonical_lineages(
        schema
            .iter()
            .map(|(name, identity)| (name.as_str(), identity.version_id.as_str())),
    )
    .map_or(Value::Null, |mismatches| json!(mismatches))
}

fn replicated_schema_status(
    replicated_schema: &OnceCell<gents_protocol::peer_schema::ReplicatedSchema>,
) -> Value {
    replicated_schema
        .get()
        .map_or(Value::Null, |schema| json!(schema))
}
