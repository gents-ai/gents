use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

pub async fn http_get_json<T>(request: reqwest::RequestBuilder) -> Result<T>
where
    T: DeserializeOwned,
{
    let (url, body) = http_send_bytes(request).await?;
    serde_json::from_slice(&body).with_context(|| format!("decoding JSON response from {url}"))
}

pub async fn http_send(request: reqwest::RequestBuilder) -> Result<()> {
    http_send_bytes(request).await.map(|_| ())
}

async fn http_send_bytes(request: reqwest::RequestBuilder) -> Result<(reqwest::Url, Vec<u8>)> {
    let (client, request) = request.build_split();
    let request = request.context("building HTTP request")?;
    let method = request.method().clone();
    let url = request.url().clone();
    let response = client
        .execute(request)
        .await
        .with_context(|| format!("sending {method} request to {url}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("reading {method} response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "{method} {url} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok((url, body.to_vec()))
}

pub fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(ToOwned::to_owned)
}
