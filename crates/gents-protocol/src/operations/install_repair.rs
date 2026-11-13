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
    /// A store an earlier Gents made that this release cannot open. This
    /// release is fresh-init only (migrations begin after 1.0), so such a
    /// home is reported and left as it is. A key others can read, a missing key and a
    /// damaged store are not about a version, and are named as what they are.
    pub fn is_older(self) -> bool {
        matches!(
            self,
            Self::LegacyRocksDb
                | Self::LegacyLark
                | Self::UnknownLineage
                | Self::UnencryptedStore
                | Self::OtherVersionSchema
        )
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
    let home = data_path.parent().unwrap_or(data_path);
    let home_word = shell_word(home);
    let older = |what: &str| {
        format!(
            "{home} was made by an older version of Gents ({what}), which this version cannot open: this release has no conversion from it. Nothing in it was changed and no background service was started for it. You can keep using it with the older Gents. To keep its data, export it first with the older Gents (`gents utils data export --help`). To start fresh with this version, move it aside yourself (`mv {home_word} {home_word}.older`), then run `gents`",
            home = home.display()
        )
    };
    match kind {
        IncompatibleStoreKind::LegacyRocksDb => older("a RocksDB store"),
        IncompatibleStoreKind::LegacyLark => older("a Lark store"),
        IncompatibleStoreKind::UnknownLineage => older("a store without this version's lineage marker"),
        IncompatibleStoreKind::UnencryptedStore => older("a store that is not encrypted at rest"),
        IncompatibleStoreKind::OtherVersionSchema => older("its collections' access policies or schema differ from this version's"),
        IncompatibleStoreKind::UnsupportedOrCorrupt => format!(
            "{path} is not a store this version can read: it may be damaged, or made by a program other than Gents. Nothing in it was changed. To set it aside and start fresh, run `gents --home {home_word}` in a terminal and choose to set it aside"
        ),
        IncompatibleStoreKind::InsecureKey => format!(
            "{path} can be read by other users of this computer, so Gents will not load it. Nothing was changed. If you changed its permissions yourself, restore them with `chmod 600 {}`, then try again",
            shell_word(data_path)
        ),
        IncompatibleStoreKind::ForeignVersion => format!(
            "{path} was written by a newer version of Gents, which this version does not know. Nothing in it was changed. Open it with that newer version of Gents"
        ),
        IncompatibleStoreKind::MissingStoreKey => format!(
            "{path} is encrypted, but the key this home recorded for it no longer exists (its Keychain item or key file was removed), so its data cannot be read. Nothing in it was changed. To set it aside and start fresh, run `gents --home {home_word}` in a terminal and choose to set it aside"
        ),
    }
}

