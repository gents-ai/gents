use std::path::Path;

use serde::Deserialize;

use super::{LocalRuntimeError, Result};

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
    let bytes = std::fs::read(path).map_err(|source| LocalRuntimeError::ReadJsonFile {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| LocalRuntimeError::DecodeJsonFile {
        path: path.to_path_buf(),
        source,
    })
}

pub(super) fn p2p_api_base(graphql: &str) -> Result<String> {
    graphql
        .trim()
        .strip_suffix("/graphql")
        .map(ToOwned::to_owned)
        .ok_or_else(|| LocalRuntimeError::NotGraphqlEndpoint {
            endpoint: graphql.to_owned(),
        })
}

pub(super) async fn http_get_json<T>(client: &reqwest::Client, url: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let response = client.get(url).send().await.map_err(|source| {
        if source.is_connect() {
            LocalRuntimeError::NoServerFound {
                url: url.to_owned(),
                source,
            }
        } else {
            LocalRuntimeError::SendGet {
                url: url.to_owned(),
                source,
            }
        }
    })?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|source| LocalRuntimeError::ReadGetBody {
            url: url.to_owned(),
            source,
        })?;
    if !status.is_success() {
        return Err(LocalRuntimeError::GetStatus {
            url: url.to_owned(),
            status,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    }
    serde_json::from_slice(&body).map_err(|source| LocalRuntimeError::DecodeGetJson {
        url: url.to_owned(),
        source,
    })
}
