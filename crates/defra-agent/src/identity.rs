use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use crypto::Key;
use defra_core::signing::{SigningConfig, SigningKeyType};
use identity::{FullIdentity as _, Identity as _, RawIdentity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccount {
    pub host_id: String,
    pub deployment_id: String,
}

#[async_trait]
pub trait AgentIdentity: Send + Sync {
    fn did(&self) -> &str;

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>>;

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool>;

    fn service_account(&self) -> Option<&ServiceAccount>;
}

#[derive(Debug, Clone)]
struct KnownPublicKey {
    key_type: crypto::KeyType,
    bytes: Vec<u8>,
}

fn known_public_keys() -> &'static RwLock<HashMap<String, KnownPublicKey>> {
    static TYPED_KEYS: OnceLock<RwLock<HashMap<String, KnownPublicKey>>> = OnceLock::new();
    TYPED_KEYS.get_or_init(|| RwLock::new(HashMap::new()))
}

#[derive(Debug)]
pub struct KeyIdentity {
    did: String,
    service_account: Option<ServiceAccount>,
    identity: Arc<RawIdentity>,
}

impl KeyIdentity {
    pub fn load_or_create(
        key_path: impl Into<PathBuf>,
        service_account: Option<ServiceAccount>,
    ) -> Result<Self> {
        let identity = Arc::new(load_or_create_identity(&key_path.into())?);
        let did = identity.did().map_err(anyhow::Error::from)?.to_string();
        register_public_key(&did, identity.key_type(), identity.public_key_bytes());
        Ok(Self {
            did,
            service_account,
            identity,
        })
    }
}

#[async_trait]
impl AgentIdentity for KeyIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        self.identity
            .sign(payload)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("signing payload for {}", self.did))
    }

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
        let public_key = if did == self.did {
            KnownPublicKey {
                key_type: self.identity.key_type(),
                bytes: self.identity.public_key_bytes(),
            }
        } else {
            known_public_key_for_did(did)?
        };

        verify_with_public_key(did, public_key, payload, signature)
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        self.service_account.as_ref()
    }
}

/// Agent identity backed by a DefraDB signing identity registered in this process.
///
/// DefraDB's identity registry is process-local. This adapter is for embedded
/// hosts that have already registered a local key or remote signer before
/// constructing the agent runtime; it is not a persistent identity loader for a
/// fresh `defra-agent server` process.
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
            .ok_or_else(|| anyhow!("no DefraDB signing identity registered for DID {did}"))?;
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
impl AgentIdentity for RegisteredIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        if let Some(signer) = self.config.remote_signer.clone() {
            let payload = payload.to_vec();
            let authorization = self.config.signing_authorization.clone();
            return tokio::task::spawn_blocking(move || {
                signer.sign_sync(&payload, authorization.as_ref())
            })
            .await
            .context("joining DefraDB remote signing task")?
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("signing payload for {}", self.did));
        }

        let identity = raw_identity_from_signing_config(&self.config)?;
        identity
            .sign(payload)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("signing payload for {}", self.did))
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

        verify_with_public_key(did, public_key, payload, signature)
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        self.service_account.as_ref()
    }
}

fn register_public_key(did: &str, key_type: crypto::KeyType, public_key: Vec<u8>) {
    known_public_keys()
        .write()
        .expect("known public keys lock poisoned")
        .insert(
            did.to_string(),
            KnownPublicKey {
                key_type,
                bytes: public_key,
            },
        );
}

fn known_public_key_for_did(did: &str) -> Result<KnownPublicKey> {
    known_public_keys()
        .read()
        .expect("known public keys lock poisoned")
        .get(did)
        .cloned()
        .ok_or_else(|| anyhow!("no public key registered for DID {did}"))
}

fn verify_with_public_key(
    did: &str,
    public_key: KnownPublicKey,
    payload: &[u8],
    signature: &[u8],
) -> Result<bool> {
    let public_key = crypto::public_key_from_bytes(public_key.key_type, &public_key.bytes)
        .map_err(anyhow::Error::from)?;
    public_key
        .verify(payload, signature)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("verifying payload for {did}"))
}

fn validate_registered_identity_config(did: &str, config: &SigningConfig) -> Result<()> {
    if !config.has_local_private_key() && !config.has_remote_signer() {
        anyhow::bail!("registered identity {did} has neither a local key nor a remote signer");
    }
    if config.public_key_bytes.is_empty() {
        anyhow::bail!("registered identity {did} has no public key bytes");
    }

    let public_key = crypto::public_key_from_bytes(
        signing_key_type_to_crypto_key_type(config.key_type)?,
        &config.public_key_bytes,
    )
    .map_err(anyhow::Error::from)
    .with_context(|| format!("loading public key for registered identity {did}"))?;
    let derived_did = public_key
        .did()
        .map_err(anyhow::Error::from)
        .with_context(|| format!("deriving DID for registered identity {did}"))?;
    if derived_did != did {
        anyhow::bail!("registered identity DID mismatch: expected {did}, derived {derived_did}");
    }

    Ok(())
}

fn raw_identity_from_signing_config(config: &SigningConfig) -> Result<RawIdentity> {
    if config.private_key_bytes.is_empty() {
        anyhow::bail!(
            "registered identity has no local private key and no remote signer was available"
        );
    }
    RawIdentity::from_bytes(
        signing_key_type_to_crypto_key_type(config.key_type)?,
        &config.private_key_bytes,
    )
    .map_err(anyhow::Error::from)
    .context("constructing identity from DefraDB signing config")
}

fn signing_key_type_to_crypto_key_type(key_type: SigningKeyType) -> Result<crypto::KeyType> {
    match key_type {
        SigningKeyType::Ed25519 => Ok(crypto::KeyType::Ed25519),
        SigningKeyType::Secp256k1 => Ok(crypto::KeyType::Secp256k1),
        SigningKeyType::Secp256r1 => Ok(crypto::KeyType::Secp256r1),
        SigningKeyType::Bls => anyhow::bail!(
            "BLS registered identities cannot be used as defra-agent runtime identities"
        ),
        other => anyhow::bail!(
            "registered identity key type {other} cannot be used as a defra-agent runtime identity"
        ),
    }
}

fn load_or_create_identity(path: &Path) -> Result<RawIdentity> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating key directory {}", parent.display()))?;
    }

    match std::fs::read(path) {
        Ok(bytes) => RawIdentity::from_bytes(crypto::KeyType::Ed25519, &bytes)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("loading identity from {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let private_key = crypto::generate_ed25519().map_err(anyhow::Error::from)?;
            let bytes = private_key.raw();
            std::fs::write(path, &bytes)
                .with_context(|| format!("persisting identity key to {}", path.display()))?;
            RawIdentity::from_private_key(private_key)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("constructing identity from {}", path.display()))
        }
        Err(error) => {
            Err(anyhow::Error::from(error)).with_context(|| format!("reading {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests;
