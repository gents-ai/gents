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

/// [`http_get_json`] for the request `build` makes on `endpoint`, sent again
/// once when its credential is refused and the home's status recovers it.
pub async fn endpoint_get_json<T, F>(
    endpoint: &gents_store::config_client::GraphqlEndpoint,
    build: F,
) -> Result<T>
where
    T: DeserializeOwned,
    F: Fn(
        &gents_store::config_client::GraphqlEndpoint,
    ) -> Result<reqwest::RequestBuilder, gents_store::config_client::ConfigClientError>,
{
    let (url, body) = endpoint_send_bytes(endpoint, build).await?;
    serde_json::from_slice(&body).with_context(|| format!("decoding JSON response from {url}"))
}

/// [`http_send`] for the request `build` makes on `endpoint`, as
/// [`endpoint_get_json`] sends it.
pub async fn endpoint_send<F>(
    endpoint: &gents_store::config_client::GraphqlEndpoint,
    build: F,
) -> Result<()>
where
    F: Fn(
        &gents_store::config_client::GraphqlEndpoint,
    ) -> Result<reqwest::RequestBuilder, gents_store::config_client::ConfigClientError>,
{
    endpoint_send_bytes(endpoint, build).await.map(|_| ())
}

async fn endpoint_send_bytes<F>(
    endpoint: &gents_store::config_client::GraphqlEndpoint,
    build: F,
) -> Result<(reqwest::Url, Vec<u8>)>
where
    F: Fn(
        &gents_store::config_client::GraphqlEndpoint,
    ) -> Result<reqwest::RequestBuilder, gents_store::config_client::ConfigClientError>,
{
    let response = endpoint
        .send_resolving(build)
        .await?
        .with_context(|| format!("sending a request to {endpoint}"))?;
    successful_body(response).await
}

async fn successful_body(response: reqwest::Response) -> Result<(reqwest::Url, Vec<u8>)> {
    let url = response.url().clone();
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("reading the response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "{url} failed with {status}: {}",
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