/// A path as one shell word, quoted only when it needs it.
fn shell_word(path: &Path) -> String {
    let text = path.display().to_string();
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-~+@%:,=".contains(c))
    {
        text
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
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
    InsecureKey,
    CorruptStore,
    UnfinishedSetup,
}

impl HomeState {
    /// A home its node can serve: healthy, or an unfinished setup nobody
    /// chose to continue or start over.
    pub fn is_complete(self) -> bool {
        matches!(self, Self::Healthy | Self::UnfinishedSetup)
    }
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
            Self::OlderStore => "it was made by an older version of Gents",
            Self::NewerStore => "its data is from a newer Gents version",
            Self::InsecureKey => "one of its key files can be read by other users",
            Self::CorruptStore => "its data can't be read; it may be damaged",
            Self::UnfinishedSetup => "its setup stopped before it installed every core pack",
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

/// What the user asked for: `Run` is any command against a home; `Repoint` is
/// `gents runtime service repoint`, the one operation that moves the per-user
/// service to another home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOperation {
    Run,
    Repoint,
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
    ServiceServesAnotherHome {
        home: PathBuf,
        serving: PathBuf,
        /// A port no listener held when this was observed, for a node
        /// started by hand next to the service's.
        free_port: Option<u16>,
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
                "{} is in use by another Gents process (a node started by hand, or a setup in progress). Stop it or let it finish, then try again.",
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
            Self::ServiceServesAnotherHome {
                home,
                serving,
                free_port,
            } => {
                let port = match free_port {
                    Some(port) => format!(" --http-port {port}"),
                    None => " --http-port PORT".to_owned(),
                };
                write!(
                    f,
                    "{} has no running node, and the background service serves {}, which this command leaves as it is. Start a node for it next to the service's with `gents runtime server --home {home}{port}`, or move the background service to it with `gents runtime service repoint --home {home}`",
                    home.display(),
                    serving.display(),
                    home = shell_word(home),
                )
            }
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
    ServiceDefinitionMissing,
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
                f.write_str("the background node was serving another node's data on this folder's port")
            }
            Self::ServiceForAnotherFolder { from } => {
                write!(f, "the background node was serving {}", from.display())
            }
            Self::ServiceProgramMissing => {
                f.write_str("the background service pointed at a copy of Gents that is gone")
            }
            Self::ServiceDefinitionMissing => f.write_str(
                "the background service's definition was deleted while its node ran; it was written back so the node starts at login",
            ),
            Self::BrokenHome { why, kept_at } => {
                write!(f, "{why}; it was set aside as {}", kept_at.display())
            }
            Self::MovedToThisVersion { from: Some(from) } => write!(
                f,
                "Gents moved your local node from version {from} to this version"
            ),
            Self::MovedToThisVersion { from: None } => {
                f.write_str("the background service now runs this copy of Gents")
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
    /// The home is used as it is although its setup did not finish; the
    /// caller says what it lacks.
    #[serde(default)]
    pub unfinished_setup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OlderData {
    pub from: Option<semver::Version>,
}

impl Blocker {
    /// The call that follows, when the message does not already name the
    /// exact ones.
    pub fn next_call(&self) -> Option<NextCall> {
        match self {
            Self::StoreRefused { .. }
            | Self::ServiceServesAnotherHome { .. }
            | Self::NewerNode { .. }
            | Self::BackgroundApprovalPending { .. } => None,
            Self::NoServiceManager { .. } => Some(NextCall::VerbHelp {
                resource: "server",
                verb: "run",
            }),
            Self::ServiceUpdateFailed { .. } => Some(NextCall::VerbHelp {
                resource: "service",
                verb: "update-status",
            }),
            _ => Some(NextCall::Verb {
                resource: "node",
                verb: "status",
            }),
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
        operation: RepairOperation,
        /// A terminal the user can be asked on.
        interactive: bool,
        answer: RepairAnswer,
    },
    Start {
        home: PathBuf,
        serving_home: Option<PathBuf>,
        older_data: Option<OlderData>,
    },
    Restore {
        choice: HomeChoice,
    },
}

/// What the user answered at a terminal about a home that cannot be used as
/// it is. Only an answer moves a home aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairAnswer {
    None,
    SetAside,
    ContinueSetup,
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
    OlderStore,
    InsecureKey,
    Ask,
    Unattended,
    UnfinishedSetup,
    Busy,
    Keychain,
    ForeignProcess,
    UnreadableHome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairObservation {
    pub home: HomeState,
    pub version: RepairVersion,
    pub interactive: bool,
    pub answer: RepairAnswer,
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
    pub served_home_present: bool,
    pub operation: RepairOperation,
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
    pub setup_unfinished: bool,
    pub store: StoreVerdict,
}

/// What a read-only look at a home's store and keys found, before anything
/// opens the store for writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreVerdict {
    Fine,
    Older,
    Newer,
    Corrupt,
    InsecureKey,
    MissingKey,
}

impl StoreVerdict {
    pub fn of(kind: IncompatibleStoreKind) -> Self {
        match kind {
            IncompatibleStoreKind::ForeignVersion => Self::Newer,
            IncompatibleStoreKind::InsecureKey => Self::InsecureKey,
            IncompatibleStoreKind::MissingStoreKey => Self::MissingKey,
            IncompatibleStoreKind::UnsupportedOrCorrupt => Self::Corrupt,
            kind if kind.is_older() => Self::Older,
            _ => Self::Corrupt,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RefusalObservation {
    pub fresh: bool,
    pub missing_key: bool,
    pub older_store: bool,
    pub newer_store: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalAction {
    Ignore,
    Ask,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RepairClientScope {
    All,
    Orphaned,
    Store { kind: IncompatibleStoreKind },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairClientInput {
    pub scope: RepairClientScope,
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
pub struct ClientIdentityObservation {
    pub key_present: bool,
    pub settings_present: bool,
    pub pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientIdentityAction {
    Load,
    Create,
    Refuse,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RepairRecoveryObservation {
    pub candidate_exposed: bool,
    pub same_build: bool,
}
