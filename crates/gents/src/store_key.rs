//! The per-store at-rest encryption key: creation, custody and loading.
//!
//! Every persistent Gents store (a gents home's data directory and the
//! desktop client's store) uses DefraDB's engine file encryption with the
//! 32-byte key this module owns. New stores start encrypted. An existing
//! plaintext store is copied and verified before replacement; a recorded encrypted store is
//! never reopened without its key.
//!
//! At-rest encryption protects the disk and its backups. The process that
//! opens the store holds the key, so it never protects a query that node
//! answers; credential fields stay behind the query guards for that.
//!
//! Custody: on macOS the key is a generic password in the user's login
//! keychain, created and read the way the `macos-keychain` identity is. That
//! keychain never syncs through iCloud, but it moves with the login keychain
//! (Migration Assistant, restored backups), and its access list is bound to
//! the creating binary's signature, so a rebuilt or re-signed binary is
//! prompted for access. This-device-only accessibility needs the
//! data-protection keychain, which refuses unentitled binaries
//! (`errSecMissingEntitlement`); it waits on a `keychain-access-groups`
//! entitlement and Developer ID signing. Elsewhere, and whenever
//! [`StoreKeyCustodyChoice::File`] is requested, the key is an owner-only
//! file created like a file identity key. Tests request file custody so they
//! never write login-keychain items.

use std::path::{Path, PathBuf};

use k256::elliptic_curve::rand_core::{OsRng, RngCore};
use k256::elliptic_curve::zeroize::Zeroizing;
use serde::{Deserialize, Serialize};

use crate::storage_backend::{IncompatibleStore, IncompatibleStoreKind, StorageBackendError};

#[derive(Debug, thiserror::Error)]
pub enum StoreKeyError {
    #[error("store key in {origin} is not {STORE_KEY_LEN} bytes")]
    WrongLength { origin: String },
    #[error(transparent)]
    Incompatible(#[from] IncompatibleStore),
    #[error(transparent)]
    Unavailable(#[from] StoreKeyUnavailable),
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),
    #[error(transparent)]
    Storage(#[from] StorageBackendError),
    #[error("unrecorded file at {}; refusing to replace it with a store key", path.display())]
    UnrecordedKeyFile { path: PathBuf },
    #[error("inspecting store key {}", path.display())]
    InspectKeyFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("removing store key {}", path.display())]
    RemoveKeyFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("storing store key {label} in the macOS keychain")]
    KeychainStore {
        label: String,
        #[source]
        source: Box<crate::identity::IdentityError>,
    },
    #[error("removing store key {label} from the macOS keychain")]
    KeychainDelete {
        label: String,
        #[source]
        source: Box<crate::identity::IdentityError>,
    },
    #[error("Keychain store keys are only available on macOS; use file custody")]
    KeychainUnsupported,
    #[error("store key {label} is in a macOS Keychain, which is only available on macOS")]
    KeychainUnavailable { label: String },
    #[error("gents home {} is not initialized; run `gents init --home {}` first", home.display(), home.display())]
    HomeNotInitialized { home: PathBuf },
    #[error("store upgrade custody does not match init.json")]
    UpgradeCustodyMismatch,
    #[error(transparent)]
    Home(#[from] crate::home::HomeError),
    #[error(transparent)]
    Upgrade(#[from] upgrade::UpgradeError),
}

impl StoreKeyError {
    pub fn incompatible(&self) -> Option<&IncompatibleStore> {
        match self {
            Self::Incompatible(store) => Some(store),
            Self::Storage(error) => error.incompatible(),
            Self::Upgrade(error) => error.incompatible(),
            _ => None,
        }
    }

    pub fn insecure_key(&self) -> Option<&crate::identity::InsecureKeyPermissions> {
        match self {
            Self::Identity(error) => error.insecure_key(),
            _ => None,
        }
    }

    pub fn unavailable(&self) -> Option<&StoreKeyUnavailable> {
        match self {
            Self::Unavailable(unavailable) => Some(unavailable),
            _ => None,
        }
    }

    pub fn lock_held(&self) -> Option<&crate::home::StoreLockHeld> {
        match self {
            Self::Home(error) => error.lock_held(),
            _ => None,
        }
    }
}

/// The unavailable store key anywhere in an application error's chain,
/// including through [`StoreKeyError`], which wraps it transparently.
pub fn store_key_unavailable<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a StoreKeyUnavailable> {
    gents_protocol::error_chain::find_map_source(error, |cause| {
        cause
            .downcast_ref::<StoreKeyUnavailable>()
            .or_else(|| cause.downcast_ref::<StoreKeyError>()?.unavailable())
    })
}

type Result<T, E = StoreKeyError> = std::result::Result<T, E>;

pub mod upgrade;

pub const STORE_ENCRYPTION_VERSION: u32 = 1;

const STORE_KEY_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKeyCustodyChoice {
    Keychain,
    File,
}

impl Default for StoreKeyCustodyChoice {
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            Self::Keychain
        } else {
            Self::File
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreEncryption {
    pub version: u32,
    #[serde(flatten)]
    pub custody: StoreKeyCustody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "custody", rename_all = "kebab-case")]
pub enum StoreKeyCustody {
    MacosKeychain { keychain_label: String },
    File,
}

/// A store's 32-byte at-rest key. Never printed.
pub struct StoreKey(Zeroizing<[u8; STORE_KEY_LEN]>);

impl std::fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreKey([redacted])")
    }
}

