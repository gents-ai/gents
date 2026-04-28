use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{Context, Result};
use async_trait::async_trait;
use crypto::keys::PublicKey;
use crypto::Key;
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

fn known_public_keys() -> &'static RwLock<HashMap<String, Vec<u8>>> {
    static KEYS: OnceLock<RwLock<HashMap<String, Vec<u8>>>> = OnceLock::new();
    KEYS.get_or_init(|| RwLock::new(HashMap::new()))
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
        register_public_key(&did, identity.public_key_bytes());
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
            crypto::Ed25519PublicKey::from_bytes(&self.identity.public_key_bytes())
                .map_err(anyhow::Error::from)?
        } else {
            let keys = known_public_keys()
                .read()
                .expect("known public keys lock poisoned");
            let bytes = keys
                .get(did)
                .ok_or_else(|| anyhow::anyhow!("no public key registered for DID {did}"))?;
            crypto::Ed25519PublicKey::from_bytes(bytes).map_err(anyhow::Error::from)?
        };

        public_key
            .verify(payload, signature)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("verifying payload for {did}"))
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        self.service_account.as_ref()
    }
}

fn register_public_key(did: &str, public_key: Vec<u8>) {
    known_public_keys()
        .write()
        .expect("known public keys lock poisoned")
        .insert(did.to_string(), public_key);
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
