use std::path::PathBuf;

use defra_core::signing::SigningKeyType;

use super::InsecureKeyPermissions;

/// An OS custody failure does not prove the recorded identity is absent and
/// must never authorize creation of a replacement key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("macOS could not access recorded identity {label} (Keychain error {code}); allow access and try again; the identity is unchanged")]
pub struct IdentityKeyUnavailable {
    pub label: String,
    pub code: i32,
}

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
    #[error("loading node identity for {did}")]
    LoadNodeIdentity {
        did: String,
        #[source]
        source: identity::Error,
    },
    #[error("signing payload as {did}")]
    SignPayloadAs {
        did: String,
        #[source]
        source: crypto::Error,
    },
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
    #[error("reading macOS keychain identity {label}")]
    KeychainReadFailed {
        label: String,
        #[source]
        source: IdentityKeyUnavailable,
    },
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
    #[error("searching keychain for macOS Secure Enclave key label {label}")]
    SearchSecureEnclaveKey {
        label: String,
        #[source]
        source: IdentityKeyUnavailable,
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

/// The recorded identity's custody failure anywhere in `error`'s chain.
pub fn identity_key_unavailable<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a IdentityKeyUnavailable> {
    gents_protocol::error_chain::find_source::<IdentityKeyUnavailable>(error)
}
