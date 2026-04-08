use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use async_trait::async_trait;
use crypto::keys::PublicKey;
use crypto::Key;
use identity::{FullIdentity as _, RawIdentity};
use tokio::sync::RwLock;

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
pub struct SimpleIdentity {
    did: String,
    key_path: PathBuf,
    service_account: Option<ServiceAccount>,
    identity: tokio::sync::Mutex<Option<Arc<RawIdentity>>>,
}

impl SimpleIdentity {
    pub fn new(
        profile_name: impl Into<String>,
        key_path: impl Into<PathBuf>,
        service_account: Option<ServiceAccount>,
    ) -> Self {
        let profile_name = profile_name.into();
        Self {
            did: format!("did:defra-agent:{profile_name}"),
            key_path: key_path.into(),
            service_account,
            identity: tokio::sync::Mutex::new(None),
        }
    }

    async fn load_identity(&self) -> Result<Arc<RawIdentity>> {
        if let Some(identity) = self.identity.lock().await.clone() {
            return Ok(identity);
        }

        let mut guard = self.identity.lock().await;
        if let Some(identity) = guard.clone() {
            return Ok(identity);
        }

        let identity = Arc::new(load_or_create_identity(&self.key_path).await?);
        register_public_key(&self.did, identity.public_key_bytes()).await;
        *guard = Some(identity.clone());
        Ok(identity)
    }
}

#[async_trait]
impl AgentIdentity for SimpleIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        let identity = self.load_identity().await?;
        identity
            .sign(payload)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("signing payload for {}", self.did))
    }

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
        let public_key = if did == self.did {
            let identity = self.load_identity().await?;
            crypto::Ed25519PublicKey::from_bytes(&identity.public_key_bytes())
                .map_err(anyhow::Error::from)?
        } else {
            let keys = known_public_keys().read().await;
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

async fn register_public_key(did: &str, public_key: Vec<u8>) {
    known_public_keys()
        .write()
        .await
        .insert(did.to_string(), public_key);
}

async fn load_or_create_identity(path: &Path) -> Result<RawIdentity> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating key directory {}", parent.display()))?;
    }

    match tokio::fs::read(path).await {
        Ok(bytes) => RawIdentity::from_bytes(crypto::KeyType::Ed25519, &bytes)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("loading identity from {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let private_key = crypto::generate_ed25519().map_err(anyhow::Error::from)?;
            let bytes = private_key.raw();
            tokio::fs::write(path, &bytes)
                .await
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn simple_identity_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("amy-general.key");
        let identity = SimpleIdentity::new("amy-general", &path, None);
        let payload = b"hello world";

        let signature = identity.sign(payload).await.unwrap();
        assert!(identity
            .verify(identity.did(), payload, &signature)
            .await
            .unwrap());

        let second = SimpleIdentity::new("amy-general", path, None);
        assert_eq!(identity.did(), second.did());
        assert!(second
            .verify(identity.did(), payload, &signature)
            .await
            .unwrap());
    }
}
