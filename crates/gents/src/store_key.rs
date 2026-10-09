//! The per-store at-rest encryption key: creation, custody and loading.
//!
//! Every persistent Gents store (a gents home's data directory and the
//! desktop client's store) is opened through DefraDB's `EncryptedStore`
//! (AES-256-GCM over values; keys stay plaintext) with the 32-byte key this
//! module owns. New stores start encrypted. An existing plaintext store is
//! copied and verified before replacement; a recorded encrypted store is
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

use anyhow::{bail, Context, Result};
use k256::elliptic_curve::rand_core::{OsRng, RngCore};
use k256::elliptic_curve::zeroize::Zeroizing;
use serde::{Deserialize, Serialize};

use crate::storage_backend::{IncompatibleStore, IncompatibleStoreKind};

pub mod upgrade;

/// The only store encryption record version this build writes and reads.
pub const STORE_ENCRYPTION_VERSION: u32 = 1;

const STORE_KEY_LEN: usize = 32;

/// Where a new store key is kept. The default is the login keychain on
/// macOS and a key file elsewhere.
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

/// A store's recorded encryption: persisted with the store's home (a gents
/// home's `init.json`, the desktop client's `store-encryption.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreEncryption {
    pub version: u32,
    #[serde(flatten)]
    pub custody: StoreKeyCustody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "custody", rename_all = "kebab-case")]
pub enum StoreKeyCustody {
    /// Login-keychain generic password under this label.
    MacosKeychain { keychain_label: String },
    /// Owner-only key file at the store owner's key-file path.
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

    fn from_slice(bytes: &[u8], source: &str) -> Result<Self> {
        let bytes: [u8; STORE_KEY_LEN] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("store key in {source} is not {STORE_KEY_LEN} bytes"))?;
        Ok(Self(Zeroizing::new(bytes)))
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
            Err(error)
                if error
                    .downcast_ref::<IncompatibleStore>()
                    .is_some_and(|store| store.kind == IncompatibleStoreKind::MissingStoreKey) =>
            {
                if data_path.join("MANIFEST").exists() {
                    return Err(error);
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

    /// Loads the recorded key for the store at `data_path`. A definitely
    /// absent key is the typed [`IncompatibleStoreKind::MissingStoreKey`]
    /// refusal; a Keychain that will not answer is [`StoreKeyUnavailable`].
    pub fn load(&self, key_file: &Path, data_path: &Path) -> Result<StoreKey> {
        if self.version != STORE_ENCRYPTION_VERSION {
            return Err(IncompatibleStore {
                kind: IncompatibleStoreKind::ForeignVersion,
                data_path: data_path.to_path_buf(),
            }
            .into());
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

    /// Removes the recorded key, for a store that is being wiped. An already
    /// absent key is not an error.
    pub fn delete_key(&self, key_file: &Path) -> Result<()> {
        match &self.custody {
            StoreKeyCustody::MacosKeychain { keychain_label } => keychain::delete(keychain_label),
            StoreKeyCustody::File => match std::fs::remove_file(key_file) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(error).with_context(|| format!("removing store key {}", key_file.display()))
                }
                _ => Ok(()),
            },
        }
    }
}

/// Refuses a store that holds data but has no recorded key: it was written
/// unencrypted by an earlier release, or by an interrupted initialization.
pub fn reject_unrecorded_store(data_path: &Path) -> Result<()> {
    if data_path.join("MANIFEST").exists() {
        return Err(unencrypted(data_path));
    }
    Ok(())
}

fn require_unoccupied_key_file(key_file: &Path) -> Result<()> {
    match std::fs::symlink_metadata(key_file) {
        Ok(_) => bail!(
            "unrecorded file at {}; refusing to replace it with a store key",
            key_file.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting store key {}", key_file.display()))
        }
    }
    Ok(())
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

/// As [`open_home_store_key`], selecting custody only when the home has never
/// recorded a store key. An interrupted upgrade keeps its recorded choice.
pub async fn open_home_store_key_with_custody(
    home_dir: &Path,
    data_path: &Path,
    choice: StoreKeyCustodyChoice,
) -> Result<StoreKey> {
    let mut config =
        crate::home::read_init_config::<serde_json::Value, serde_json::Value>(home_dir)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "gents home {} is not initialized; run `gents init --home {}` first",
                    home_dir.display(),
                    home_dir.display()
                )
            })?;
    let key_file = home_key_file(home_dir);
    let pending = upgrade::pending_record(data_path)?;
    if let Some(record) = config.store_encryption.as_ref() {
        if let Some(pending) = pending.as_ref() {
            anyhow::ensure!(
                pending == record,
                "store upgrade custody does not match init.json"
            );
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

/// The DefraDB builder for a persistent store at `data_path` encrypted with
/// `key`.
pub fn persistent_builder(data_path: &Path, key: &StoreKey) -> Result<defra_node::NodeBuilder> {
    crate::storage_backend::reject_legacy_store(data_path)?;
    Ok(key.encrypt(
        defra_node::EmbeddedNode::builder()
            .data_path(data_path)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .with_regolith_options(crate::storage_backend::regolith_options()),
    ))
}

fn unencrypted(data_path: &Path) -> anyhow::Error {
    IncompatibleStore {
        kind: IncompatibleStoreKind::UnencryptedStore,
        data_path: data_path.to_path_buf(),
    }
    .into()
}

fn missing_key(data_path: &Path) -> anyhow::Error {
    IncompatibleStore {
        kind: IncompatibleStoreKind::MissingStoreKey,
        data_path: data_path.to_path_buf(),
    }
    .into()
}

fn keychain_failure(code: i32, data_path: &Path) -> anyhow::Error {
    if code == crate::identity::ERR_SEC_ITEM_NOT_FOUND {
        missing_key(data_path)
    } else {
        StoreKeyUnavailable {
            data_path: data_path.to_path_buf(),
            code,
        }
        .into()
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
    #[cfg(not(target_os = "macos"))]
    use anyhow::bail;
    use anyhow::Result;
    use k256::elliptic_curve::zeroize::Zeroizing;

    #[cfg(target_os = "macos")]
    const SERVICE: &str = "com.source-inc.gents.store-key";

    #[cfg(target_os = "macos")]
    pub(super) fn store(label: &str, bytes: &[u8]) -> Result<()> {
        use anyhow::Context as _;
        crate::identity::macos_keychain_set(SERVICE, label, bytes)
            .with_context(|| format!("storing store key {label} in the macOS keychain"))
    }

    #[cfg(target_os = "macos")]
    pub(super) fn load(label: &str) -> Result<std::result::Result<Zeroizing<Vec<u8>>, i32>> {
        crate::identity::macos_keychain_find(SERVICE, label)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn delete(label: &str) -> Result<()> {
        use anyhow::Context as _;
        crate::identity::macos_keychain_delete(SERVICE, label)
            .with_context(|| format!("removing store key {label} from the macOS keychain"))
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn store(_label: &str, _bytes: &[u8]) -> Result<()> {
        bail!("Keychain store keys are only available on macOS; use file custody")
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn load(label: &str) -> Result<std::result::Result<Zeroizing<Vec<u8>>, i32>> {
        bail!("store key {label} is in a macOS Keychain, which is only available on macOS")
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn delete(label: &str) -> Result<()> {
        bail!("store key {label} is in a macOS Keychain, which is only available on macOS")
    }
}

#[cfg(test)]
mod tests;
