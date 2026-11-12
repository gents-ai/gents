use super::NextCall;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncompatibleStoreKind {
    LegacyRocksDb,
    LegacyLark,
    UnsupportedOrCorrupt,
    UnknownLineage,
    ForeignVersion,
    InsecureKey,
    UnencryptedStore,
    MissingStoreKey,
    OtherVersionSchema,
}

impl IncompatibleStoreKind {
    pub fn is_older(self) -> bool {
        !matches!(self, Self::ForeignVersion)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[error("{}", describe(*kind, data_path))]
pub struct IncompatibleStore {
    pub kind: IncompatibleStoreKind,
    pub data_path: PathBuf,
}

fn describe(kind: IncompatibleStoreKind, data_path: &Path) -> String {
    let path = data_path.display();
    match kind {
        IncompatibleStoreKind::LegacyRocksDb => format!(
            "{path} contains a legacy RocksDB Gents store; this release uses Regolith and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first"
        ),
        IncompatibleStoreKind::LegacyLark => format!(
            "{path} contains a legacy Lark Gents store; this release uses Regolith and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first"
        ),
        IncompatibleStoreKind::UnsupportedOrCorrupt => format!(
            "{path} contains an unsupported or corrupt Gents store; this release uses Regolith and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first"
        ),
        IncompatibleStoreKind::UnknownLineage => format!(
            "{path} was created by an older Gents version. This release is a breaking release with no migration from it, so it cannot open that store. Back it up or delete it to start fresh, or use the release that created it to export any data you need first"
        ),
        IncompatibleStoreKind::InsecureKey => format!(
            "This home's keys were created by an older Gents version with unsafe file permissions ({path} is readable by other users). This release does not load a key that may have been exposed. Back it up or delete it to start fresh with a new identity"
        ),
        IncompatibleStoreKind::ForeignVersion => format!(
            "{path} was written by a different Gents version whose schema this release does not know, so it cannot open that store. Use the version that wrote it, or back it up to start fresh"
        ),
        IncompatibleStoreKind::UnencryptedStore => format!(
            "{path} is not encrypted at rest: it was created by an older Gents version (or by an initialization that did not finish). This release opens only encrypted stores and cannot convert one, so re-initialize the home to start fresh (`gents init --dangerously-overwrite`), backing it up first if you need its data"
        ),
        IncompatibleStoreKind::MissingStoreKey => format!(
            "{path} is encrypted, but the key this home recorded for it no longer exists (its Keychain item or key file was removed), so its data cannot be read. Re-initialize the home to start fresh (`gents init --dangerously-overwrite`)"
        ),
        IncompatibleStoreKind::OtherVersionSchema => format!(
            "{path} was written by another Gents version whose storage layout this release cannot change in place, so it cannot open that store. Back it up or delete it to start fresh"
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeState {
    Absent,
    Healthy,
    Interrupted,
    UnreadableSettings,
    MissingSettings,
    MissingStoreKey,
    OlderStore,
    NewerStore,
}

impl fmt::Display for HomeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Absent => "the node folder is absent",
            Self::Healthy => "the node folder is healthy",
            Self::Interrupted => "its setup stopped midway",
            Self::UnreadableSettings => "its settings file (init.json) can't be read",
            Self::MissingSettings => "its settings file (init.json) is missing",
            Self::MissingStoreKey => "the key for its data is missing",
            Self::OlderStore => "its data is from an older Gents version",
            Self::NewerStore => "its data is from a newer Gents version",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortHolder {
    pub pid: u32,
    pub program: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceMove {
    BeforeSetup,
    AfterSetup,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blocker {
    PortInUse {
        port: u16,
        holder: Option<PortHolder>,
    },
    HandStartedNode {
        port: u16,
        home: Option<String>,
    },
    KeychainUnavailable,
    StoreRefused {
        kind: IncompatibleStoreKind,
        message: String,
    },
    CannotSetAside {
        home: PathBuf,
        reason: String,
    },
    KeepsExiting {
        code: Option<i32>,
        last_error: Option<String>,
    },
    NoAnswer {
        seconds: u64,
        last_error: Option<String>,
    },
    NoServiceManager {
        home: PathBuf,
    },
    HomeUnreadable {
        home: PathBuf,
        reason: String,
    },
    HomeBusy {
        home: PathBuf,
    },
    OldNodeStillRunning {
        pid: u32,
    },
    ServiceUpdateFailed {
        reason: String,
    },
    BackgroundApprovalPending {
        detail: Option<String>,
    },
    NewerNode {
        running: String,
        ours: String,
    },
}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortInUse {
                port,
                holder: Some(holder),
            } => write!(
                f,
                "port {port} is in use by {} (pid {}). Quit it, then try again.",
                holder.program, holder.pid
            ),
            Self::PortInUse { port, holder: None } => write!(
                f,
                "port {port} is in use by another app. Quit it, then try again."
            ),
            Self::HandStartedNode { port, home } => {
                write!(f, "port {port} is used by a Gents node started by hand")?;
                if let Some(home) = home {
                    write!(f, " from {home}")?;
                }
                f.write_str(". Stop it, then try again.")
            }
            Self::KeychainUnavailable => f.write_str(
                "the macOS Keychain is locked or Gents isn't allowed to read it. Unlock it, then try again.",
            ),
            Self::StoreRefused { message, .. } => f.write_str(message),
            Self::CannotSetAside { home, reason } => {
                write!(f, "{} couldn't be set aside: {reason}", home.display())
            }
            Self::KeepsExiting { code, last_error } => {
                f.write_str("the background node keeps exiting")?;
                match (last_error, code) {
                    (Some(error), _) => write!(f, ": {error}"),
                    (None, Some(code)) => write!(f, ": exit code {code}"),
                    (None, None) => Ok(()),
                }
            }
            Self::NoAnswer {
                seconds,
                last_error,
            } => {
                write!(f, "the background node did not answer within {seconds} s")?;
                match last_error {
                    Some(error) => write!(f, ": {error}"),
                    None => Ok(()),
                }
            }
            Self::NoServiceManager { home } => {
                f.write_str(&format!("no service manager is available for {}", home.display()))
            }
            Self::HomeUnreadable { home, reason } => write!(
                f,
                "{} can't be read: {reason}. Check its permissions, then try again.",
                home.display()
            ),
            Self::HomeBusy { home } => write!(
                f,
                "{} is being set up right now. Try again when it finishes.",
                home.display()
            ),
            Self::OldNodeStillRunning { pid } => write!(
                f,
                "the old background node (pid {pid}) did not stop. Quit it, then try again."
            ),
            Self::ServiceUpdateFailed { reason } => {
                write!(f, "the background service could not be updated: {reason}")
            }
            Self::BackgroundApprovalPending { .. } => f.write_str("Enable Gents in System Settings → General → Login Items, then try again."),
            Self::NewerNode { running, ours } => write!(
                f,
                "Your local node runs Gents {running}, newer than this Gents ({ours}). Open Gents {running} or newer, then try again."
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeChoice {
    pub home: PathBuf,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Finding {
    BackupSaved {
        kept_at: PathBuf,
    },
    ClientConnectionsSetAside {
        kept_at: PathBuf,
    },
    ClientStoreSetAside {
        kept_at: PathBuf,
    },
    ClientPreferenceSetAside {
        kept_at: PathBuf,
    },
    StaleNode,
    ServiceForAnotherFolder {
        from: PathBuf,
    },
    ServiceProgramMissing,
    BrokenHome {
        why: HomeState,
        kept_at: PathBuf,
    },
    MovedToThisVersion {
        from: Option<semver::Version>,
    },
    OlderDataSetAside {
        from: Option<semver::Version>,
        kept_at: PathBuf,
    },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackupSaved { kept_at } => write!(f, "the node and service were backed up at {}", kept_at.display()),
            Self::ClientConnectionsSetAside { kept_at } => write!(f, "saved connections couldn't be read and were set aside at {}; add your remote nodes again", kept_at.display()),
            Self::ClientStoreSetAside { kept_at } => write!(f, "the desktop's local data couldn't be opened and was set aside at {}; your identity was kept", kept_at.display()),
            Self::ClientPreferenceSetAside { kept_at } => write!(f, "the desktop's previous settings were set aside at {}", kept_at.display()),
            Self::StaleNode => {
                f.write_str("the background node was still serving a deleted or replaced folder")
            }
            Self::ServiceForAnotherFolder { from } => {
                write!(f, "the background node was serving {}", from.display())
            }
            Self::ServiceProgramMissing => {
                f.write_str("the background service pointed at a copy of Gents that is gone")
            }
            Self::BrokenHome { why, kept_at } => {
                write!(f, "{why}; it was set aside as {}", kept_at.display())
            }
            Self::MovedToThisVersion { from: Some(from) } => write!(
                f,
                "Gents moved your local node from version {from} to this version"
            ),
            Self::MovedToThisVersion { from: None } => {
                f.write_str("Gents moved your local node to this version")
            }
            Self::OlderDataSetAside { from, kept_at } => {
                f.write_str("this version couldn't open the data from ")?;
                match from {
                    Some(from) => write!(f, "Gents {from}")?,
                    None => f.write_str("an older Gents version")?,
                }
                write!(f, ", so it was set aside at {}", kept_at.display())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairStep {
    ColdBackup,
    Drain,
    Backup,
    Rollback,
    #[serde(rename = "stop")]
    StopOldNode,
    UpdateService,
    #[serde(rename = "archive_home")]
    SetAsideFolder,
    #[serde(rename = "start")]
    StartNode,
    #[serde(rename = "health")]
    WaitForNode,
}

impl fmt::Display for RepairStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Drain => "Waiting for active work to finish",
            Self::ColdBackup => "Completing the cold backup",
            Self::Backup => "Backing up the node",
            Self::Rollback => "Restoring the prior service",
            Self::StopOldNode => "Stopping the old node",
            Self::UpdateService => "Updating the background service",
            Self::SetAsideFolder => "Setting aside the old folder",
            Self::StartNode => "Starting the node",
            Self::WaitForNode => "Waiting for the node",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeDecision {
    pub home: PathBuf,
    pub why: HomeState,
    pub default_home: PathBuf,
    pub serving_home: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
    Ready,
    Answering(String),
    NeedsSetup,
    Decide(HomeDecision),
    Blocked(Blocker),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepairReceipt {
    pub client: Option<RepairClientReceipt>,
    pub repaired: Vec<Finding>,
    pub outcome: RepairOutcome,
    pub serving_home: Option<PathBuf>,
    pub older_data: Option<OlderData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OlderData {
    pub from: Option<semver::Version>,
}

impl Blocker {
    pub fn next_call(&self) -> NextCall {
        match self {
            Self::NoServiceManager { .. } => NextCall::VerbHelp {
                resource: "server",
                verb: "run",
            },
            Self::StoreRefused { .. } | Self::CannotSetAside { .. } => NextCall::VerbHelp {
                resource: "runtime",
                verb: "reset",
            },
            Self::ServiceUpdateFailed { .. } => NextCall::VerbHelp {
                resource: "service",
                verb: "update-status",
            },
            _ => NextCall::Verb {
                resource: "node",
                verb: "status",
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairInput {
    Client {
        input: RepairClientInput,
    },
    Prepare {
        choice: HomeChoice,
        service_move: ServiceMove,
    },
    Start {
        home: PathBuf,
        serving_home: Option<PathBuf>,
        older_data: Option<OlderData>,
    },
    Restore {
        choice: HomeChoice,
    },
    Refused {
        choice: HomeChoice,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairVersion {
    Unknown,
    Older,
    Same,
    Newer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairPlanBlocker {
    None,
    NewerVersion,
    ExplicitHome,
    Busy,
    Keychain,
    ForeignProcess,
    UnreadableHome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairObservation {
    pub home: HomeState,
    pub version: RepairVersion,
    pub explicit_home: bool,
    pub busy: bool,
    pub keychain_locked: bool,
    pub foreign_process: bool,
    pub readable: bool,
    pub service_present: bool,
    pub service_running: bool,
    pub service_program_present: bool,
    pub service_same_home: bool,
    pub managed_update: bool,
    pub drain_supported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairPlan {
    pub steps: Vec<RepairStep>,
    pub blocker: RepairPlanBlocker,
    pub needs_setup: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HomeObservation {
    pub present: bool,
    pub owned_entries: bool,
    pub bootstrap_consumed: bool,
    pub pending: bool,
    pub settings_present: bool,
    pub settings_readable: bool,
    pub data_present: bool,
    pub keys_present: bool,
    pub missing_key_refusal: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RefusalObservation {
    pub fresh: bool,
    pub missing_key: bool,
    pub explicit_home: bool,
    pub older_store: bool,
    pub newer_store: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalAction {
    Ignore,
    Ask,
    Archive,
    Block,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClientObservation {
    pub reviewed: bool,
    pub configured_matches: bool,
    pub configured_same_did: bool,
    pub reviewed_home_same_did: bool,
    pub reviewed_home_present: bool,
    pub reviewed_init_present: bool,
    pub stale_local_peer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairClientInput {
    pub root: PathBuf,
    pub user_home: PathBuf,
    pub node_home: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepairClientReceipt {
    pub archive: Option<PathBuf>,
    pub local_peers: usize,
    pub findings: Vec<Finding>,
    pub local_node_lost: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RepairRecoveryObservation {
    pub candidate_exposed: bool,
    pub same_build: bool,
}
