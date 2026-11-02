use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};

use super::RuntimeHttpState;
use crate::http::fleet::load_fleet_snapshot;
use crate::http::fleet_slots::load_fleet_slot_snapshot;
use crate::http::healthz::render_healthz_payload;
use crate::http::mcp_pool::load_mcp_pool_snapshot;
use crate::http::prometheus::{
    load_metrics_query_data, with_local_native_executors, MetricsRuntimeRow,
};
use crate::http::self_view::{load_self_view, ContextBudget, SelfAgent};
use crate::http::sessions::{load_session_history_snapshot, SessionHistoryParams};
use crate::http::version::version_response;

pub(super) async fn self_handler(State(state): State<RuntimeHttpState>) -> Response {
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

pub(super) async fn sessions_handler(
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

pub(super) async fn fleet_handler(State(state): State<RuntimeHttpState>) -> Response {
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

pub(super) async fn fleet_slots_handler(State(state): State<RuntimeHttpState>) -> Response {
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

pub(super) async fn mcp_pool_handler(State(state): State<RuntimeHttpState>) -> Response {
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
