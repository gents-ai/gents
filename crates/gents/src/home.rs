use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(deserialize = "ToolPackage: Deserialize<'de>, ToolCeiling: Deserialize<'de>"))]
pub struct StoredInitConfig<ToolPackage = String, ToolCeiling = String> {
    pub home: String,
    pub node_name: String,
    pub node_did: String,
    pub key_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keychain_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure_enclave_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_package: Option<ToolPackage>,
    pub tool_ceiling: ToolCeiling,
    pub tool_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_encryption: Option<crate::store_key::StoreEncryption>,
}

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

#[derive(Debug)]
pub struct StoreLock {
    _lock: crate::file_lock::FileLock,
    path: PathBuf,
}

impl StoreLock {
    /// A renamed or removed lock file no longer excludes a new holder.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn lock_store(home_dir: &Path, data_dir: &Path) -> Result<StoreLock> {
    lock_store_with_wait(home_dir, data_dir, false)
}

/// Wait for the current store owner to release its OS lock, then release the
/// reservation immediately. A dedicated OS thread keeps the blocking lock
/// call out of Tokio's shared blocking pool, so an abandoned wait cannot keep
/// that runtime pool alive. Cancellation stops awaiting the result but cannot
/// cancel the OS lock call; the thread drops any lock it later acquires.
/// Callers must be standalone startup processes, with at most one waiter per
/// process, so a previous process can finish releasing the store.
pub async fn wait_for_store_release(home_dir: &Path, data_dir: &Path) -> Result<()> {
    let (receive, worker) = start_store_release_waiter(home_dir, data_dir)?;
    drop(worker);
    receive.await.map_err(HomeError::ReleaseWaiterStopped)?
}

fn start_store_release_waiter(
    home_dir: &Path,
    data_dir: &Path,
) -> Result<(
    tokio::sync::oneshot::Receiver<Result<()>>,
    std::thread::JoinHandle<()>,
)> {
    let home_dir = home_dir.to_path_buf();
    let data_dir = data_dir.to_path_buf();
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = std::thread::Builder::new()
        .name("gents-store-release-wait".into())
        .spawn(move || {
            let result = lock_store_with_wait(&home_dir, &data_dir, true).map(drop);
            let _ = send.send(result);
        })
        .map_err(HomeError::StartReleaseWaiter)?;
    Ok((receive, worker))
}

fn lock_store_with_wait(home_dir: &Path, data_dir: &Path, wait: bool) -> Result<StoreLock> {
    let canonical = fs::canonicalize(data_dir).map_err(|source| HomeError::ResolveDataDir {
        path: data_dir.to_path_buf(),
        source,
    })?;
    let (Some(parent), Some(name)) = (canonical.parent(), canonical.file_name()) else {
        return Err(HomeError::DataDirRoot { path: canonical });
    };
    lock_path_with_wait(
        home_dir,
        parent.join(format!("{}.lock", name.to_string_lossy())),
        wait,
    )
}

pub fn lock_home_store(home_dir: &Path) -> Result<StoreLock> {
    lock_store_at(home_dir, &default_data_dir(home_dir))
}

pub fn lock_store_at(home_dir: &Path, data_dir: &Path) -> Result<StoreLock> {
    match fs::symlink_metadata(data_dir) {
        Ok(_) => lock_store(home_dir, data_dir),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let (Some(parent), Some(name)) = (data_dir.parent(), data_dir.file_name()) else {
                return Err(HomeError::DataDirRoot {
                    path: data_dir.to_path_buf(),
                });
            };
            let lock_dir = fs::canonicalize(parent).map_err(|source| HomeError::ResolveParent {
                path: parent.to_path_buf(),
                source,
            })?;
            lock_path(
                home_dir,
                lock_dir.join(format!("{}.lock", name.to_string_lossy())),
            )
        }
        Err(source) => Err(HomeError::InspectDataDir {
            path: data_dir.to_path_buf(),
            source,
        }),
    }
}

#[derive(Debug)]
pub struct StoreLockHeld {
    pub home: PathBuf,
    pub holder_pid: Option<u32>,
}

impl std::fmt::Display for StoreLockHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let holder = self
            .holder_pid
            .map(|pid| format!(" (process {pid})"))
            .unwrap_or_default();
        let home = self.home.display();
        write!(
            f,
            "another Gents runtime{holder} is already using {home}. Stop it first: `gents service stop --home {home}` if it runs as the background service, or Ctrl-C in the terminal running `gents server`"
        )
    }
}

