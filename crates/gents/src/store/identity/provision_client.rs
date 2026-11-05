use crate::store::config_client::GraphqlEndpoint;
use anyhow::{Context, Result};
use gents_protocol::agent_identity::{
    AgentIdentityBinding, ProvisionAgentIdentityRequest, AGENT_IDENTITY_PATH,
};

pub async fn provision(endpoint: &GraphqlEndpoint, node_did: &str) -> Result<AgentIdentityBinding> {
    let mut url = reqwest::Url::parse(endpoint.url()).context("runtime endpoint is invalid")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "runtime endpoint must be an HTTP URL without credentials"
    );
    let base = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .context("runtime endpoint has no GraphQL path")?;
    let path = format!("{base}{AGENT_IDENTITY_PATH}");
    url.set_path(&path);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut response = endpoint
        .authorize(client.post(url).json(&ProvisionAgentIdentityRequest {
            node_did: node_did.to_owned(),
        }))?
        .send()
        .await?
        .error_for_status()?;
    anyhow::ensure!(
        response.content_length().is_none_or(|size| size <= 4096),
        "agent identity response exceeds limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= 4096,
            "agent identity response exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("invalid agent identity response")
}
