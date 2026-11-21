use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use gents_pack::pack::registry::split_pack_coordinate;

use super::router::RuntimeHttpState;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordQuery {
    pub plugin: String,
}

/// The installed plugin record a client wiring over this runtime checks
/// against a pack it approved (`Configuration.wirePlanSubject`): read-only,
/// for the served Node or a native admin, as a private snapshot is.
pub async fn plugin_record(
    State(state): State<RuntimeHttpState>,
    identity: defra_http::ExtractIdentity,
    headers: HeaderMap,
    Query(query): Query<RecordQuery>,
) -> Response {
    if let Err(refused) =
        crate::http::private_reader::authorize_private_reader(&state, &identity, &headers).await
    {
        return *refused;
    }
    let Some(home) = state.home.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "runtime plugin store unavailable"})),
        )
            .into_response();
    };
    let plugin = query.plugin;
    let read = tokio::task::spawn_blocking(move || {
        let (namespace, name) = split_pack_coordinate(&plugin);
        gents_pack::plugin::store::read_record(std::path::Path::new(&home), namespace, name)
    })
    .await;
    let mut response = match read {
        Ok(Ok(record)) => Json(record).into_response(),
        Ok(Err(error)) if gents_pack::plugin::store::is_not_found(&error) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "the plugin is not installed on this runtime"})),
        )
            .into_response(),
        Ok(Err(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{error:#}")})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
