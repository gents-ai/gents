use async_trait::async_trait;
use defra_core::signing::{SigningConfig, SigningKeyType};
use identity::{FullIdentity as _, RawIdentity};

use super::{
    known_public_key_for_did, register_public_key, verify_with_public_key, IdentityError,
    KnownPublicKey, NodeIdentity, Result, ServiceAccount,
};

pub struct RegisteredIdentity {
    did: String,
    service_account: Option<ServiceAccount>,
    config: SigningConfig,
}

impl RegisteredIdentity {
    pub fn from_registered_did(
        did: impl Into<String>,
        service_account: Option<ServiceAccount>,
    ) -> Result<Self> {
        let did = did.into();
        let config = defra_core::signing::get_identity(&did)
            .ok_or_else(|| IdentityError::NoDefraSigningIdentity { did: did.clone() })?;
        validate_registered_identity_config(&did, &config)?;
        register_public_key(
            &did,
            signing_key_type_to_crypto_key_type(config.key_type)?,
            config.public_key_bytes.clone(),
        );
        Ok(Self {
            did,
            service_account,
            config,
        })
    }
}

impl std::fmt::Debug for RegisteredIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredIdentity")
            .field("did", &self.did)
            .field("key_type", &self.config.key_type)
            .field(
                "has_local_private_key",
                &self.config.has_local_private_key(),
            )
            .field("has_remote_signer", &self.config.has_remote_signer())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl NodeIdentity for RegisteredIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        if let Some(signer) = self.config.remote_signer.clone() {
            let payload = payload.to_vec();
            let authorization = self.config.signing_authorization.clone();
            let signed = tokio::task::spawn_blocking(move || {
                signer.sign_sync(&payload, authorization.as_ref())
            })
            .await
            .map_err(IdentityError::JoinRemoteSigning)?
            .map_err(|error| IdentityError::RemoteSignFailed {
                did: self.did.clone(),
                error,
            })?;
            return Ok(signed);
        }

        let identity = raw_identity_from_signing_config(&self.config)?;
        Ok(identity
            .sign(payload)
            .map_err(|source| IdentityError::SignPayload {
                did: self.did.clone(),
                source,
            })?)
    }

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
        let public_key = if did == self.did {
            KnownPublicKey {
                key_type: signing_key_type_to_crypto_key_type(self.config.key_type)?,
                bytes: self.config.public_key_bytes.clone(),
            }
        } else {
            known_public_key_for_did(did)?
        };

        Ok(verify_with_public_key(did, public_key, payload, signature)?)
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        self.service_account.as_ref()
    }
}

fn validate_registered_identity_config(did: &str, config: &SigningConfig) -> Result<()> {
    if !config.has_local_private_key() && !config.has_remote_signer() {
        return Err(IdentityError::NoLocalOrRemoteSigner {
            did: did.to_owned(),
        });
    }
    if config.public_key_bytes.is_empty() {
        return Err(IdentityError::NoRegisteredPublicKey {
            did: did.to_owned(),
        });
    }

    let public_key = crypto::public_key_from_bytes(
        signing_key_type_to_crypto_key_type(config.key_type)?,
        &config.public_key_bytes,
    )
    .map_err(|source| IdentityError::LoadRegisteredPublicKey {
        did: did.to_owned(),
        source,
    })?;
    let derived_did = public_key
        .did()
        .map_err(|source| IdentityError::DeriveRegisteredDid {
            did: did.to_owned(),
            source,
        })?;
    if derived_did != did {
        return Err(IdentityError::RegisteredDidMismatch {
            did: did.to_owned(),
            derived_did,
        });
    }

    Ok(())
}

pub(super) fn raw_identity_from_signing_config(config: &SigningConfig) -> Result<RawIdentity> {
    if config.private_key_bytes.is_empty() {
        return Err(IdentityError::NoLocalPrivateKey);
    }
    RawIdentity::from_bytes(
        signing_key_type_to_crypto_key_type(config.key_type)?,
        &config.private_key_bytes,
    )
    .map_err(IdentityError::IdentityFromSigningConfig)
}

fn signing_key_type_to_crypto_key_type(key_type: SigningKeyType) -> Result<crypto::KeyType> {
    match key_type {
        SigningKeyType::Ed25519 => Ok(crypto::KeyType::Ed25519),
        SigningKeyType::Secp256k1 => Ok(crypto::KeyType::Secp256k1),
        SigningKeyType::Secp256r1 => Ok(crypto::KeyType::Secp256r1),
        SigningKeyType::BlsAugV1 => Err(IdentityError::BlsRegistered),
        other => Err(IdentityError::UnsupportedRegisteredKeyType { key_type: other }),
    }
}
