use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("sending GET request to {url}"))?;
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

pub(super) async fn http_post_json<B: Serialize>(
    client: &reqwest::Client,
    url: &str,
    body: &B,
) -> Result<()> {
    let response = client
        .post(url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("sending POST request to {url}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("reading POST response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "POST {url} failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(())
}
