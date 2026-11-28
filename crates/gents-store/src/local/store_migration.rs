//! The 1.0 store migration contract owner (`Proofs/Store/StoreMigration.lean`):
//! typed store manifest metadata, pre-lock lineage and manifest admission,
//! release change classification, policy transition planning, and crash-safe
//! staged migration execution.

use std::fs::{self, File};
use std::future::Future;
use std::path::{Path, PathBuf};

use gents_protocol::schemas::{BuiltInPolicy, HomePolicyIds};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use super::backend_protection::MARKER_PREFIX;
use super::document_acp::DocumentAcp;

pub const STORE_MANIFEST_FILE: &str = ".gents-store-manifest.json";
pub const STORE_CONTRACT_VERSION: u32 = 1;
pub const STORAGE_BACKEND_REGOLITH: &str = "regolith";
const MIGRATION_JOURNAL_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreMigrationError {
    #[error("inspecting {}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading store manifest {}", path.display())]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing store manifest {}", path.display())]
    InvalidManifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("writing store manifest {}", path.display())]
    WriteManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("syncing migration directory {}", path.display())]
    SyncDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("migration path is a symlink: {}", path.display())]
    SymlinkPath { path: PathBuf },
    #[error("migration change kind {kind:?} cannot run as a staged migration")]
    ChangeNotStaged { kind: ChangeKind },
    #[error("staged migration requires a named data directory")]
    UnnamedDataDirectory,
    #[error("source store {} has no existing Regolith database", path.display())]
    SourceStoreAbsent { path: PathBuf },
    #[error("unrecorded migration artifacts exist beside {}", data.display())]
    UnrecordedArtifacts { data: PathBuf },
    #[error("staged migration has not been started for {}", data.display())]
    NotBegun { data: PathBuf },
    #[error("staged migration journal for {} does not match requested transition", data.display())]
    JournalMismatch { data: PathBuf },
    #[error("unsupported staged migration journal version {version}")]
    UnsupportedJournalVersion { version: u32 },
    #[error("reading staged migration journal {}", path.display())]
    ReadJournal {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("inconsistent staged migration paths for {}", data.display())]
    InconsistentPaths { data: PathBuf },
    #[error("staged migration for {} cannot finish until promoted and accepted", data.display())]
    CannotFinish { data: PathBuf },
    #[error("cannot copy special file {} during staged migration", path.display())]
    SpecialFile { path: PathBuf },
    #[error(transparent)]
    StorageBackend(#[from] super::storage_backend::StorageBackendError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Persist(#[from] tempfile::PersistError),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
}

