use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

pub(super) async fn peer_status(
    record: &crate::client::PeerRecord,
) -> Result<Option<serde_json::Value>> {
    let http =
        |endpoint: &&str| endpoint.starts_with("http://") || endpoint.starts_with("https://");
    let endpoint = record
        .graphql
        .as_deref()
        .map(str::trim)
        .filter(http)
        .or_else(|| Some(record.addr.trim()).filter(http));
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };
    let url = super::runtime_status_url(endpoint)?;
    let client = super::runtime_http_client().await?;
    http_get_json(client, &url).await.map(Some)
}

pub(super) fn read_json<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("decoding {}", path.display()))
}

pub(super) fn p2p_api_base(graphql: &str) -> Result<String> {
    graphql
        .trim()
        .strip_suffix("/graphql")
        .map(ToOwned::to_owned)
        .with_context(|| format!("expected GraphQL endpoint ending in /graphql, got {graphql}"))
}

pub(super) async fn http_get_json<T>(client: &reqwest::Client, url: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let response = client.get(url).send().await.map_err(|error| {
        if error.is_connect() {
            anyhow::Error::from(error).context(format!(
                "no gents server found at {url}. Start one first with \
                 `gents server`. Remote runtimes must be added \
                 through authenticated status enrollment in the desktop app."
            ))
        } else {
            anyhow::Error::from(error).context(format!("sending GET request to {url}"))
        }
    })?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("reading GET response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "GET {url} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    serde_json::from_slice(&body).with_context(|| format!("decoding JSON response from {url}"))
}
