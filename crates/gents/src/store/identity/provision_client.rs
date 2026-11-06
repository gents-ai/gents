use crate::store::config_client::{ConfigClientError, GraphqlEndpoint};
use gents_protocol::agent_identity::{
    AgentIdentityBinding, ProvisionAgentIdentityRequest, AGENT_IDENTITY_PATH,
};

#[derive(Debug, thiserror::Error)]
pub enum RemoteAgentProvisionError {
    #[error("runtime endpoint is invalid")]
    InvalidEndpoint(#[source] <reqwest::Url as std::str::FromStr>::Err),
    #[error("runtime endpoint must be an HTTP URL without credentials")]
    UnsafeEndpoint,
    #[error("runtime endpoint has no GraphQL path")]
    MissingGraphqlPath,
    #[error(transparent)]
    Authorization(#[from] ConfigClientError),
    #[error("agent identity request failed")]
    Request(#[from] reqwest::Error),
    #[error("agent identity response exceeds limit")]
    ResponseTooLarge,
    #[error("invalid agent identity response")]
    InvalidResponse(#[source] serde_json::Error),
}

pub async fn provision(
    endpoint: &GraphqlEndpoint,
    node_did: &str,
) -> std::result::Result<AgentIdentityBinding, RemoteAgentProvisionError> {
    let mut url =
        reqwest::Url::parse(endpoint.url()).map_err(RemoteAgentProvisionError::InvalidEndpoint)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(RemoteAgentProvisionError::UnsafeEndpoint);
    }
    let base = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .ok_or(RemoteAgentProvisionError::MissingGraphqlPath)?;
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
    if response.content_length().is_some_and(|size| size > 4096) {
        return Err(RemoteAgentProvisionError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > 4096 - bytes.len() {
            return Err(RemoteAgentProvisionError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(RemoteAgentProvisionError::InvalidResponse)
}