type Result<T, E = StoreMigrationError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lineage {
    Current,
    Older,
    Newer,
    Unmarked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestState {
    Absent,
    Matching,
    OlderContract,
    NewerContract,
    CorruptOrMismatched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenDisposition {
    Open,
    Migrate,
    RefuseOlder,
    RefuseNewer,
    RefuseCorrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    NewCollection,
    NewOrdinaryIndex,
    AdditiveNullableField,
    BreakingSchemaChange,
    PolicyIdOrRuleChange,
    DocumentOwnershipOrResourceChange,
    IdentityDerivationChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeHandling {
    InPlaceOnBoot,
    StagedMigration,
    Forbidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationSigner {
    Node,
    User,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRule {
    pub handling: ChangeHandling,
    pub signer: MigrationSigner,
    pub bumps_lineage: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationPhase {
    Staging,
    Verified,
    Promoted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationAction {
    StageAndTransform,
    RetireSource,
    PromoteStage,
    RecordPromoted,
    VerifyAndFinish,
    Refuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpProvider {
    Local,
    Vera,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyRegistration {
    None,
    NodeCounter,
    VeraSeed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyTransitionPlan {
    pub policy_registration: PolicyRegistration,
    pub rebind_collections: bool,
    pub re_register_documents: bool,
    pub rebuild_staged_store: bool,
    pub signer: MigrationSigner,
}

/// Built-in policy IDs recorded in a store's manifest so a future migrator
/// knows which policy IDs governed the store before opening DefraDB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestPolicyIds {
    pub household: String,
    pub mailbox: String,
    pub client_journal: String,
    pub tool_file_change: String,
}

impl ManifestPolicyIds {
    pub fn from_home_policy_ids(ids: &HomePolicyIds) -> Self {
        Self {
            household: ids.id(BuiltInPolicy::Household).to_owned(),
            mailbox: ids.id(BuiltInPolicy::Mailbox).to_owned(),
            client_journal: ids.id(BuiltInPolicy::ClientJournal).to_owned(),
            tool_file_change: ids.id(BuiltInPolicy::ToolFileChange).to_owned(),
        }
    }
}

/// Typed 1.0 store lineage and version metadata stored at
/// `data/.gents-store-manifest.json` and read before taking the store lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreManifest {
    pub contract_version: u32,
    pub lineage: u64,
    pub storage_backend: String,
    pub document_acp: DocumentAcp,
    pub policy_ids: ManifestPolicyIds,
    pub schema_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrated_from: Option<u64>,
}

impl StoreManifest {
    pub fn for_fresh_store(
        lineage: u64,
        document_acp: DocumentAcp,
        ids: &HomePolicyIds,
        schema_fingerprint: String,
    ) -> Self {
        Self {
            contract_version: STORE_CONTRACT_VERSION,
            lineage,
            storage_backend: STORAGE_BACKEND_REGOLITH.to_owned(),
            document_acp,
            policy_ids: ManifestPolicyIds::from_home_policy_ids(ids),
            schema_fingerprint,
            migrated_from: None,
        }
    }

    pub fn for_migrated_store(
        from_lineage: u64,
        to_lineage: u64,
        document_acp: DocumentAcp,
        policy_ids: ManifestPolicyIds,
        schema_fingerprint: String,
    ) -> Self {
        Self {
            contract_version: STORE_CONTRACT_VERSION,
            lineage: to_lineage,
            storage_backend: STORAGE_BACKEND_REGOLITH.to_owned(),
            document_acp,
            policy_ids,
            schema_fingerprint,
            migrated_from: Some(from_lineage),
        }
    }
}

/// Lean `StoreMigration.openDisposition`.
pub fn open_disposition(
    lineage: Lineage,
    manifest: ManifestState,
    migratable_from: bool,
    before_lock: bool,
) -> OpenDisposition {
    match (lineage, manifest) {
        (Lineage::Newer, _) | (_, ManifestState::NewerContract) => OpenDisposition::RefuseNewer,
        (_, ManifestState::CorruptOrMismatched) => OpenDisposition::RefuseCorrupt,
        (Lineage::Current, ManifestState::Matching) => OpenDisposition::Open,
        (Lineage::Current, ManifestState::Absent | ManifestState::OlderContract) => {
            OpenDisposition::RefuseOlder
        }
        (Lineage::Older, ManifestState::OlderContract) => {
            if migratable_from {
                OpenDisposition::Migrate
            } else {
                OpenDisposition::RefuseOlder
            }
        }
        (Lineage::Older, ManifestState::Absent) => OpenDisposition::RefuseOlder,
        (Lineage::Older, ManifestState::Matching) => OpenDisposition::RefuseCorrupt,
        (Lineage::Unmarked, ManifestState::Absent | ManifestState::Matching) => {
            if before_lock {
                OpenDisposition::Open
            } else {
                OpenDisposition::RefuseOlder
            }
        }
        (Lineage::Unmarked, ManifestState::OlderContract) => {
            if before_lock && migratable_from {
                OpenDisposition::Migrate
            } else {
                OpenDisposition::RefuseOlder
            }
        }
    }
}

/// Lean `StoreMigration.changeRule`.
pub fn change_rule(kind: ChangeKind) -> ChangeRule {
    match kind {
        ChangeKind::NewCollection | ChangeKind::NewOrdinaryIndex => ChangeRule {
            handling: ChangeHandling::InPlaceOnBoot,
            signer: MigrationSigner::Node,
            bumps_lineage: false,
        },
        ChangeKind::AdditiveNullableField => ChangeRule {
            handling: ChangeHandling::StagedMigration,
            signer: MigrationSigner::Node,
            bumps_lineage: true,
        },
        ChangeKind::BreakingSchemaChange
        | ChangeKind::PolicyIdOrRuleChange
        | ChangeKind::DocumentOwnershipOrResourceChange => ChangeRule {
            handling: ChangeHandling::StagedMigration,
            signer: MigrationSigner::User,
            bumps_lineage: true,
        },
        ChangeKind::IdentityDerivationChange => ChangeRule {
            handling: ChangeHandling::Forbidden,
            signer: MigrationSigner::None,
            bumps_lineage: false,
        },
    }
}

/// Lean `StoreMigration.migrationStep`.
pub fn migration_step(
    phase: MigrationPhase,
    source: bool,
    stage: bool,
    retired: bool,
) -> MigrationAction {
    match (phase, source, stage, retired) {
        (MigrationPhase::Staging, true, _, false) => MigrationAction::StageAndTransform,
        (MigrationPhase::Verified, true, true, false) => MigrationAction::RetireSource,
        (MigrationPhase::Verified, false, true, true) => MigrationAction::PromoteStage,
        (MigrationPhase::Verified, true, false, true) => MigrationAction::RecordPromoted,
        (MigrationPhase::Promoted, true, false, _) => MigrationAction::VerifyAndFinish,
        _ => MigrationAction::Refuse,
    }
}

/// Lean `StoreMigration.mayFinish`.
pub fn may_finish(phase: MigrationPhase, target_accepted: bool) -> bool {
    phase == MigrationPhase::Promoted && target_accepted
}

/// Lean `StoreMigration.policyTransition`.
pub fn policy_transition(
    policy_id_changed: bool,
    resource_changed: bool,
    same_policy_id_rules_changed: bool,
    provider: AcpProvider,
) -> PolicyTransitionPlan {
    let policy_changes = policy_id_changed || same_policy_id_rules_changed;
    let binding_changes = policy_changes || resource_changed;
    if binding_changes {
        let policy_registration = if policy_changes {
            match provider {
                AcpProvider::Local => PolicyRegistration::NodeCounter,
                AcpProvider::Vera => PolicyRegistration::VeraSeed,
            }
        } else {
            PolicyRegistration::None
        };
        PolicyTransitionPlan {
            policy_registration,
            rebind_collections: true,
            re_register_documents: true,
            rebuild_staged_store: true,
            signer: MigrationSigner::User,
        }
    } else {
        PolicyTransitionPlan {
            policy_registration: PolicyRegistration::None,
            rebind_collections: false,
            re_register_documents: false,
            rebuild_staged_store: false,
            signer: MigrationSigner::None,
        }
    }
}

pub fn manifest_path(data: &Path) -> PathBuf {
    data.join(STORE_MANIFEST_FILE)
}

pub fn read_manifest(data: &Path) -> Result<Option<StoreManifest>> {
    let path = manifest_path(data);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(StoreMigrationError::ReadManifest { path, source }),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| StoreMigrationError::InvalidManifest { path, source })
}

pub fn write_manifest(data: &Path, manifest: &StoreManifest) -> Result<()> {
    let path = manifest_path(data);
    let staged = tempfile::NamedTempFile::new_in(data).map_err(|source| {
        StoreMigrationError::WriteManifest {
            path: path.clone(),
            source,
        }
    })?;
    serde_json::to_writer_pretty(staged.as_file(), manifest)?;
    staged
        .as_file()
        .sync_all()
        .map_err(|source| StoreMigrationError::WriteManifest {
            path: path.clone(),
            source,
        })?;
    staged.persist(&path)?;
    sync_directory(data)
}

pub async fn write_manifest_async(
    data: &Path,
    manifest: &StoreManifest,
    durability: storage::backends::DurabilityMode,
) -> Result<()> {
    let path = manifest_path(data);
    let staging = data.join(format!("{STORE_MANIFEST_FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&staging)
        .await
        .map_err(|source| StoreMigrationError::WriteManifest {
            path: staging.clone(),
            source,
        })?;
    file.write_all(&bytes)
        .await
        .map_err(|source| StoreMigrationError::WriteManifest {
            path: staging.clone(),
            source,
        })?;
    file.flush()
        .await
        .map_err(|source| StoreMigrationError::WriteManifest {
            path: staging.clone(),
            source,
        })?;
    if durability == storage::backends::DurabilityMode::Immediate {
        file.sync_all()
            .await
            .map_err(|source| StoreMigrationError::WriteManifest {
                path: staging.clone(),
                source,
            })?;
    }
    drop(file);
    tokio::fs::rename(&staging, &path).await.map_err(|source| {
        StoreMigrationError::WriteManifest {
            path: path.clone(),
            source,
        }
    })?;
    if durability == storage::backends::DurabilityMode::Immediate {
        let dir = data.to_path_buf();
        tokio::task::spawn_blocking(move || sync_directory(&dir)).await??;
    }
    Ok(())
}

/// Classifies the directory's `.gents-private-backend-v<N>` markers and returns
/// the single parsed version if all parsed markers agree on one version.
pub(crate) fn classify_markers(current: u64, names: &[String]) -> (Lineage, Option<u64>) {
    let mut marked = false;
    let mut newer = false;
    let mut parsed_versions = Vec::new();
    for suffix in names
        .iter()
        .filter_map(|name| name.strip_prefix(MARKER_PREFIX))
    {
        match suffix
            .parse::<u64>()
            .ok()
            .filter(|version| version.to_string() == suffix)
        {
            Some(version) if version == current => return (Lineage::Current, Some(current)),
            Some(version) => {
                newer |= version > current;
                parsed_versions.push(version);
            }
            None => newer = true,
        }
        marked = true;
    }
    if newer {
        (Lineage::Newer, None)
    } else if marked {
        let unique = match parsed_versions.as_slice() {
            [only] => Some(*only),
            _ => None,
        };
        (Lineage::Older, unique)
    } else {
        (Lineage::Unmarked, None)
    }
}

/// Peeks at `contract_version` before strict deserialization so a newer
/// contract version with additional fields fails closed as `NewerContract`
/// rather than `CorruptOrMismatched`.
#[derive(Deserialize)]
struct ManifestHeader {
    contract_version: u32,
}

pub fn inspect_manifest(
    data: &Path,
    current_lineage: u64,
    observed_marker_lineage: Option<u64>,
) -> Result<ManifestState> {
    let path = manifest_path(data);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ManifestState::Absent)
        }
        Err(source) => return Err(StoreMigrationError::ReadManifest { path, source }),
    };
    let Ok(header) = serde_json::from_slice::<ManifestHeader>(&bytes) else {
        return Ok(ManifestState::CorruptOrMismatched);
    };
    if header.contract_version > STORE_CONTRACT_VERSION {
        return Ok(ManifestState::NewerContract);
    }
    if header.contract_version == 0 {
        return Ok(ManifestState::CorruptOrMismatched);
    }
    if header.contract_version < STORE_CONTRACT_VERSION {
        return Ok(ManifestState::OlderContract);
    }
    let Ok(manifest) = serde_json::from_slice::<StoreManifest>(&bytes) else {
        return Ok(ManifestState::CorruptOrMismatched);
    };
    if manifest.storage_backend != STORAGE_BACKEND_REGOLITH {
        return Ok(ManifestState::CorruptOrMismatched);
    }
    if manifest.lineage > current_lineage {
        return Ok(ManifestState::NewerContract);
    }
    if let Some(marker_lineage) = observed_marker_lineage {
        if marker_lineage != manifest.lineage {
            return Ok(if manifest.lineage == current_lineage {
                ManifestState::Matching
            } else {
                ManifestState::CorruptOrMismatched
            });
        }
    }
    if manifest.lineage == current_lineage {
        Ok(ManifestState::Matching)
    } else {
        Ok(ManifestState::OlderContract)
    }
}

/// Read-only inspection of a store directory's lineage markers and manifest.
/// Reads only directory entry names and `.gents-store-manifest.json`, never
/// takes a lock, and writes nothing so any refusal leaves the home byte-identical.
pub fn inspect_store_disposition(
    data: &Path,
    current_lineage: u64,
    migratable_from: bool,
    before_lock: bool,
) -> Result<OpenDisposition> {
    let names = fs::read_dir(data)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    let (lineage, observed_marker) = classify_markers(current_lineage, &names);
    let manifest = inspect_manifest(data, current_lineage, observed_marker)?;
    Ok(open_disposition(
        lineage,
        manifest,
        migratable_from,
        before_lock,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedMigrationJournal {
    pub version: u32,
    pub from_lineage: u64,
    pub to_lineage: u64,
    pub change_kind: ChangeKind,
    pub phase: MigrationPhase,
}

fn sibling(data: &Path, suffix: &str) -> PathBuf {
    let mut name = data.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    data.with_file_name(name)
}

pub fn journal_path(data: &Path) -> PathBuf {
    sibling(data, ".migration.json")
}

pub fn staging_path(data: &Path) -> PathBuf {
    sibling(data, ".migration-stage")
}

pub fn retired_path(data: &Path) -> PathBuf {
    sibling(data, ".migration-source")
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| StoreMigrationError::SyncDirectory {
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn exists_no_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(StoreMigrationError::SymlinkPath {
                    path: path.to_path_buf(),
                });
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(StoreMigrationError::Inspect {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn load_journal(data: &Path) -> Result<Option<StagedMigrationJournal>> {
    let path = journal_path(data);
    if !exists_no_symlink(&path)? {
        return Ok(None);
    }
    let journal: StagedMigrationJournal =
        serde_json::from_slice(&fs::read(&path)?).map_err(|source| {
            StoreMigrationError::ReadJournal {
                path: path.clone(),
                source,
            }
        })?;
    if journal.version != MIGRATION_JOURNAL_VERSION {
        return Err(StoreMigrationError::UnsupportedJournalVersion {
            version: journal.version,
        });
    }
    Ok(Some(journal))
}

fn save_journal(data: &Path, journal: &StagedMigrationJournal, initial: bool) -> Result<()> {
    let path = journal_path(data);
    let parent = parent_dir(&path);
    let staged = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(staged.as_file(), journal)?;
    staged.as_file().sync_all()?;
    if initial {
        staged.persist_noclobber(&path)?;
    } else {
        staged.persist(&path)?;
    }
    sync_directory(parent)
}

/// Begins or idempotently re-enters a crash-safe staged migration under the
/// exclusive store lock. Refuses forbidden or non-staged change kinds before
/// writing any journal or touching `data`.
pub fn begin_staged_migration(
    data: &Path,
    from_lineage: u64,
    to_lineage: u64,
    change_kind: ChangeKind,
) -> Result<StagedMigrationJournal> {
    let rule = change_rule(change_kind);
    if rule.handling != ChangeHandling::StagedMigration {
        return Err(StoreMigrationError::ChangeNotStaged { kind: change_kind });
    }
    if let Some(existing) = load_journal(data)? {
        if existing.from_lineage != from_lineage
            || existing.to_lineage != to_lineage
            || existing.change_kind != change_kind
        {
            return Err(StoreMigrationError::JournalMismatch {
                data: data.to_path_buf(),
            });
        }
        return Ok(existing);
    }
    if data.file_name().is_none() {
        return Err(StoreMigrationError::UnnamedDataDirectory);
    }
    if !(exists_no_symlink(data)? && data.join("MANIFEST").is_file()) {
        return Err(StoreMigrationError::SourceStoreAbsent {
            path: data.to_path_buf(),
        });
    }
    if exists_no_symlink(&staging_path(data))? || exists_no_symlink(&retired_path(data))? {
        return Err(StoreMigrationError::UnrecordedArtifacts {
            data: data.to_path_buf(),
        });
    }
    super::storage_backend::reject_legacy_store(data)?;
    let journal = StagedMigrationJournal {
        version: MIGRATION_JOURNAL_VERSION,
        from_lineage,
        to_lineage,
        change_kind,
        phase: MigrationPhase::Staging,
    };
    save_journal(data, &journal, true)?;
    Ok(journal)
}

fn next_action(data: &Path, phase: MigrationPhase) -> Result<MigrationAction> {
    let stage = exists_no_symlink(&staging_path(data))?;
    let retired = exists_no_symlink(&retired_path(data))?;
    let source = exists_no_symlink(data)?;
    let empty_placeholder = phase == MigrationPhase::Verified
        && source
        && stage
        && retired
        && fs::read_dir(data)?.next().is_none();
    Ok(migration_step(
        phase,
        source && !empty_placeholder,
        stage,
        retired,
    ))
}

/// Drives a begun staged migration to `MigrationPhase::Promoted`, resuming
/// cleanly from any interrupted step without mutating the source store before
/// staging and verification succeed.
pub async fn run_staged_migration<F, Fut, E>(
    data: &Path,
    mut stage_and_verify: F,
) -> std::result::Result<(), E>
where
    F: FnMut(PathBuf, PathBuf) -> Fut,
    Fut: Future<Output = std::result::Result<(), E>>,
    E: From<StoreMigrationError>,
{
    let mut journal = load_journal(data)?.ok_or_else(|| StoreMigrationError::NotBegun {
        data: data.to_path_buf(),
    })?;
    loop {
        match next_action(data, journal.phase)? {
            MigrationAction::StageAndTransform => {
                let stage = staging_path(data);
                if exists_no_symlink(&stage)? {
                    fs::remove_dir_all(&stage).map_err(StoreMigrationError::from)?;
                }
                fs::create_dir(&stage).map_err(StoreMigrationError::from)?;
                stage_and_verify(data.to_path_buf(), stage.clone()).await?;
                sync_directory(&stage)?;
                sync_directory(parent_dir(&stage))?;
                journal.phase = MigrationPhase::Verified;
                save_journal(data, &journal, false)?;
            }
            MigrationAction::RetireSource => {
                fs::rename(data, retired_path(data)).map_err(StoreMigrationError::from)?;
                sync_directory(parent_dir(data))?;
            }
            MigrationAction::PromoteStage => {
                if exists_no_symlink(data)? {
                    fs::remove_dir(data).map_err(StoreMigrationError::from)?;
                }
                fs::rename(staging_path(data), data).map_err(StoreMigrationError::from)?;
                sync_directory(parent_dir(data))?;
            }
            MigrationAction::RecordPromoted => {
                journal.phase = MigrationPhase::Promoted;
                save_journal(data, &journal, false)?;
            }
            MigrationAction::VerifyAndFinish => return Ok(()),
            MigrationAction::Refuse => {
                return Err(StoreMigrationError::InconsistentPaths {
                    data: data.to_path_buf(),
                }
                .into());
            }
        }
    }
}

/// Removes the retired source store and migration journal only after the
/// migration reached `MigrationPhase::Promoted` and the target release accepted
/// the promoted store (`StoreMigration.mayFinish`).
pub fn finish_staged_migration(data: &Path, target_accepted: bool) -> Result<()> {
    let Some(journal) = load_journal(data)? else {
        return Ok(());
    };
    if !may_finish(journal.phase, target_accepted)
        || next_action(data, journal.phase)? != MigrationAction::VerifyAndFinish
    {
        return Err(StoreMigrationError::CannotFinish {
            data: data.to_path_buf(),
        });
    }
    let retired = retired_path(data);
    if exists_no_symlink(&retired)? {
        fs::remove_dir_all(&retired)?;
        sync_directory(parent_dir(&retired))?;
    }
    fs::remove_file(journal_path(data))?;
    sync_directory(parent_dir(data))
}

/// Copies a closed Regolith store directory into an existing empty staging
/// directory without mutating `source`.
pub fn copy_store_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            fs::create_dir(&target)?;
            copy_store_tree(&entry.path(), &target)?;
            fs::set_permissions(&target, metadata.permissions())?;
            sync_directory(&target)?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), &target)?;
            File::open(&target)?.sync_all()?;
        } else {
            return Err(StoreMigrationError::SpecialFile { path: entry.path() });
        }
    }
    sync_directory(destination)
}

#[cfg(test)]
mod tests;
