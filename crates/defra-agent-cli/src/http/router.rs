use std::time::Instant;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::http::healthz::render_healthz_payload;
use crate::http::prometheus::{load_metrics_query_data, render_prometheus_metrics};
use crate::http::version::version_response;

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

#[derive(Clone)]
pub(crate) struct RuntimeHttpState {
    pub(crate) graphql: String,
    pub(crate) started_at: String,
    pub(crate) started_instant: Instant,
}

pub(crate) fn runtime_contract_router(graphql: String) -> Router {
    let state = RuntimeHttpState {
        graphql,
        started_at: chrono::Utc::now().to_rfc3339(),
        started_instant: Instant::now(),
    };

    Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/version", get(version_handler))
        .route("/healthz", get(healthz_handler))
        .with_state(state)
}

async fn metrics_handler(State(state): State<RuntimeHttpState>) -> Response {
    match render_prometheus_metrics(&state.graphql).await {
        Ok(body) => ([(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)], body).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("metrics render failed: {error}"),
        )
            .into_response(),
    }
}

async fn version_handler() -> impl IntoResponse {
    axum::Json(version_response())
}

async fn healthz_handler(State(state): State<RuntimeHttpState>) -> Response {
    match load_metrics_query_data(&state.graphql).await {
        Ok(data) => {
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
