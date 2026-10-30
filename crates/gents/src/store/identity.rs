use rapidhash::{HashMapExt, RapidHashMap};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use defra_core::signing::{SigningConfig, SigningKeyType};
use identity::{FullIdentity as _, Identity as _, RawIdentity};

mod error;
mod file_key;
pub(crate) mod macos;
mod registered;

pub use error::{identity_key_unavailable, IdentityError, IdentityKeyUnavailable};
pub use file_key::{load_file_identity, load_or_create_file_identity, InsecureKeyPermissions};
pub(crate) use file_key::{publish_private_key_file, read_private_key_file};
pub use macos::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity,
    load_or_create_macos_keychain_identity, load_or_create_macos_secure_enclave_identity,
};
use registered::raw_identity_from_signing_config;
pub use registered::RegisteredIdentity;

type Result<T, E = IdentityError> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccount {
    pub host_id: String,
    pub deployment_id: String,
}

#[derive(Clone)]
pub struct RuntimeNode {
    pub node_did: String,
    pub identity: Arc<dyn NodeIdentity>,
    pub default_agent_id: String,
    pub display_name: Option<String>,
    pub enabled: bool,
}

impl std::fmt::Debug for RuntimeNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeNode")
            .field("node_did", &self.node_did)
            .field("default_agent_id", &self.default_agent_id)
            .field("display_name", &self.display_name)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait NodeIdentity: Send + Sync {
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

fn known_public_keys() -> &'static RwLock<RapidHashMap<String, KnownPublicKey>> {
    static TYPED_KEYS: OnceLock<RwLock<RapidHashMap<String, KnownPublicKey>>> = OnceLock::new();
    TYPED_KEYS.get_or_init(|| RwLock::new(RapidHashMap::new()))
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
        Self::from_raw_identity(
            load_or_create_file_identity(&key_path.into())?,
            service_account,
        )
    }

    /// Opens only an existing key; initialized homes must not mint a
    /// replacement when their recorded key disappears.
    pub fn load_existing(
        key_path: impl Into<PathBuf>,
        service_account: Option<ServiceAccount>,
    ) -> Result<Self> {
        Self::from_raw_identity(load_file_identity(&key_path.into())?, service_account)
    }

    fn from_raw_identity(
        identity: RawIdentity,
        service_account: Option<ServiceAccount>,
    ) -> Result<Self> {
        let identity = Arc::new(identity);
        let did = identity.did()?.to_string();
        let public_key_bytes = identity.public_key_bytes();
        register_public_key(&did, identity.key_type(), public_key_bytes.clone());
        register_ed25519_signing_identity(&did, &identity.private_key_bytes(), &public_key_bytes)?;
        Ok(Self {
            did,
            service_account,
            identity,
        })
    }
}

pub fn register_ed25519_signing_identity(
    did: &str,
    private_key_bytes: &[u8],
    public_key_bytes: &[u8],
) -> Result<()> {
    let identity = RawIdentity::from_bytes(crypto::KeyType::Ed25519, private_key_bytes)
        .map_err(IdentityError::LoadNodeSigning)?;
    let derived_did = identity.did()?.to_string();
    if derived_did != did {
        return Err(IdentityError::NodeDidMismatch {
            did: did.to_owned(),
            derived_did,
        });
    }
    if identity.public_key_bytes() != public_key_bytes {
        return Err(IdentityError::NodePublicKeyMismatch {
            did: did.to_owned(),
        });
    }

    defra_core::signing::store_identity(
        did,
        SigningConfig {
            key_type: SigningKeyType::Ed25519,
            private_key_bytes: SigningConfig::private_key_bytes_from_vec(
                private_key_bytes.to_vec(),
            ),
            public_key_bytes: public_key_bytes.to_vec(),
            public_key_hex: lowercase_hex(public_key_bytes),
            remote_signer: None,
            signing_authorization: None,
        },
    );
    Ok(())
}

