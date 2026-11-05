use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::router::RuntimeHttpState;
use crate::commands::pack::wire::{wire_in, WireRequest};

pub(crate) const MAX_BODY: usize = 64 * 1024;
const PATH: &str = "/packs/wire";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireCommand {
    node_did: String,
    request: WireRequest,
}

pub(crate) async fn wire_remote(
    endpoint: &gents::store::config_client::GraphqlEndpoint,
    owner: &str,
    request: &WireRequest,
) -> anyhow::Result<Value> {
    let mut url = reqwest::Url::parse(endpoint.url())?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "pack wiring requires an HTTP URL without credentials"
    );
    let prefix = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .ok_or_else(|| anyhow::anyhow!("pack wiring requires a Gents runtime GraphQL endpoint"))?;
    url.set_path(&format!("{prefix}{PATH}"));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let response = endpoint
        .authorize(client.post(url))?
        .json(&WireCommand {
            node_did: owner.to_owned(),
            request: request.clone(),
        })
        .send()
        .await
        .map_err(|error| {
            anyhow::anyhow!(
            "pack wiring outcome is unconfirmed; inspect the agent's Tools before retrying: {error}"
        )
        })?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|error| {
        anyhow::anyhow!(
        "pack wiring response was unreadable; inspect the agent's Tools before retrying: {error}"
    )
    })?;
    anyhow::ensure!(
        status.is_success(),
        "pack wiring returned HTTP {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
        "pack wiring response was unreadable; inspect the agent's Tools before retrying: {error}"
    )
    })
}

pub(crate) async fn wire_handler(
    State(state): State<RuntimeHttpState>,
    Json(command): Json<WireCommand>,
) -> Response {
    if command.node_did != state.node_did {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "endpoint serves a different node"})),
        )
            .into_response();
    }
    let Some(runtime) = state.activation_runtime.get() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "runtime is not active"})),
        )
            .into_response();
    };
    let Some(home) = state.home.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "runtime plugin store unavailable"})),
        )
            .into_response();
    };
    let access = runtime.operator_config_access();
    let owner = state.node_did;
    let home = std::path::PathBuf::from(home);
    let request = command.request;
    // Dropping the HTTP client must not release the store lock before the embedded commit completes.
    match tokio::spawn(async move { wire_in(&access, &owner, &home, &request).await }).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({"error": format!("{error:#}"), "outcome": "unconfirmed"}))).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("pack wiring outcome unconfirmed: {error}"), "outcome": "unconfirmed"}))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Router};
    use std::sync::{Arc, Mutex};

    fn request() -> WireRequest {
        WireRequest {
            plugin: "acme/tool".into(),
            agent: "assistant".into(),
            targets_from: vec!["acme/pack".into()],
            remove: false,
            preview: false,
        }
    }

    #[tokio::test]
    async fn remote_wire_sends_one_bounded_request_to_runtime() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new().route(
            "/packs/wire",
            post({
                let received = received.clone();
                move |Json(command): Json<WireCommand>| {
                    let received = received.clone();
                    async move {
                        received.lock().unwrap().push(command);
                        Json(serde_json::json!({"wired": true}))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let endpoint = gents::store::config_client::GraphqlEndpoint::anonymous(format!(
            "http://{address}/api/v0/graphql"
        ));
        let result = wire_remote(&endpoint, "did:key:owner", &request())
            .await
            .unwrap();
        assert_eq!(result["wired"], true);
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].node_did, "did:key:owner");
        assert_eq!(received[0].request.plugin, "acme/tool");
        assert_eq!(received[0].request.targets_from, ["acme/pack"]);
        server.abort();
    }

    #[tokio::test]
    async fn lost_response_is_unconfirmed_and_is_not_retried() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
        });
        let endpoint = gents::store::config_client::GraphqlEndpoint::anonymous(format!(
            "http://{address}/api/v0/graphql"
        ));
        let error = wire_remote(&endpoint, "did:key:owner", &request())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("outcome is unconfirmed"));
        server.await.unwrap();
    }
}