impl StoreKey {
    fn generate() -> Self {
        let mut bytes = Zeroizing::new([0_u8; STORE_KEY_LEN]);
        OsRng.fill_bytes(bytes.as_mut());
        Self(bytes)
    }

    fn from_slice(bytes: &[u8], origin: &str) -> Result<Self> {
        let bytes: [u8; STORE_KEY_LEN] =
            bytes.try_into().map_err(|_| StoreKeyError::WrongLength {
                origin: origin.to_owned(),
            })?;
        Ok(Self(Zeroizing::new(bytes)))
    }

    /// A trusted host supplies keys only over its private startup channel.
    /// The key must remain bound to the store's durable custody record.
    pub fn from_host(bytes: &[u8]) -> Result<Self> {
        Self::from_slice(bytes, "host secret storage")
    }

    /// A trusted host may cache this value in encrypted secret storage. Native
    /// custody remains the durable owner; bytes must never enter logs or a webview.
    pub fn with_secret<T>(&self, operation: impl FnOnce(&[u8]) -> T) -> T {
        operation(self.0.as_ref())
    }

    /// Opens `builder`'s persistent store encrypted with this key.
    pub fn encrypt(&self, builder: defra_node::NodeBuilder) -> defra_node::NodeBuilder {
        builder.with_at_rest_encryption_key(*self.0)
    }
}

/// The store key is recorded but the macOS Keychain refused to hand it over
/// (locked, access denied, or another Keychain error). The store and its key
/// are intact, so this is never a store refusal and never offers a reset.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the encryption key for {} is in the macOS Keychain but could not be read (Keychain error {code}). Unlock the login keychain or allow Gents access, then try again; the store is unchanged", data_path.display())]
pub struct StoreKeyUnavailable {
    pub data_path: PathBuf,
    pub code: i32,
}

impl StoreEncryption {
    /// Allocates custody without creating a secret. The owner must durably
    /// persist this record before calling [`Self::initialize`], while holding
    /// its store lock, so an interrupted initialization keeps the same key.
    pub fn prepare(
        choice: StoreKeyCustodyChoice,
        key_file: &Path,
        data_path: &Path,
    ) -> Result<Self> {
        reject_unrecorded_store(data_path)?;
        require_unoccupied_key_file(key_file)?;
        Ok(Self {
            version: STORE_ENCRYPTION_VERSION,
            custody: match choice {
                StoreKeyCustodyChoice::Keychain => StoreKeyCustody::MacosKeychain {
                    keychain_label: new_keychain_label(),
                },
                StoreKeyCustodyChoice::File => StoreKeyCustody::File,
            },
        })
    }