/// Identity string DefraDB stores in commit signature headers for a
/// `did:key` principal: the hexadecimal public key, not the DID.
pub fn commit_signer_identity_for_did(did: &str) -> Result<String> {
    let (_, public_key_bytes) =
        crypto::parse_did_key(did).map_err(|source| IdentityError::ParseCommitSignerDid {
            did: did.to_owned(),
            source,
        })?;
    Ok(lowercase_hex(&public_key_bytes))
}

/// DefraDB tolerates up to 60s of clock skew and its bearers carry no nonce,
/// so a replayed bearer stays usable for at most two minutes.
const DEFRADB_BEARER_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

/// Mint an `Authorization` header authenticating one DefraDB HTTP request as
/// `did`. DefraDB verifies the JWT against the request's `Host` header, so
/// `audience` must be the `host[:port]` the request is sent to. A DID whose
/// private key bytes this process did not register is refused, not silently
/// downgraded to an anonymous request.
pub fn defradb_bearer_authorization(did: &str, audience: &str) -> Result<String> {
    let config = defra_core::signing::get_identity(did).ok_or_else(|| {
        IdentityError::SigningIdentityNotLoaded {
            did: did.to_owned(),
        }
    })?;
    let identity = raw_identity_from_signing_config(&config).map_err(|source| {
        IdentityError::LoadSigningIdentity {
            did: did.to_owned(),
            source: Box::new(source),
        }
    })?;
    let token = identity::new_token(
        &identity,
        DEFRADB_BEARER_LIFETIME,
        Some(audience.to_string()),
        None,
    )
    .map_err(|source| IdentityError::MintBearer {
        did: did.to_owned(),
        source,
    })?;
    let token = String::from_utf8(token).map_err(IdentityError::BearerNotUtf8)?;
    Ok(format!("Bearer {token}"))
}

pub fn can_mint_defradb_bearer(did: &str) -> bool {
    defra_core::signing::get_identity(did).is_some_and(|config| config.has_local_private_key())
}

/// P2P administration must carry the node DID explicitly across worker boundaries.
pub fn node_p2p(
    node: &defra_node::EmbeddedNode,
) -> Option<Arc<dyn defra_p2p_adapter::P2POperations>> {
    let context = defra_core::RequestContext::default()
        .with_identity(node.node_identity_did().map(ToOwned::to_owned));
    node.p2p_arc().and_then(|p2p| p2p.bind_request(&context))
}

#[async_trait]
impl NodeIdentity for KeyIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        Ok(self
            .identity
            .sign(payload)
            .map_err(|source| IdentityError::SignPayload {
                did: self.did.clone(),
                source,
            })?)
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

        Ok(verify_with_public_key(did, public_key, payload, signature)?)
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

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn known_public_key_for_did(did: &str) -> Result<KnownPublicKey> {
    if let Some(key) = known_public_keys()
        .read()
        .expect("known public keys lock poisoned")
        .get(did)
        .cloned()
    {
        return Ok(key);
    }

    if did.starts_with("did:key:") {
        let (key_type, bytes) =
            crypto::parse_did_key(did).map_err(|source| IdentityError::ParseDidKey {
                did: did.to_owned(),
                source,
            })?;
        tracing::trace!(
            did,
            ?key_type,
            "resolved public key from did:key (not in local registry)"
        );
        return Ok(KnownPublicKey { key_type, bytes });
    }

    Err(IdentityError::NoPublicKey {
        did: did.to_owned(),
    })
}

fn verify_with_public_key(
    did: &str,
    public_key: KnownPublicKey,
    payload: &[u8],
    signature: &[u8],
) -> Result<bool> {
    let public_key = crypto::public_key_from_bytes(public_key.key_type, &public_key.bytes)?;
    public_key
        .verify(payload, signature)
        .map_err(|source| IdentityError::VerifyPayload {
            did: did.to_owned(),
            source,
        })
}

pub(crate) fn verify_did_signature(did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
    verify_with_public_key(did, known_public_key_for_did(did)?, payload, signature)
}

#[cfg(test)]
mod tests;
