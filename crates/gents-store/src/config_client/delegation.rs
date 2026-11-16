use std::time::Duration;

use futures::FutureExt as _;
use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use vera::vera_rs::{DelegationSigner as _, Ed25519Signer};

use super::{ClientSigningCapability, GraphqlEndpoint};

/// `Identity.UserDelegation.lifetimeSeconds`.
pub const VERA_DELEGATION_LIFETIME: Duration = Duration::from_secs(120);

const STATUS_PATH: &str = "/status";
const GRAPHQL_PATH: &str = "/api/v0/graphql";
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const STATUS_LIMIT: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    #[error("the held client key cannot sign a Vera delegation")]
    Signer(#[source] vera::vera_rs::SignerError),
    #[error("the held client key signs as {signer}, not as {actor}")]
    Actor { signer: String, actor: String },
    #[error("minting a Vera delegation")]
    Mint(#[source] vera::vera_rs::BearerError),
    #[error("the Vera delegation signer did not sign at once")]
    Pending,
    #[error(
        "the runtime decides document access through Vera but did not report the worker and \
         deployment a delegation must name; retry once it is ready"
    )]
    AudienceUnavailable,
    #[error("{url} is not a Gents runtime GraphQL endpoint")]
    StatusEndpoint { url: String },
    #[error("reading the runtime's status")]
    Status(#[source] reqwest::Error),
    #[error("the runtime's status is larger than {STATUS_LIMIT} bytes")]
    StatusTooLarge,
    #[error("decoding the runtime's status")]
    StatusResponse(#[source] serde_json::Error),
}

pub(super) fn mint(
    capability: &ClientSigningCapability,
    audience: &DelegationAudience,
) -> Result<String, DelegationError> {
    mint_for(capability, audience, VERA_DELEGATION_LIFETIME)
}

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub fn mint_with_lifetime_for_test(
    capability: &ClientSigningCapability,
    audience: &DelegationAudience,
    lifetime: Duration,
) -> Result<String, DelegationError> {
    mint_for(capability, audience, lifetime)
}

fn mint_for(
    capability: &ClientSigningCapability,
    audience: &DelegationAudience,
    lifetime: Duration,
) -> Result<String, DelegationError> {
    let signer = Ed25519Signer::from_slice(&capability.config.private_key_bytes)
        .map_err(DelegationError::Signer)?;
    if signer.did() != capability.did() {
        return Err(DelegationError::Actor {
            signer: signer.did().to_owned(),
            actor: capability.did().to_owned(),
        });
    }
    let audience = acp::DelegationAudience {
        worker: audience.worker.clone(),
        deployment: audience.deployment,
    };
    vera::vera_rs::caller_delegation(&signer, &audience, lifetime)
        .now_or_never()
        .ok_or(DelegationError::Pending)?
        .map_err(DelegationError::Mint)
}

fn status_url(endpoint: &GraphqlEndpoint) -> Result<reqwest::Url, DelegationError> {
    let refused = || DelegationError::StatusEndpoint {
        url: endpoint.url().to_owned(),
    };
    let mut url = reqwest::Url::parse(endpoint.url().trim()).map_err(|_| refused())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(refused());
    }
    let path = format!(
        "{}{STATUS_PATH}",
        url.path().strip_suffix(GRAPHQL_PATH).ok_or_else(refused)?
    );
    url.set_path(&path);
    Ok(url)
}

/// What the runtime serving `endpoint` reports in its public status. A node
/// that serves no Gents status is not a Gents runtime and reports nothing.
pub(super) async fn reported(
    endpoint: &GraphqlEndpoint,
) -> Result<ReportedDelegationAudience, DelegationError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(STATUS_TIMEOUT)
        .build()
        .map_err(DelegationError::Status)?;
    let mut response = client
        .get(status_url(endpoint)?)
        .send()
        .await
        .map_err(DelegationError::Status)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(ReportedDelegationAudience::Absent);
    }
    response = response
        .error_for_status()
        .map_err(DelegationError::Status)?;
    let mut response_bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(DelegationError::Status)? {
        if chunk.len() > STATUS_LIMIT - response_bytes.len() {
            return Err(DelegationError::StatusTooLarge);
        }
        response_bytes.extend_from_slice(&chunk);
    }
    let status: serde_json::Value =
        serde_json::from_slice(&response_bytes).map_err(DelegationError::StatusResponse)?;
    Ok(ReportedDelegationAudience::observe(&status))
}
