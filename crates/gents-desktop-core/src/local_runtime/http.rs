use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

pub(super) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
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

pub(super) async fn http_get_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let response = client.get(url).send().await.map_err(|error| {
        if error.is_connect() {
            anyhow::anyhow!(
                "no gents server found at {url}. Start one first with \
                 `gents server` or `gents demo`. Remote runtimes must be added \
                 through authenticated status enrollment in the desktop app."
            )
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
