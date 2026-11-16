use std::time::Duration;

use futures::FutureExt as _;
use gents_protocol::delegation_audience::DelegationAudience;
use vera::vera_rs::{DelegationSigner as _, Ed25519Signer};

use super::ClientSigningCapability;

/// `Identity.UserDelegation.lifetimeSeconds`.
pub const VERA_DELEGATION_LIFETIME: Duration = Duration::from_secs(120);

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
