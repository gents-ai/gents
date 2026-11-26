use std::time::Duration;

use futures::FutureExt as _;
use gents_protocol::delegation_audience::{DelegationAudience, HomeClock};
use vera_delegation::{DelegationSigner as _, Ed25519Signer};

use super::ClientSigningCapability;

/// `Identity.UserDelegation.lifetimeSeconds`.
pub const VERA_DELEGATION_LIFETIME: Duration =
    Duration::from_secs(gents_protocol::delegation_audience::DELEGATION_LIFETIME_SECS);

/// `Identity.UserDelegation.requestBoundLifetimeSeconds`.
pub const VERA_REQUEST_BOUND_LIFETIME: Duration = Duration::from_secs(570);

#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    #[error("this device's key cannot be used to connect to this home")]
    Signer(#[source] vera_delegation::SignerError),
    #[error("this device's key belongs to {signer}, not to {actor}")]
    Actor { signer: String, actor: String },
    #[error("preparing this device's connection to the home")]
    Mint(#[source] vera_delegation::BearerError),
    #[error("this device's key did not answer")]
    Pending,
    #[error("this home's runtime is still starting; try again in a moment")]
    AudienceUnavailable,
    #[error("preparing this change for the home")]
    Command(#[source] vera_delegation::BearerError),
}

/// The general delegation `capability`'s User presents to `audience`, on a
/// device whose clock reads `clock` and that read its home's clock as `home`
/// (`Identity.UserDelegation.mint`).
pub(super) fn mint(
    capability: &ClientSigningCapability,
    audience: &DelegationAudience,
    home: Option<&HomeClock>,
    clock: u64,
) -> Result<String, DelegationError> {
    mint_for(
        capability,
        audience,
        Duration::from_secs(gents_protocol::delegation_audience::general_lifetime(
            home, clock,
        )),
    )
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

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub fn mint_expired_for_test(
    capability: &ClientSigningCapability,
    audience: &DelegationAudience,
) -> Result<String, DelegationError> {
    let signer = signer_of(capability)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            DelegationError::Mint(vera_delegation::BearerError::Time(error.to_string()))
        })?
        .as_secs();
    let issued_at = now.saturating_sub(3600);
    vera_delegation::bearer::create_scoped_bearer_token_at(
        &signer,
        &vera_delegation::did_spelling::wire(signer.did()),
        &audience.worker,
        audience.deployment,
        60,
        vera_delegation::DelegationScope::PolicyCommands,
        issued_at,
    )
    .now_or_never()
    .ok_or(DelegationError::Pending)?
    .map_err(DelegationError::Mint)
}

fn mint_for(
    capability: &ClientSigningCapability,
    audience: &DelegationAudience,
    lifetime: Duration,
) -> Result<String, DelegationError> {
    let signer = signer_of(capability)?;
    vera_delegation::caller_delegation(&signer, &provider_audience(audience)?, lifetime)
        .now_or_never()
        .ok_or(DelegationError::Pending)?
        .map_err(DelegationError::Mint)
}

/// The audience a request-bound delegation is minted for and the provider's
/// time it is issued at (`Identity.UserDelegation.issueAt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestBoundAudience {
    pub audience: DelegationAudience,
    pub issued_at: u64,
}

/// The delegation `capability`'s User binds to the one policy command
/// `command` on `bound`'s deployment, at the one minting point
/// (`Identity.UserDelegation.mint`). Refused while its audience names no
/// genesis identity.
pub(crate) fn mint_request_bound(
    capability: &ClientSigningCapability,
    bound: &RequestBoundAudience,
    command: &vera_delegation::PolicyCommand<'_>,
    lifetime: Duration,
) -> Result<String, DelegationError> {
    let signer = signer_of(capability)?;
    let audience = provider_audience(&bound.audience)?;
    if audience.genesis_id.is_none() {
        return Err(DelegationError::AudienceUnavailable);
    }
    let binding = vera_delegation::RequestBinding::for_command(
        &audience,
        command,
        rand::random::<[u8; 24]>(),
    )
    .map_err(DelegationError::Command)?;
    vera_delegation::request_delegation_at(&signer, &audience, &binding, lifetime, bound.issued_at)
        .now_or_never()
        .ok_or(DelegationError::Pending)?
        .map_err(DelegationError::Mint)
}

fn signer_of(capability: &ClientSigningCapability) -> Result<Ed25519Signer, DelegationError> {
    let signer = Ed25519Signer::from_slice(&capability.config.private_key_bytes)
        .map_err(DelegationError::Signer)?;
    if signer.did() != capability.did() {
        return Err(DelegationError::Actor {
            signer: signer.did().to_owned(),
            actor: capability.did().to_owned(),
        });
    }
    Ok(signer)
}

pub(crate) fn provider_audience(
    audience: &DelegationAudience,
) -> Result<acp::DelegationAudience, DelegationError> {
    let genesis_id = match &audience.genesis_id {
        None => None,
        Some(text) => {
            let mut bytes = [0_u8; 32];
            hex::decode_to_slice(text.strip_prefix("0x").unwrap_or(text), &mut bytes)
                .map_err(|_| DelegationError::AudienceUnavailable)?;
            Some(bytes)
        }
    };
    Ok(acp::DelegationAudience {
        worker: audience.worker.clone(),
        deployment: audience.deployment,
        genesis_id,
    })
}
