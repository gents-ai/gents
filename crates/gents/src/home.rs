use std::path::{Path, PathBuf};

mod init_config;
mod lock;
mod retire;

pub use init_config::{
    default_key_path, ensure_home_identity_can_serve, init_config_path, keys_dir, read_init_config,
    write_init_config, StoredInitConfig, SECURE_ENCLAVE_IDENTITY_BACKEND,
};
pub use lock::{
    lock_home_store, lock_store, lock_store_at, reserve_runtime_startup, wait_for_store_release,
    RuntimeStartupReservation, StoreLock, StoreLockHeld,
};
pub use retire::{
    archive_preflight, home_entries, retire_entries, HomeEntries, RetireDisposition, RetireGroup,
};
const INIT_CONFIG_FILE_NAME: &str = "init.json";

#[derive(Debug, thiserror::Error)]
pub enum HomeError {
    #[error("unable to resolve the user's home directory; set GENTS_HOME")]
    HomeDirUnresolved,
    #[error(transparent)]
    LockHeld(#[from] StoreLockHeld),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("resolving data directory {}", path.display())]
    ResolveDataDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("data directory {} cannot be the filesystem root", path.display())]
    DataDirRoot { path: PathBuf },
    #[error("resolving {}", path.display())]
    ResolveParent {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("inspecting data directory {}", path.display())]
    InspectDataDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("opening runtime lock {}", path.display())]
    OpenLock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("inspecting runtime lock {}", path.display())]
    InspectLock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime lock {} is not a regular file", path.display())]
    LockNotRegular { path: PathBuf },
    #[error("runtime startup waiter stopped before completing")]
    StartupWaiterStopped(#[source] tokio::sync::oneshot::error::RecvError),
    #[error("starting runtime startup waiter")]
    StartStartupWaiter(#[source] std::io::Error),
    #[error("resolving runtime home {}", path.display())]
    ResolveRuntimeHome {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reserving runtime startup")]
    ReserveStartup(#[source] std::io::Error),
    #[error("store release waiter stopped before completing")]
    ReleaseWaiterStopped(#[source] tokio::sync::oneshot::error::RecvError),
    #[error("starting store release waiter")]
    StartReleaseWaiter(#[source] std::io::Error),
    #[error("waiting for store lock {}", path.display())]
    WaitForLock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("locking {}", path.display())]
    Lock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("creating home directory {}", path.display())]
    CreateHome {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("encoding local init config JSON")]
    EncodeInitConfig(#[source] serde_json::Error),
    #[error("staging init config {}", path.display())]
    StageInitConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing init config {}", path.display())]
    WriteInitConfig {
        path: PathBuf,
        #[source]
        source: Box<tempfile::PersistError>,
    },
    #[error("reading init config {}", path.display())]
    ReadInitConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decoding init config {}", path.display())]
    DecodeInitConfig {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "home {} uses the {SECURE_ENCLAVE_IDENTITY_BACKEND} identity backend, whose key cannot own the served node's access control; re-initialize the home with a file or macos-keychain identity (`gents init --home {} --dangerously-overwrite --identity-backend file`) to serve it",
        home.display(),
        home.display()
    )]
    SecureEnclaveHome { home: PathBuf },
    #[error("listing home {}", home.display())]
    ListHome {
        home: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("backup {} has no parent directory", backup.display())]
    BackupWithoutParent { backup: PathBuf },
    #[error("inspecting {}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is on a different filesystem than the backup location {}; nothing was changed", entry.display(), parent.display())]
    CrossFilesystem { entry: PathBuf, parent: PathBuf },
    #[error("creating backup {}", path.display())]
    CreateBackup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("creating {}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} has no file name to archive", path.display())]
    NoFileName { path: PathBuf },
    #[error("moving {} to {}", from.display(), to.display())]
    Move {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("archiving was rolled back; nothing was moved")]
    ArchiveRolledBack {
        #[source]
        source: Box<HomeError>,
    },
    #[error("archiving failed and these entries could not be moved back: {entries}")]
    ArchiveUnrestored {
        entries: String,
        #[source]
        source: Box<HomeError>,
    },
    #[error("deleting stopped after deleting [{removed}]")]
    DeleteStopped {
        removed: String,
        #[source]
        source: Box<HomeError>,
    },
    #[error("deleting {} failed after deleting [{removed}]", path.display())]
    DeleteFailed {
        path: PathBuf,
        removed: String,
        #[source]
        source: std::io::Error,
    },
}

impl HomeError {
    pub fn lock_held(&self) -> Option<&StoreLockHeld> {
        match self {
            Self::LockHeld(held) => Some(held),
            _ => None,
        }
    }
}

/// The store lock contention anywhere in an application error's chain,
/// including through the owners that wrap [`HomeError`] transparently.
pub fn store_lock_held<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a StoreLockHeld> {
    gents_protocol::error_chain::find_map_source(error, |cause| {
        cause
            .downcast_ref::<StoreLockHeld>()
            .or_else(|| cause.downcast_ref::<HomeError>()?.lock_held())
            .or_else(|| {
                cause
                    .downcast_ref::<crate::store_key::StoreKeyError>()?
                    .lock_held()
            })
    })
}

type Result<T, E = HomeError> = std::result::Result<T, E>;

pub fn default_home_dir() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("GENTS_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    Ok(dirs::home_dir()
        .ok_or(HomeError::HomeDirUnresolved)?
        .join(".gents"))
}

const DATA_DIR_NAME: &str = "data";
const KEYS_DIR_NAME: &str = "keys";
pub const RUNTIME_STATE_FILE_NAME: &str = "runtime.json";
pub const RUNTIME_STARTUP_LOCK_FILE_NAME: &str = "runtime-startup.lock";
/// Host-local listen addresses for `gents server`; binding precedes opening
/// the store, so they cannot live in a document.
pub const NETWORK_CONFIG_FILE_NAME: &str = "network.json";
pub const P2P_SECRET_KEY_FILE_NAME: &str = "p2p-secret-key";
pub const PACKS_DIR_NAME: &str = "packs";
pub const ALLOWED_DIRS_FILE_NAME: &str = "allowed-dirs.json";
pub const ALLOWED_DIRS_LOCK_FILE_NAME: &str = "allowed-dirs.lock";
pub const WORKSPACE_APPROVALS_FILE_NAME: &str = "workspace-approvals.json";
pub const WORKSPACE_APPROVALS_LOCK_FILE_NAME: &str = "workspace-approvals.lock";
pub const PERMISSION_STAGING_DIR_NAME: &str = ".permission-staging";
pub const PLUGIN_APPROVALS_DIR_NAME: &str = "plugin-approvals";
pub const PLUGIN_APPROVALS_LOCK_FILE_NAME: &str = ".plugin-approvals.lock";
pub const PLUGINS_DIR_NAME: &str = "plugins";
pub const PLUGIN_SERVICES_DIR_NAME: &str = "plugin-services";
pub const REGISTRY_DIR_NAME: &str = "registry";
pub const CODEX_UI_DIR_NAME: &str = "codex-ui";
pub const EVAL_DIR_NAME: &str = "eval";
pub const PACK_INSTALLS_DIR_NAME: &str = "pack-installs";
/// UX plugins dropped in by hand for development (`<id>/plugin.js`); the
/// desktop's dev door. Installed packs keep theirs in the pack store.
pub const UX_PLUGINS_DIR_NAME: &str = "ux-plugins";

pub const RUNTIME_HOME_ENTRIES: &[&str] = &[
    DATA_DIR_NAME,
    INIT_CONFIG_FILE_NAME,
    KEYS_DIR_NAME,
    RUNTIME_STATE_FILE_NAME,
    NETWORK_CONFIG_FILE_NAME,
    P2P_SECRET_KEY_FILE_NAME,
    PACKS_DIR_NAME,
    PLUGINS_DIR_NAME,
    PLUGIN_SERVICES_DIR_NAME,
    ALLOWED_DIRS_FILE_NAME,
    WORKSPACE_APPROVALS_FILE_NAME,
    PERMISSION_STAGING_DIR_NAME,
    PLUGIN_APPROVALS_DIR_NAME,
    REGISTRY_DIR_NAME,
    CODEX_UI_DIR_NAME,
    EVAL_DIR_NAME,
    PACK_INSTALLS_DIR_NAME,
    UX_PLUGINS_DIR_NAME,
];

/// Lock files stay in place when state is retired: replacing a held lock file
/// would let another writer acquire a different lock for the same resource.
/// A custom data directory's store lock lives beside that directory instead.
pub const RUNTIME_HOME_LOCK_FILES: &[&str] = &[
    "data.lock",
    RUNTIME_STARTUP_LOCK_FILE_NAME,
    ALLOWED_DIRS_LOCK_FILE_NAME,
    WORKSPACE_APPROVALS_LOCK_FILE_NAME,
    PLUGIN_APPROVALS_LOCK_FILE_NAME,
];

pub fn default_data_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(DATA_DIR_NAME)
}

#[cfg(test)]
mod tests;