    /// Materializes a durably recorded key-creation intent, or loads its key.
    /// Only a definitely absent key with no store may be created. Once a
    /// MANIFEST exists, a lost key must never be replaced.
    pub fn initialize(&self, key_file: &Path, data_path: &Path) -> Result<StoreKey> {
        match self.load(key_file, data_path) {
            Ok(key) => return Ok(key),
            Err(StoreKeyError::Incompatible(store))
                if store.kind == IncompatibleStoreKind::MissingStoreKey =>
            {
                if data_path.join("MANIFEST").exists() {
                    return Err(StoreKeyError::Incompatible(store));
                }
            }
            Err(error) => return Err(error),
        }
        let key = StoreKey::generate();
        match &self.custody {
            StoreKeyCustody::MacosKeychain { keychain_label } => {
                keychain::store(keychain_label, key.0.as_ref())?;
            }
            StoreKeyCustody::File => {
                if !crate::identity::publish_private_key_file(key_file, key.0.as_ref())? {
                    return self.load(key_file, data_path);
                }
            }
        }
        Ok(key)
    }

    pub fn load(&self, key_file: &Path, data_path: &Path) -> Result<StoreKey> {
        if self.version != STORE_ENCRYPTION_VERSION {
            return Err(StoreKeyError::Incompatible(IncompatibleStore {
                kind: IncompatibleStoreKind::ForeignVersion,
                data_path: data_path.to_path_buf(),
            }));
        }
        match &self.custody {
            StoreKeyCustody::MacosKeychain { keychain_label } => {
                match keychain::load(keychain_label)? {
                    Ok(bytes) => {
                        StoreKey::from_slice(&bytes, &format!("Keychain item {keychain_label}"))
                    }
                    Err(code) => Err(keychain_failure(code, data_path)),
                }
            }
            StoreKeyCustody::File => match crate::identity::read_private_key_file(key_file)? {
                Some(bytes) => {
                    let bytes = Zeroizing::new(bytes);
                    StoreKey::from_slice(&bytes, &key_file.display().to_string())
                }
                None => Err(missing_key(data_path)),
            },
        }
    }

    pub fn delete_key(&self, key_file: &Path) -> Result<()> {
        match &self.custody {
            StoreKeyCustody::MacosKeychain { keychain_label } => keychain::delete(keychain_label),
            StoreKeyCustody::File => match std::fs::remove_file(key_file) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(StoreKeyError::RemoveKeyFile {
                        path: key_file.to_path_buf(),
                        source: error,
                    })
                }
                _ => Ok(()),
            },
        }
    }
}

pub fn reject_unrecorded_store(data_path: &Path) -> Result<()> {
    if data_path.join("MANIFEST").exists() {
        return Err(unencrypted(data_path));
    }
    Ok(())
}