impl std::error::Error for StoreLockHeld {}

fn lock_path(home_dir: &Path, path: PathBuf) -> Result<StoreLock> {
    lock_path_with_wait(home_dir, path, false)
}

fn lock_path_with_wait(home_dir: &Path, path: PathBuf, wait: bool) -> Result<StoreLock> {
    use std::io::{Read as _, Seek as _, Write as _};

    let mut file = open_runtime_lock(&path)?;
    if wait {
        if let Err(source) = file.lock() {
            return Err(HomeError::WaitForLock { path, source });
        }
    } else {
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                let mut holder = String::new();
                let _ = (&mut file).take(32).read_to_string(&mut holder);
                return Err(HomeError::LockHeld(StoreLockHeld {
                    home: home_dir.to_path_buf(),
                    holder_pid: holder.trim().parse::<u32>().ok(),
                }));
            }
            Err(fs::TryLockError::Error(source)) => {
                return Err(HomeError::Lock { path, source });
            }
        }
    }
    file.set_len(0)?;
    file.rewind()?;
    writeln!(file, "{}", std::process::id())?;
    Ok(StoreLock {
        _lock: crate::file_lock::FileLock::adopt(file),
        path,
    })
}

fn open_runtime_lock(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    // Never follow a planted symlink: truncating below would clobber its
    // target. A planted FIFO must not block the open either.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|source| HomeError::OpenLock {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| HomeError::InspectLock {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(HomeError::LockNotRegular {
            path: path.to_path_buf(),
        });
    }
    Ok(file)
}

/// Serializes standalone runtime preparation for one existing home. Keep the
/// reservation until the new server holds its store claim and has published
/// readiness; a failed preparation releases it by dropping the guard. This
/// reservation conveys no runtime identity or operator authority.
#[derive(Debug)]
pub struct RuntimeStartupReservation {
    _lock: crate::file_lock::FileLock,
}

/// A dedicated OS thread keeps an abandoned OS lock wait out of Tokio's
/// blocking pool. Only standalone preparation processes may call this, once
/// per process. Cancellation abandons the receiver; a later acquisition is
/// immediately dropped, and process exit terminates the pending OS wait.
pub async fn reserve_runtime_startup(home_dir: &Path) -> Result<RuntimeStartupReservation> {
    let (receive, worker) = start_runtime_startup_waiter(home_dir)?;
    drop(worker);
    receive.await.map_err(HomeError::StartupWaiterStopped)?
}

fn start_runtime_startup_waiter(
    home_dir: &Path,
) -> Result<(
    tokio::sync::oneshot::Receiver<Result<RuntimeStartupReservation>>,
    std::thread::JoinHandle<()>,
)> {
    let home_dir = home_dir.to_path_buf();
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = std::thread::Builder::new()
        .name("gents-runtime-startup-wait".into())
        .spawn(move || {
            let result = (|| {
                let home = fs::canonicalize(&home_dir).map_err(|source| {
                    HomeError::ResolveRuntimeHome {
                        path: home_dir.clone(),
                        source,
                    }
                })?;
                let file = open_runtime_lock(&home.join(RUNTIME_STARTUP_LOCK_FILE_NAME))?;
                crate::file_lock::FileLock::exclusive(file)
                    .map(|lock| RuntimeStartupReservation { _lock: lock })
                    .map_err(HomeError::ReserveStartup)
            })();
            let _ = send.send(result);
        })
        .map_err(HomeError::StartStartupWaiter)?;
    Ok((receive, worker))
}

pub fn default_key_path(home_dir: &Path, node_name: &str) -> PathBuf {
    keys_dir(home_dir).join(format!("{node_name}.key"))
}

pub fn keys_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(KEYS_DIR_NAME)
}

pub fn init_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(INIT_CONFIG_FILE_NAME)
}

/// Publish and sync the complete custody record before encrypted store writes.
pub fn write_init_config<ToolPackage, ToolCeiling>(
    home_dir: &Path,
    state: &StoredInitConfig<ToolPackage, ToolCeiling>,
) -> Result<()>
where
    ToolPackage: Serialize,
    ToolCeiling: Serialize,
{
    fs::create_dir_all(home_dir).map_err(|source| HomeError::CreateHome {
        path: home_dir.to_path_buf(),
        source,
    })?;
    let path = init_config_path(home_dir);
    let contents = serde_json::to_vec_pretty(state).map_err(HomeError::EncodeInitConfig)?;
    let staged =
        tempfile::NamedTempFile::new_in(home_dir).map_err(|source| HomeError::StageInitConfig {
            path: path.clone(),
            source,
        })?;
    use std::io::Write as _;
    staged.as_file().write_all(&contents)?;
    staged.as_file().sync_all()?;
    staged
        .persist(&path)
        .map_err(|error| HomeError::WriteInitConfig {
            path: path.clone(),
            source: Box::new(error),
        })?;
    #[cfg(unix)]
    fs::File::open(home_dir)?.sync_all()?;
    Ok(())
}

