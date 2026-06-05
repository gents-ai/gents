use std::path::Path;

use anyhow::{Context, Result};
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};

use crate::client::PrincipalIdentity;
use crate::remote_admin::http_impl::{
    hex_encode, signing_payload, ACTOR_DID_HEADER, ACTOR_SIGNATURE_HEADER, ACTOR_SIGNATURE_VERSION,
    ACTOR_SIGNATURE_VERSION_HEADER,
};

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

pub(super) async fn http_post_json_signed<B: Serialize>(
    client: &reqwest::Client,
    url: &str,
    path: &str,
    body: &B,
    actor: &PrincipalIdentity,
) -> Result<()> {
    let body = serde_json::to_vec(body).with_context(|| format!("encoding POST body for {url}"))?;
    let signature = actor
        .sign(&signing_payload("POST", path, &body))
        .with_context(|| format!("signing POST request to {url}"))?;
    let response = client
        .post(url)
        .header(ACTOR_DID_HEADER, actor.did())
        .header(ACTOR_SIGNATURE_HEADER, hex_encode(&signature))
        .header(ACTOR_SIGNATURE_VERSION_HEADER, ACTOR_SIGNATURE_VERSION)
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .with_context(|| format!("sending signed POST request to {url}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("reading signed POST response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "signed POST {url} failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(())
}
