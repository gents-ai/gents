use rapidhash::{HashMapExt, RapidHashMap};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
#[cfg(target_os = "macos")]
use defra_core::signing::RemoteSigner;
use defra_core::signing::{SigningConfig, SigningKeyType};
use identity::{FullIdentity as _, Identity as _, RawIdentity};

mod file_key;

pub use file_key::{load_file_identity, load_or_create_file_identity, InsecureKeyPermissions};
pub(crate) use file_key::{publish_private_key_file, read_private_key_file};

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error(transparent)]
    InsecureKey(#[from] InsecureKeyPermissions),
    #[error("identity key does not exist at {}", path.display())]
    KeyMissing { path: PathBuf },
    #[error("constructing identity from {}", path.display())]
    ConstructIdentity {
        path: PathBuf,
        #[source]
        source: identity::Error,
    },
    #[error("loading identity from {}", path.display())]
    LoadIdentity {
        path: PathBuf,
        #[source]
        source: identity::Error,
    },
    #[error("staging key for {}", path.display())]
    StageKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("securing staged key for {}", path.display())]
    SecureStagedKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing staged key for {}", path.display())]
    WriteStagedKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("syncing staged key for {}", path.display())]
    SyncStagedKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("persisting key to {}", path.display())]
    PersistKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("opening key {}", path.display())]
    OpenKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading key {}", path.display())]
    ReadKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("inspecting key {}", path.display())]
    InspectKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("key {} is not a regular file", path.display())]
    KeyNotRegular { path: PathBuf },
    #[error("creating key directory {}", path.display())]
    CreateKeyDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Crypto(#[from] crypto::Error),
    #[error(transparent)]
    Identity(#[from] identity::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("node signing DID mismatch: expected {did}, derived {derived_did}")]
    NodeDidMismatch { did: String, derived_did: String },
    #[error("node signing public key does not match DID {did}")]
    NodePublicKeyMismatch { did: String },
    #[error("loading Ed25519 node signing identity")]
    LoadNodeSigning(#[source] identity::Error),
    #[error("parsing commit signer DID {did}")]
    ParseCommitSignerDid {
        did: String,
        #[source]
        source: crypto::Error,
    },
    #[error("no signing identity is loaded for {did}")]
    SigningIdentityNotLoaded { did: String },
    #[error("loading signing identity {did}")]
    LoadSigningIdentity {
        did: String,
        #[source]
        source: Box<IdentityError>,
    },
    #[error("minting DefraDB bearer token for {did}")]
    MintBearer {
        did: String,
        #[source]
        source: identity::Error,
    },
    #[error("DefraDB bearer token is not UTF-8")]
    BearerNotUtf8(#[source] std::string::FromUtf8Error),
    #[error("signing payload for {did}")]
    SignPayload {
        did: String,
        #[source]
        source: crypto::Error,
    },
    #[error("no DefraDB signing identity registered for DID {did}")]
    NoDefraSigningIdentity { did: String },
    #[error("macos-secure-enclave identity backend is only available on macOS")]
    SecureEnclaveUnavailable,
    #[error("macos-keychain identity backend is only available on macOS")]
    KeychainUnavailable,
    #[error("no public key registered for DID {did}")]
    NoPublicKey { did: String },
    #[error("parsing did:key public key from DID {did}")]
    ParseDidKey {
        did: String,
        #[source]
        source: crypto::Error,
    },
    #[error("verifying payload for {did}")]
    VerifyPayload {
        did: String,
        #[source]
        source: crypto::Error,
    },
    #[error("registered identity {did} has neither a local key nor a remote signer")]
    NoLocalOrRemoteSigner { did: String },
    #[error("registered identity {did} has no public key bytes")]
    NoRegisteredPublicKey { did: String },
    #[error("loading public key for registered identity {did}")]
    LoadRegisteredPublicKey {
        did: String,
        #[source]
        source: crypto::Error,
    },
    #[error("deriving DID for registered identity {did}")]
    DeriveRegisteredDid {
        did: String,
        #[source]
        source: crypto::Error,
    },
    #[error("registered identity DID mismatch: expected {did}, derived {derived_did}")]
    RegisteredDidMismatch { did: String, derived_did: String },
    #[error("registered identity has no local private key and no remote signer was available")]
    NoLocalPrivateKey,
    #[error("constructing identity from DefraDB signing config")]
    IdentityFromSigningConfig(#[source] identity::Error),
    #[error("signing payload for {did}: {error}")]
    RemoteSignFailed { did: String, error: String },
    #[error("joining DefraDB remote signing task")]
    JoinRemoteSigning(#[source] tokio::task::JoinError),
    #[error("BLS registered identities cannot be used as gents runtime identities")]
    BlsRegistered,
    #[error("registered identity key type {key_type} cannot be used as a gents runtime identity")]
    UnsupportedRegisteredKeyType { key_type: SigningKeyType },
    #[error("macOS keychain identity not found for label {label}")]
    KeychainIdentityNotFound { label: String },
    #[error("loading macOS keychain identity {label}")]
    LoadKeychainIdentity {
        label: String,
        #[source]
        source: identity::Error,
    },
    #[error("storing macOS keychain identity {label}")]
    StoreKeychainIdentity {
        label: String,
        #[source]
        source: Box<IdentityError>,
    },
    #[error("constructing macOS keychain identity {label}")]
    ConstructKeychainIdentity {
        label: String,
        #[source]
        source: identity::Error,
    },
    #[error("reading macOS keychain identity {label}: macOS Keychain error {code}")]
    KeychainReadFailed { label: String, code: i32 },
    #[error("verifying deletion of Keychain item {label}: macOS Keychain error {code}")]
    KeychainVerifyFailed { label: String, code: i32 },
    #[error("Keychain item {label} is still present after deletion; its custody record must be retained")]
    KeychainStillPresent { label: String },
    #[cfg(target_os = "macos")]
    #[error("loading default macOS keychain")]
    DefaultKeychain(#[source] security_framework::base::Error),
    #[cfg(target_os = "macos")]
    #[error(transparent)]
    Keychain(#[from] security_framework::base::Error),
    #[error("macOS Secure Enclave key not found for label {label}")]
    SecureEnclaveKeyNotFound { label: String },
    #[error("loading public key for Secure Enclave key {label}")]
    SecureEnclavePublicKey {
        label: String,
        #[source]
        source: Box<IdentityError>,
    },
    #[error("deriving DID for Secure Enclave key {label}")]
    SecureEnclaveDid {
        label: String,
        #[source]
        source: Box<IdentityError>,
    },
    #[error("multiple keychain private keys found for label {label}")]
    MultipleSecureEnclaveKeys { label: String },
    #[cfg(target_os = "macos")]
    #[error("searching keychain for macOS Secure Enclave key label {label}")]
    SearchSecureEnclaveKey {
        label: String,
        #[source]
        source: security_framework::base::Error,
    },
    #[cfg(target_os = "macos")]
    #[error("creating Secure Enclave key access control")]
    SecureEnclaveAccessControl(#[source] security_framework::base::Error),
    #[error("creating macOS Secure Enclave key label {label}: {error}")]
    CreateSecureEnclaveKey { label: String, error: String },
    #[error("macOS Secure Enclave key label {label} requires a codesigned binary with Data Protection keychain access-group entitlements")]
    SecureEnclaveEntitlement { label: String },
    #[error("Secure Enclave key has no public key")]
    SecureEnclaveNoPublicKey,
    #[error("Secure Enclave public key is not exportable")]
    SecureEnclaveNotExportable,
}

impl IdentityError {
    pub fn insecure_key(&self) -> Option<&InsecureKeyPermissions> {
        match self {
            Self::InsecureKey(key) => Some(key),
            _ => None,
        }
    }
}

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

    async fn sign(&self, payload: &[u8]) -> anyhow::Result<Vec<u8>>;

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> anyhow::Result<bool>;

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

    async fn sign(&self, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
        Ok(self
            .identity
            .sign(payload)
            .map_err(|source| IdentityError::SignPayload {
                did: self.did.clone(),
                source,
            })?)
    }

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> anyhow::Result<bool> {
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

pub fn load_or_create_macos_secure_enclave_identity(
    label: &str,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    register_macos_secure_enclave_identity(label, true, service_account)
}

pub fn load_macos_secure_enclave_identity(
    label: &str,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    register_macos_secure_enclave_identity(label, false, service_account)
}

pub fn load_or_create_macos_keychain_identity(
    label: &str,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    register_macos_keychain_identity(label, true, service_account)
}

pub fn load_macos_keychain_identity(
    label: &str,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    register_macos_keychain_identity(label, false, service_account)
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

    async fn sign(&self, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
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

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> anyhow::Result<bool> {
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

/// The only Keychain status that proves an item is absent; every other
/// failure leaves the item possibly intact.
pub(crate) const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
#[cfg(target_os = "macos")]
const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34018;

#[cfg(target_os = "macos")]
fn register_macos_secure_enclave_identity(
    label: &str,
    create_if_missing: bool,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    let signer = MacosSecureEnclaveSigner::load(label, create_if_missing)?;
    let did = signer.did.clone();
    let public_key_bytes = signer.public_key_bytes.clone();
    defra_core::signing::store_identity(
        &did,
        SigningConfig {
            key_type: SigningKeyType::Secp256r1,
            private_key_bytes: Vec::new(),
            public_key_bytes: public_key_bytes.clone(),
            public_key_hex: lowercase_hex(&public_key_bytes),
            remote_signer: Some(Arc::new(signer)),
            signing_authorization: None,
        },
    );
    RegisteredIdentity::from_registered_did(did, service_account)
}

#[cfg(not(target_os = "macos"))]
fn register_macos_secure_enclave_identity(
    _label: &str,
    _create_if_missing: bool,
    _service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    Err(IdentityError::SecureEnclaveUnavailable)
}

#[cfg(target_os = "macos")]
fn register_macos_keychain_identity(
    label: &str,
    create_if_missing: bool,
    service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    let identity = load_or_create_macos_keychain_raw_identity(label, create_if_missing)?;
    let did = identity.did()?.to_string();
    let public_key_bytes = identity.public_key_bytes();
    defra_core::signing::store_identity(
        &did,
        SigningConfig {
            key_type: SigningKeyType::Ed25519,
            private_key_bytes: SigningConfig::private_key_bytes_from_vec(
                identity.private_key_bytes(),
            ),
            public_key_bytes: public_key_bytes.clone(),
            public_key_hex: lowercase_hex(&public_key_bytes),
            remote_signer: None,
            signing_authorization: None,
        },
    );
    RegisteredIdentity::from_registered_did(did, service_account)
}

#[cfg(not(target_os = "macos"))]
fn register_macos_keychain_identity(
    _label: &str,
    _create_if_missing: bool,
    _service_account: Option<ServiceAccount>,
) -> Result<RegisteredIdentity> {
    Err(IdentityError::KeychainUnavailable)
}

#[cfg(target_os = "macos")]
fn load_or_create_macos_keychain_raw_identity(
    label: &str,
    create_if_missing: bool,
) -> Result<RawIdentity> {
    use crypto::Key as _;

    match macos_keychain_find(MACOS_KEYCHAIN_SERVICE, label)? {
        Ok(password) => {
            RawIdentity::from_bytes(crypto::KeyType::Ed25519, &password).map_err(|source| {
                IdentityError::LoadKeychainIdentity {
                    label: label.to_owned(),
                    source,
                }
            })
        }
        Err(ERR_SEC_ITEM_NOT_FOUND) => {
            if !create_if_missing {
                return Err(IdentityError::KeychainIdentityNotFound {
                    label: label.to_owned(),
                });
            }
            let private_key = crypto::generate_ed25519()?;
            let bytes = private_key.raw();
            macos_keychain_set(MACOS_KEYCHAIN_SERVICE, label, bytes).map_err(|source| {
                IdentityError::StoreKeychainIdentity {
                    label: label.to_owned(),
                    source: Box::new(source),
                }
            })?;
            RawIdentity::from_private_key(private_key).map_err(|source| {
                IdentityError::ConstructKeychainIdentity {
                    label: label.to_owned(),
                    source,
                }
            })
        }
        Err(code) => Err(IdentityError::KeychainReadFailed {
            label: label.to_owned(),
            code,
        }),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_keychain_find(
    service: &str,
    label: &str,
) -> Result<std::result::Result<k256::elliptic_curve::zeroize::Zeroizing<Vec<u8>>, i32>> {
    let keychain = security_framework::os::macos::keychain::SecKeychain::default()
        .map_err(IdentityError::DefaultKeychain)?;
    Ok(match keychain.find_generic_password(service, label) {
        Ok((password, _)) => Ok(k256::elliptic_curve::zeroize::Zeroizing::new(
            password.as_ref().to_vec(),
        )),
        Err(error) => Err(error.code()),
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_keychain_set(service: &str, label: &str, secret: &[u8]) -> Result<()> {
    security_framework::os::macos::keychain::SecKeychain::default()
        .map_err(IdentityError::DefaultKeychain)?
        .set_generic_password(service, label, secret)
        .map_err(IdentityError::Keychain)
}

/// `security-framework` discards `SecKeychainItemDelete`'s status, so
/// deletion is complete only after a fresh lookup returns `errSecItemNotFound`.
#[cfg(target_os = "macos")]
pub(crate) fn macos_keychain_delete(service: &str, label: &str) -> Result<()> {
    let keychain = security_framework::os::macos::keychain::SecKeychain::default()
        .map_err(IdentityError::DefaultKeychain)?;
    match keychain.find_generic_password(service, label) {
        Ok((_, item)) => {
            item.delete();
            match macos_keychain_find(service, label)? {
                Err(ERR_SEC_ITEM_NOT_FOUND) => Ok(()),
                Err(code) => Err(IdentityError::KeychainVerifyFailed {
                    label: label.to_owned(),
                    code,
                }),
                Ok(_) => Err(IdentityError::KeychainStillPresent {
                    label: label.to_owned(),
                }),
            }
        }
        Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
        Err(error) => Err(IdentityError::Keychain(error)),
    }
}

#[cfg(target_os = "macos")]
const MACOS_KEYCHAIN_SERVICE: &str = "com.source-inc.gents.identity";

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct MacosSecureEnclaveSigner {
    label: String,
    key: security_framework::key::SecKey,
    did: String,
    public_key_bytes: Vec<u8>,
}

#[cfg(target_os = "macos")]
impl MacosSecureEnclaveSigner {
    fn load(label: &str, create_if_missing: bool) -> Result<Self> {
        let key = match find_macos_secure_enclave_key(label)? {
            Some(key) => key,
            None if create_if_missing => create_macos_secure_enclave_key(label)?,
            None => {
                return Err(IdentityError::SecureEnclaveKeyNotFound {
                    label: label.to_owned(),
                })
            }
        };
        let public_key_bytes = public_key_bytes_for_sec_key(&key).map_err(|source| {
            IdentityError::SecureEnclavePublicKey {
                label: label.to_owned(),
                source: Box::new(source),
            }
        })?;
        let did = did_for_secp256r1_public_key(&public_key_bytes).map_err(|source| {
            IdentityError::SecureEnclaveDid {
                label: label.to_owned(),
                source: Box::new(source),
            }
        })?;
        Ok(Self {
            label: label.to_string(),
            key,
            did,
            public_key_bytes,
        })
    }
}

#[cfg(target_os = "macos")]
impl RemoteSigner for MacosSecureEnclaveSigner {
    fn sign_sync(
        &self,
        data: &[u8],
        _authorization: Option<&defra_core::signing::SigningAuthorization>,
    ) -> std::result::Result<Vec<u8>, String> {
        self.key
            .create_signature(
                security_framework::key::Algorithm::ECDSASignatureMessageX962SHA256,
                data,
            )
            .map_err(|error| {
                format!(
                    "Secure Enclave signing failed for label {}: {error}",
                    self.label
                )
            })
    }
}

#[cfg(target_os = "macos")]
fn find_macos_secure_enclave_key(label: &str) -> Result<Option<security_framework::key::SecKey>> {
    use security_framework::item::{ItemSearchOptions, KeyClass, Reference, SearchResult};

    let mut search = ItemSearchOptions::new();
    search
        .key_class(KeyClass::private())
        .label(label)
        .ignore_legacy_keychains()
        .load_refs(true)
        .limit(2);
    let keys = match search.search() {
        Ok(keys) => keys,
        Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => {
            return Ok(None);
        }
        Err(error) if error.code() == ERR_SEC_MISSING_ENTITLEMENT => {
            return Err(missing_secure_enclave_entitlement_error(label));
        }
        Err(source) => {
            return Err(IdentityError::SearchSecureEnclaveKey {
                label: label.to_owned(),
                source,
            });
        }
    };
    let mut matches = keys.into_iter().filter_map(|item| match item {
        SearchResult::Ref(Reference::Key(key)) => Some(key),
        _ => None,
    });
    let first = matches.next();
    if matches.next().is_some() {
        return Err(IdentityError::MultipleSecureEnclaveKeys {
            label: label.to_owned(),
        });
    }
    Ok(first)
}

#[cfg(target_os = "macos")]
fn create_macos_secure_enclave_key(label: &str) -> Result<security_framework::key::SecKey> {
    use security_framework::access_control::SecAccessControl;
    use security_framework::item::Location;
    use security_framework::key::{GenerateKeyOptions, KeyType, SecKey, Token};
    use security_framework::passwords_options::AccessControlOptions;

    let access_control =
        SecAccessControl::create_with_flags(AccessControlOptions::PRIVATE_KEY_USAGE.bits())
            .map_err(IdentityError::SecureEnclaveAccessControl)?;
    let mut options = GenerateKeyOptions::default();
    options
        .set_key_type(KeyType::ec())
        .set_size_in_bits(256)
        .set_label(label)
        .set_access_control(access_control)
        .set_token(Token::SecureEnclave)
        .set_location(Location::DataProtectionKeychain);
    match SecKey::new(&options) {
        Ok(key) => Ok(key),
        Err(error) if error.code() == ERR_SEC_MISSING_ENTITLEMENT as isize => {
            Err(missing_secure_enclave_entitlement_error(label))
        }
        Err(error) => Err(IdentityError::CreateSecureEnclaveKey {
            label: label.to_owned(),
            error: error.to_string(),
        }),
    }
}

#[cfg(target_os = "macos")]
fn missing_secure_enclave_entitlement_error(label: &str) -> IdentityError {
    IdentityError::SecureEnclaveEntitlement {
        label: label.to_owned(),
    }
}

#[cfg(target_os = "macos")]
fn public_key_bytes_for_sec_key(key: &security_framework::key::SecKey) -> Result<Vec<u8>> {
    let public_key = key
        .public_key()
        .ok_or(IdentityError::SecureEnclaveNoPublicKey)?;
    let data = public_key
        .external_representation()
        .ok_or(IdentityError::SecureEnclaveNotExportable)?;
    Ok(data.to_vec())
}

#[cfg(target_os = "macos")]
fn did_for_secp256r1_public_key(public_key_bytes: &[u8]) -> Result<String> {
    let public_key = crypto::public_key_from_bytes(crypto::KeyType::Secp256r1, public_key_bytes)?;
    Ok(public_key.did()?)
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

fn raw_identity_from_signing_config(config: &SigningConfig) -> Result<RawIdentity> {
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

#[cfg(test)]
mod tests;
