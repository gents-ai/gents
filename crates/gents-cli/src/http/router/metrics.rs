use std::time::Duration;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::Value;

use super::RuntimeHttpState;
use crate::http::healthz::render_healthz_payload;
use crate::http::prometheus::{
    load_metrics_query_data, render_prometheus_metrics, with_local_native_executors,
    P2pMetricsSnapshot,
};
use crate::shared::P2pAdmissionState;

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
pub(super) const P2P_METRICS_FETCH_BUDGET: Duration = Duration::from_millis(750);

pub(super) async fn metrics_handler(State(state): State<RuntimeHttpState>) -> Response {
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

pub(super) async fn load_p2p_metrics_for_scrape(state: &RuntimeHttpState) -> P2pMetricsSnapshot {
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

pub(super) async fn healthz_handler(State(state): State<RuntimeHttpState>) -> Response {
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

pub(super) fn p2p_metrics_from_status(
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
