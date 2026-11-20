use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

pub(super) async fn peer_status(
    record: &gents_protocol::runtime_connection::RuntimeConnection,
) -> Result<Option<serde_json::Value>> {
    let Some(url) = peer_status_url(record)? else {
        return Ok(None);
    };
    let client = super::runtime_http_client().await?;
    http_get_json(client, &url).await.map(Some)
}

pub(super) fn peer_status_url(
    record: &gents_protocol::runtime_connection::RuntimeConnection,
) -> Result<Option<String>> {
    super::runtime_status_url(&record.graphql).map(Some)
}

pub(super) fn read_json<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("decoding {}", path.display()))
}

pub(super) async fn http_get_json<T>(client: &reqwest::Client, url: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let response = client.get(url).send().await.map_err(|error| {
        let message = if error.is_connect() {
            format!(
                "no gents server found at {url}. Start one first with \
                 `gents runtime server`. Remote runtimes must be added \
                 through authenticated status enrollment in the desktop app."
            )
        } else {
            format!("sending GET request to {url}")
        };
        gents_store::client_error::BridgeError::runtime_http_error(error, message)
    })?;
    let status = response.status();
    let body = response.bytes().await.map_err(|error| {
        gents_store::client_error::BridgeError::runtime_http_error(
            error,
            format!("reading GET response body from {url}"),
        )
    })?;
    if !status.is_success() {
        anyhow::bail!(
            "GET {url} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    serde_json::from_slice(&body).with_context(|| format!("decoding JSON response from {url}"))
}