fn require_unoccupied_key_file(key_file: &Path) -> Result<()> {
    match std::fs::symlink_metadata(key_file) {
        Ok(_) => Err(StoreKeyError::UnrecordedKeyFile {
            path: key_file.to_path_buf(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StoreKeyError::InspectKeyFile {
            path: key_file.to_path_buf(),
            source,
        }),
    }
}

/// A gents home's store key file. Node identity names always end in `.key`,
/// so the encryption key uses a separate filename namespace.
pub fn home_key_file(home_dir: &Path) -> PathBuf {
    crate::home::keys_dir(home_dir).join("store.aes256")
}

/// Opens a home's store key, upgrading an earlier plaintext store without
/// changing its documents or identity. The caller must hold the store lock,
/// then call [`upgrade::finish`] only after its node opens and validates the
/// encrypted store. Until then, the original plaintext store remains intact.
pub async fn open_home_store_key(home_dir: &Path, data_path: &Path) -> Result<StoreKey> {
    open_home_store_key_with_custody(home_dir, data_path, StoreKeyCustodyChoice::default()).await
}

pub async fn open_home_store_key_with_custody(
    home_dir: &Path,
    data_path: &Path,
    choice: StoreKeyCustodyChoice,
) -> Result<StoreKey> {
    let mut config =
        crate::home::read_init_config::<serde_json::Value, serde_json::Value>(home_dir)?
            .ok_or_else(|| StoreKeyError::HomeNotInitialized {
                home: home_dir.to_path_buf(),
            })?;
    let key_file = home_key_file(home_dir);
    let pending = upgrade::pending_record(data_path)?;
    if let Some(record) = config.store_encryption.as_ref() {
        if let Some(pending) = pending.as_ref() {
            if pending != record {
                return Err(StoreKeyError::UpgradeCustodyMismatch);
            }
        }
        let key = record.initialize(&key_file, data_path)?;
        return Ok(key);
    }
    if pending.is_none() && !data_path.join("MANIFEST").exists() {
        let record = StoreEncryption::prepare(choice, &key_file, data_path)?;
        config.store_encryption = Some(record.clone());
        crate::home::write_init_config(home_dir, &config)?;
        return record.initialize(&key_file, data_path);
    }
    let record = match pending {
        Some(record) => record,
        None => {
            let record =
                StoreEncryption::prepare(choice, &key_file, &upgrade::staging_path(data_path))?;
            upgrade::begin(data_path, &record)?;
            record
        }
    };
    let key = record.initialize(&key_file, &upgrade::key_store_path(data_path)?)?;
    upgrade::encrypt_existing(data_path, &record, &key).await?;
    config.store_encryption = Some(record);
    crate::home::write_init_config(home_dir, &config)?;
    Ok(key)
}

pub fn persistent_builder(data_path: &Path, key: &StoreKey) -> Result<defra_node::NodeBuilder> {
    crate::storage_backend::reject_legacy_store(data_path)?;
    Ok(key.encrypt(
        defra_node::EmbeddedNode::builder()
            .data_path(data_path)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .with_regolith_options(crate::storage_backend::regolith_options()),
    ))
}

fn unencrypted(data_path: &Path) -> StoreKeyError {
    StoreKeyError::Incompatible(IncompatibleStore {
        kind: IncompatibleStoreKind::UnencryptedStore,
        data_path: data_path.to_path_buf(),
    })
}

fn missing_key(data_path: &Path) -> StoreKeyError {
    StoreKeyError::Incompatible(IncompatibleStore {
        kind: IncompatibleStoreKind::MissingStoreKey,
        data_path: data_path.to_path_buf(),
    })
}

fn keychain_failure(code: i32, data_path: &Path) -> StoreKeyError {
    if code == crate::identity::ERR_SEC_ITEM_NOT_FOUND {
        missing_key(data_path)
    } else {
        StoreKeyError::Unavailable(StoreKeyUnavailable {
            data_path: data_path.to_path_buf(),
            code,
        })
    }
}

fn new_keychain_label() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let suffix = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("store-{suffix}")
}

/// Store keys share the `macos-keychain` identity's login-keychain helpers
/// under their own service.
mod keychain {
    use super::{Result, StoreKeyError};
    use k256::elliptic_curve::zeroize::Zeroizing;

    #[cfg(target_os = "macos")]
    const SERVICE: &str = "com.source-inc.gents.store-key";

    #[cfg(target_os = "macos")]
    pub(super) fn store(label: &str, bytes: &[u8]) -> Result<()> {
        crate::identity::macos_keychain_set(SERVICE, label, bytes).map_err(|source| {
            StoreKeyError::KeychainStore {
                label: label.to_owned(),
                source: Box::new(source),
            }
        })
    }

    #[cfg(target_os = "macos")]
    pub(super) fn load(label: &str) -> Result<std::result::Result<Zeroizing<Vec<u8>>, i32>> {
        Ok(crate::identity::macos_keychain_find(SERVICE, label)?)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn delete(label: &str) -> Result<()> {
        crate::identity::macos_keychain_delete(SERVICE, label).map_err(|source| {
            StoreKeyError::KeychainDelete {
                label: label.to_owned(),
                source: Box::new(source),
            }
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn store(_label: &str, _bytes: &[u8]) -> Result<()> {
        Err(StoreKeyError::KeychainUnsupported)
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn load(label: &str) -> Result<std::result::Result<Zeroizing<Vec<u8>>, i32>> {
        Err(StoreKeyError::KeychainUnavailable {
            label: label.to_owned(),
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn delete(label: &str) -> Result<()> {
        Err(StoreKeyError::KeychainUnavailable {
            label: label.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests;