pub fn read_init_config<ToolPackage, ToolCeiling>(
    home_dir: &Path,
) -> Result<Option<StoredInitConfig<ToolPackage, ToolCeiling>>>
where
    ToolPackage: DeserializeOwned,
    ToolCeiling: DeserializeOwned,
{
    let path = init_config_path(home_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|source| HomeError::ReadInitConfig {
        path: path.clone(),
        source,
    })?;
    let state = serde_json::from_slice(&bytes)
        .map_err(|source| HomeError::DecodeInitConfig { path, source })?;
    Ok(Some(state))
}

pub const SECURE_ENCLAVE_IDENTITY_BACKEND: &str = "macos-secure-enclave";

/// Refuse to serve a home whose node cannot own DefraDB node access
/// control: node access control needs the node DID's private key bytes,
/// which a Secure Enclave key never exports, and serving such a home
/// without access control is undecided.
pub fn ensure_home_identity_can_serve(home_dir: &Path) -> Result<()> {
    let Some(config) = read_init_config::<serde_json::Value, serde_json::Value>(home_dir)? else {
        return Ok(());
    };
    if config.identity_backend.as_deref().map(str::trim) == Some(SECURE_ENCLAVE_IDENTITY_BACKEND) {
        return Err(HomeError::SecureEnclaveHome {
            home: home_dir.to_path_buf(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HomeEntries {
    pub owned: Vec<PathBuf>,
    pub retained: Vec<PathBuf>,
}

pub fn home_entries(home_dir: &Path, exclude: &[PathBuf]) -> Result<HomeEntries> {
    let mut entries = HomeEntries::default();
    let listing = match fs::read_dir(home_dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(source) => {
            return Err(HomeError::ListHome {
                home: home_dir.to_path_buf(),
                source,
            })
        }
    };
    let mut candidates = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|source| HomeError::ListHome {
            home: home_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let owned_name = entry
            .file_name()
            .to_str()
            .is_some_and(|name| RUNTIME_HOME_ENTRIES.contains(&name));
        if owned_name {
            candidates.push(path);
        } else {
            entries.retained.push(path);
        }
    }
    let mut references: Vec<PathBuf> = exclude.to_vec();
    for retained in &entries.retained {
        references.extend(child_home_references(retained));
    }
    for candidate in candidates {
        if references
            .iter()
            .any(|reference| reference.starts_with(&candidate))
        {
            entries.retained.push(candidate);
        } else {
            entries.owned.push(candidate);
        }
    }
    entries.owned.sort();
    entries.retained.sort();
    Ok(entries)
}

fn child_home_references(directory: &Path) -> Vec<PathBuf> {
    let Ok(bytes) = fs::read(init_config_path(directory)) else {
        return Vec::new();
    };
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    ["home", "key_path", "tool_root"]
        .iter()
        .filter_map(|field| record.get(field).and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            let path = PathBuf::from(value);
            fs::canonicalize(&path).unwrap_or(path)
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
pub enum RetireDisposition<'a> {
    Archive { backup: &'a Path },
    Delete,
}

#[derive(Debug, Clone, Copy)]
pub struct RetireGroup<'a> {
    pub name: &'a str,
    pub entries: &'a [PathBuf],
}

/// Fails unless every present entry can be renamed into `backup`: renames do
/// not cross filesystems, and discovering that after the owning service was
/// stopped would leave a half-retired home.
pub fn archive_preflight(groups: &[RetireGroup<'_>], backup: &Path) -> Result<()> {
    archive_preflight_with(groups, backup, device_of)
}

#[cfg(unix)]
fn device_of(path: &Path) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
fn device_of(_path: &Path) -> std::io::Result<u64> {
    Ok(0)
}

fn archive_preflight_with(
    groups: &[RetireGroup<'_>],
    backup: &Path,
    device: impl Fn(&Path) -> std::io::Result<u64>,
) -> Result<()> {
    let parent = backup
        .parent()
        .ok_or_else(|| HomeError::BackupWithoutParent {
            backup: backup.to_path_buf(),
        })?;
    let target = device(parent).map_err(|source| HomeError::Inspect {
        path: parent.to_path_buf(),
        source,
    })?;
    for entry in groups.iter().flat_map(|group| group.entries.iter()) {
        match device(entry) {
            Ok(source) if source == target => {}
            Ok(_) => {
                return Err(HomeError::CrossFilesystem {
                    entry: entry.clone(),
                    parent: parent.to_path_buf(),
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(HomeError::Inspect {
                    path: entry.clone(),
                    source,
                })
            }
        }
    }
    Ok(())
}

pub fn retire_entries(
    groups: &[RetireGroup<'_>],
    disposition: RetireDisposition<'_>,
) -> Result<Vec<PathBuf>> {
    retire_entries_with(groups, disposition, |path| {
        fs::symlink_metadata(path).map(|metadata| metadata.is_dir())
    })
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)
    }
    #[cfg(not(unix))]
    {
        fs::DirBuilder::new().create(path)
    }
}

fn retire_entries_with(
    groups: &[RetireGroup<'_>],
    disposition: RetireDisposition<'_>,
    inspect: impl Fn(&Path) -> std::io::Result<bool>,
) -> Result<Vec<PathBuf>> {
    let present = |entry: &Path| -> Result<Option<bool>> {
        match inspect(entry) {
            Ok(is_dir) => Ok(Some(is_dir)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(HomeError::Inspect {
                path: entry.to_path_buf(),
                source,
            }),
        }
    };
    match disposition {
        RetireDisposition::Archive { backup } => {
            private_dir(backup).map_err(|source| HomeError::CreateBackup {
                path: backup.to_path_buf(),
                source,
            })?;
            let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
            let result = (|| -> Result<()> {
                let mut created: Vec<&str> = Vec::new();
                for group in groups {
                    let target_dir = backup.join(group.name);
                    if !created.contains(&group.name) {
                        private_dir(&target_dir).map_err(|source| HomeError::CreateDirectory {
                            path: target_dir.clone(),
                            source,
                        })?;
                        created.push(group.name);
                    }
                    for entry in group.entries {
                        if present(entry)?.is_none() {
                            continue;
                        }
                        let name = entry.file_name().ok_or_else(|| HomeError::NoFileName {
                            path: entry.clone(),
                        })?;
                        let target = target_dir.join(name);
                        fs::rename(entry, &target).map_err(|source| HomeError::Move {
                            from: entry.clone(),
                            to: target.clone(),
                            source,
                        })?;
                        moved.push((entry.clone(), target));
                    }
                }
                Ok(())
            })();
            match result {
                Ok(()) => Ok(moved.into_iter().map(|(source, _)| source).collect()),
                Err(error) => {
                    let mut unrestored = Vec::new();
                    for (source, target) in moved.iter().rev() {
                        if fs::rename(target, source).is_err() {
                            unrestored.push(target.display().to_string());
                        }
                    }
                    if unrestored.is_empty() {
                        for group in groups {
                            let _ = fs::remove_dir(backup.join(group.name));
                        }
                        let _ = fs::remove_dir(backup);
                        Err(HomeError::ArchiveRolledBack {
                            source: Box::new(error),
                        })
                    } else {
                        Err(HomeError::ArchiveUnrestored {
                            entries: unrestored.join(", "),
                            source: Box::new(error),
                        })
                    }
                }
            }
        }
        RetireDisposition::Delete => {
            let mut removed: Vec<PathBuf> = Vec::new();
            let done = |removed: &[PathBuf]| {
                removed
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            for entry in groups.iter().flat_map(|group| group.entries.iter()) {
                let is_dir = match present(entry) {
                    Ok(Some(is_dir)) => is_dir,
                    Ok(None) => continue,
                    Err(error) => {
                        return Err(HomeError::DeleteStopped {
                            removed: done(&removed),
                            source: Box::new(error),
                        })
                    }
                };
                let outcome = if is_dir {
                    fs::remove_dir_all(entry)
                } else {
                    fs::remove_file(entry)
                };
                if let Err(source) = outcome {
                    return Err(HomeError::DeleteFailed {
                        path: entry.clone(),
                        removed: done(&removed),
                        source,
                    });
                }
                removed.push(entry.clone());
            }
            Ok(removed)
        }
    }
}

#[cfg(test)]
mod tests;
