use std::time::Duration;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
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

pub(super) async fn status_handler(State(state): State<RuntimeHttpState>) -> Response {
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

pub(super) async fn status_route(
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
