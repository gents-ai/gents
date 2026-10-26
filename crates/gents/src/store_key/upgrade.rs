//! Offline, lossless Regolith value encryption. The caller holds the store's
//! exclusive host lock until metadata publication and `finish` complete, and
//! checks pending intent before opening either a plaintext or encrypted node.
//! Unix also syncs directory entries. Other platforms retain process-restart
//! recovery, but this adapter does not promise rename durability across power loss.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use storage::corekv::{IterOptions, Store};
use storage::RegolithStore;

use super::{StoreEncryption, StoreKey};

#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("syncing store upgrade directory")]
    SyncDirectory(#[source] std::io::Error),
    #[error("store upgrade path is a symlink: {}", path.display())]
    SymlinkPath { path: PathBuf },
    #[error("inspecting {}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading upgrade intent {}", path.display())]
    ReadIntent {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsupported store encryption upgrade journal version")]
    UnsupportedJournalVersion,
    #[error("store upgrade encryption record changed")]
    RecordChanged,
    #[error("store upgrade requires a named data directory")]
    UnnamedDataDirectory,
    #[error("plaintext Regolith store is absent")]
    PlaintextStoreAbsent,
    #[error("unrecorded store upgrade artifacts exist beside {}", data.display())]
    UnrecordedArtifacts { data: PathBuf },
    #[error("store upgrade has no durable intent")]
    NoDurableIntent,
    #[error("inconsistent store upgrade paths for {}", data.display())]
    InconsistentPaths { data: PathBuf },
    #[error("store upgrade must begin before key creation")]
    NotBegun,
    #[error("inconsistent store encryption upgrade paths for {}", data.display())]
    InconsistentEncryptionPaths { data: PathBuf },
    #[error("store upgrade is not installed")]
    NotInstalled,
    #[error("encrypted store verification failed; original store retained")]
    VerificationFailed,
    #[cfg(not(unix))]
    #[error("cannot preserve symlink {} during store upgrade", path.display())]
    SymlinkUnsupported { path: PathBuf },
    #[error("cannot preserve special file {} during store upgrade", path.display())]
    SpecialFile { path: PathBuf },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Persist(#[from] tempfile::PersistError),
    #[error(transparent)]
    StorageBackend(#[from] crate::storage_backend::StorageBackendError),
    #[error(transparent)]
    Storage(#[from] storage::Error),
}

type Result<T, E = UpgradeError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Copying,
    Verified,
    Installed,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    record: StoreEncryption,
    phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Copy,
    RetireSource,
    Promote,
    RecordInstalled,
    Open,
    Refuse,
}

fn next(phase: Phase, source: bool, stage: bool, retired: bool) -> Action {
    match (phase, source, stage, retired) {
        (Phase::Copying, true, _, false) => Action::Copy,
        (Phase::Verified, true, true, false) => Action::RetireSource,
        (Phase::Verified, false, true, true) => Action::Promote,
        (Phase::Verified, true, false, true) => Action::RecordInstalled,
        (Phase::Installed, true, false, _) => Action::Open,
        _ => Action::Refuse,
    }
}

fn after_recheck(equal: bool) -> Phase {
    if equal {
        Phase::Verified
    } else {
        Phase::Copying
    }
}

fn may_finish(phase: Phase, metadata_committed: bool, native_accepted: bool) -> bool {
    phase == Phase::Installed && metadata_committed && native_accepted
}

fn sibling(data: &Path, suffix: &str) -> PathBuf {
    let mut name = data.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    data.with_file_name(name)
}

pub fn staging_path(data: &Path) -> PathBuf {
    sibling(data, ".encryption-stage")
}
fn retired_path(data: &Path) -> PathBuf {
    sibling(data, ".encryption-source")
}
pub fn journal_path(data: &Path) -> PathBuf {
    sibling(data, ".encryption-upgrade.json")
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_parent(path: &Path) -> Result<()> {
    sync_directory(parent(path))
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?
        .sync_all()
        .map_err(UpgradeError::SyncDirectory)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(UpgradeError::SymlinkPath {
                    path: path.to_path_buf(),
                });
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(UpgradeError::Inspect {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn load(data: &Path) -> Result<Option<Journal>> {
    let path = journal_path(data);
    if !exists(&path)? {
        return Ok(None);
    }
    let journal: Journal = serde_json::from_slice(&fs::read(&path)?)
        .map_err(|source| UpgradeError::ReadIntent {
            path: path.clone(),
            source,
        })?;
    if journal.version != 1 {
        return Err(UpgradeError::UnsupportedJournalVersion);
    }
    Ok(Some(journal))
}

fn save(data: &Path, journal: &Journal, initial: bool) -> Result<()> {
    let path = journal_path(data);
    let staged = tempfile::NamedTempFile::new_in(parent(&path))?;
    serde_json::to_writer(staged.as_file(), journal)?;
    staged.as_file().sync_all()?;
    if initial {
        staged.persist_noclobber(&path)?;
    } else {
        staged.persist(&path)?;
    }
    sync_parent(&path)
}

/// Read this before allocating a key.
pub fn pending_record(data: &Path) -> Result<Option<StoreEncryption>> {
    Ok(load(data)?.map(|journal| journal.record))
}

/// Unrecorded siblings are never adopted.
pub fn retirement_entries(data: &Path) -> Result<Vec<PathBuf>> {
    if load(data)?.is_none() {
        return Ok(Vec::new());
    }
    let mut entries = vec![journal_path(data)];
    for path in [staging_path(data), retired_path(data)] {
        if exists(&path)? {
            entries.push(path);
        }
    }
    Ok(entries)
}

/// Persist custody before materializing the key. The caller must never open
/// the plaintext store once publication has begun; this journal owns recovery.
pub fn begin(data: &Path, record: &StoreEncryption) -> Result<()> {
    if let Some(journal) = load(data)? {
        if journal.record != *record {
            return Err(UpgradeError::RecordChanged);
        }
        return Ok(());
    }
    if data.file_name().is_none() {
        return Err(UpgradeError::UnnamedDataDirectory);
    }
    if !(exists(data)? && data.join("MANIFEST").is_file()) {
        return Err(UpgradeError::PlaintextStoreAbsent);
    }
    if exists(&staging_path(data))? || exists(&retired_path(data))? {
        return Err(UpgradeError::UnrecordedArtifacts {
            data: data.to_path_buf(),
        });
    }
    crate::storage_backend::reject_legacy_store(data)?;
    save(
        data,
        &Journal {
            version: 1,
            record: record.clone(),
            phase: Phase::Copying,
        },
        true,
    )
}

/// The original plaintext MANIFEST must not prevent first key creation here.
pub fn key_store_path(data: &Path) -> Result<PathBuf> {
    let journal = load(data)?.ok_or(UpgradeError::NoDurableIntent)?;
    Ok(match action(data, journal.phase)? {
        Action::RecordInstalled | Action::Open => data.to_path_buf(),
        Action::Copy | Action::RetireSource | Action::Promote => staging_path(data),
        Action::Refuse => {
            return Err(UpgradeError::InconsistentPaths {
                data: data.to_path_buf(),
            })
        }
    })
}

fn action(data: &Path, phase: Phase) -> Result<Action> {
    let stage = exists(&staging_path(data))?;
    let retired = exists(&retired_path(data))?;
    let source = exists(data)?;
    // Hosts may recreate their data directory before reaching recovery. Only
    // an empty placeholder at the interrupted promotion boundary is absent
    // evidence; a nonempty competing store must never be removed.
    let empty_placeholder = phase == Phase::Verified
        && source
        && stage
        && retired
        && fs::read_dir(data)?.next().is_none();
    Ok(next(phase, source && !empty_placeholder, stage, retired))
}

/// Encrypt every raw KV entry through DefraDB's encryption owner, then verify
/// the entire decrypted destination before publishing it. No node, query
/// executor, schema writer or P2P service starts here.
pub async fn encrypt_existing(data: &Path, record: &StoreEncryption, key: &StoreKey) -> Result<()> {
    encrypt_existing_with_progress(data, record, key, &mut |_| {}).await
}

/// Reports only completed copy/verification batches to the startup progress owner.
pub async fn encrypt_existing_with_progress(
    data: &Path,
    record: &StoreEncryption,
    key: &StoreKey,
    progress: &mut (dyn FnMut(&'static str) + Send),
) -> Result<()> {
    let mut journal = load(data)?.ok_or(UpgradeError::NotBegun)?;
    if journal.record != *record {
        return Err(UpgradeError::RecordChanged);
    }
    loop {
        match action(data, journal.phase)? {
            Action::Copy => {
                copy_and_verify(data, &staging_path(data), key, progress).await?;
                journal.phase = Phase::Verified;
                save(data, &journal, false)?;
            }
            Action::RetireSource => {
                journal.phase =
                    after_recheck(recheck_source(data, &staging_path(data), key, progress).await?);
                if journal.phase == Phase::Copying {
                    save(data, &journal, false)?;
                    continue;
                }
                fs::rename(data, retired_path(data))?;
                sync_parent(data)?;
            }
            Action::Promote => {
                if exists(data)? {
                    fs::remove_dir(data)?;
                }
                fs::rename(staging_path(data), data)?;
                sync_parent(data)?;
            }
            Action::RecordInstalled => {
                journal.phase = Phase::Installed;
                save(data, &journal, false)?;
            }
            Action::Open => return Ok(()),
            Action::Refuse => {
                return Err(UpgradeError::InconsistentEncryptionPaths {
                    data: data.to_path_buf(),
                })
            }
        }
    }
}

/// Raw KV equality proves byte preservation, not valid DefraDB plaintext:
/// an unrecorded store encrypted by another key must retain its original until
/// the native node/schema owners accept the destination's contents. Removing
/// the old representation is deferred until that commit.
pub fn finish(data: &Path) -> Result<()> {
    let Some(journal) = load(data)? else {
        return Ok(());
    };
    if !may_finish(journal.phase, true, true) {
        return Err(UpgradeError::NotInstalled);
    }
    if action(data, journal.phase)? != Action::Open {
        return Err(UpgradeError::NotInstalled);
    }
    let retired = retired_path(data);
    if exists(&retired)? {
        fs::remove_dir_all(&retired)?;
        sync_parent(&retired)?;
    }
    fs::remove_file(journal_path(data))?;
    sync_parent(data)
}

/// Only the offline staging writer uses snapshot isolation: DefraLevel forbids
/// raw writes to assigned log keys. Close it before verification and promotion
/// so every subsequent open restores normal append and validation rules.
async fn copy_and_verify(
    data: &Path,
    stage: &Path,
    key: &StoreKey,
    progress: &mut (dyn FnMut(&'static str) + Send),
) -> Result<()> {
    let source =
        RegolithStore::open_with_options(data, crate::storage_backend::regolith_options())?;
    let result = async {
        if exists(stage)? {
            fs::remove_dir_all(stage)?;
        }
        fs::create_dir(stage)?;
        fs::set_permissions(stage, fs::metadata(data)?.permissions())?;
        let target_options = crate::storage_backend::regolith_options()
            .with_encryption_key(*key.0)
            .with_isolation(regolith::IsolationLevel::SnapshotIsolation);
        let destination = RegolithStore::open_with_options(stage, target_options)?;
        let copied = copy_values(&source, &destination, progress).await;
        let closed = destination.close().await;
        drop(destination);
        copied?;
        closed?;
        let destination = RegolithStore::open_with_options(
            stage,
            crate::storage_backend::regolith_options().with_encryption_key(*key.0),
        )?;
        let verified = verify_values(&source, &destination, progress).await;
        let closed = destination.close().await;
        drop(destination);
        verified?;
        closed?;
        copy_sidecars(data, stage)?;
        sync_directory(stage)?;
        sync_parent(stage)?;
        Ok::<_, UpgradeError>(())
    }
    .await;
    let closed = source.close().await;
    drop(source);
    result?;
    closed?;
    Ok(())
}

async fn copy_values(
    source: &dyn Store,
    destination: &dyn Store,
    progress: &mut (dyn FnMut(&'static str) + Send),
) -> Result<()> {
    const BATCH_BYTES: usize = 1024 * 1024;
    const BATCH_ENTRIES: usize = 1024;

    let read = source.new_txn(true).await?;
    let mut entries = read.iterator(IterOptions::default()).await?;
    let mut write = destination.new_txn(false).await?;
    let mut bytes = 0;
    let mut count = 0;
    while let Some(entry) = entries.next().await? {
        write.set(&entry.key, &entry.value).await?;
        bytes += entry.key.len() + entry.value.len();
        count += 1;
        if bytes >= BATCH_BYTES || count >= BATCH_ENTRIES {
            write.commit().await?;
            progress("store_encryption_copy");
            write = destination.new_txn(false).await?;
            bytes = 0;
            count = 0;
        }
    }
    write.commit().await?;
    if count > 0 {
        progress("store_encryption_copy");
    }
    entries.close().await?;
    drop(entries);
    read.discard();
    Ok(())
}

async fn recheck_source(
    data: &Path,
    stage: &Path,
    key: &StoreKey,
    progress: &mut (dyn FnMut(&'static str) + Send),
) -> Result<bool> {
    let source =
        RegolithStore::open_with_options(data, crate::storage_backend::regolith_options())?;
    let result = async {
        let destination = RegolithStore::open_with_options(
            stage,
            crate::storage_backend::regolith_options().with_encryption_key(*key.0),
        )?;
        let equal = values_equal(&source, &destination, progress).await;
        let closed = destination.close().await;
        drop(destination);
        let equal = equal?;
        closed?;
        if equal {
            for entry in fs::read_dir(stage)? {
                let entry = entry?;
                if engine_entry(&entry.file_name()) {
                    continue;
                }
                if entry.file_type()?.is_dir() {
                    fs::remove_dir_all(entry.path())?;
                } else {
                    fs::remove_file(entry.path())?;
                }
            }
            copy_sidecars(data, stage)?;
            sync_directory(stage)?;
        }
        Ok::<_, UpgradeError>(equal)
    }
    .await;
    let closed = source.close().await;
    drop(source);
    let equal = result?;
    closed?;
    Ok(equal)
}

async fn verify_values(
    source: &dyn Store,
    destination: &dyn Store,
    progress: &mut (dyn FnMut(&'static str) + Send),
) -> Result<()> {
    if !values_equal(source, destination, progress).await? {
        return Err(UpgradeError::VerificationFailed);
    }
    Ok(())
}

async fn values_equal(
    source: &dyn Store,
    destination: &dyn Store,
    progress: &mut (dyn FnMut(&'static str) + Send),
) -> Result<bool> {
    let left = source.new_txn(true).await?;
    let right = destination.new_txn(true).await?;
    let mut a = left.iterator(IterOptions::default()).await?;
    let mut b = right.iterator(IterOptions::default()).await?;
    let mut verified = 0;
    let equal = loop {
        let expected = a.next().await?;
        let actual = b.next().await?;
        if expected != actual {
            break false;
        }
        if expected.is_none() {
            break true;
        }
        verified += 1;
        if verified == 1024 {
            progress("store_encryption_verify");
            verified = 0;
        }
    };
    if verified > 0 {
        progress("store_encryption_verify");
    }
    a.close().await?;
    b.close().await?;
    drop((a, b));
    left.discard();
    right.discard();
    Ok(equal)
}

fn copy_sidecars(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if engine_entry(&entry.file_name()) {
            continue;
        }
        copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn engine_entry(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("MANIFEST" | "MANIFEST.tmp" | "LOCK" | "sst" | "wal")
    )
}

fn copy_entry(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, metadata.permissions())?;
        sync_directory(destination)?;
    } else if metadata.is_file() {
        fs::copy(source, destination)?;
        File::open(destination)?.sync_all()?;
    } else if metadata.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
        #[cfg(not(unix))]
        return Err(UpgradeError::SymlinkUnsupported {
            path: source.to_path_buf(),
        });
    } else {
        return Err(UpgradeError::SpecialFile {
            path: source.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
