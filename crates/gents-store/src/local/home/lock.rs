use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{default_data_dir, HomeError, Result, RUNTIME_STARTUP_LOCK_FILE_NAME};

/// The lock file holds the holder's pid on its first line. A holder that
/// begins to stop appends [`RELEASING`] before it releases anything, and never
/// withdraws it, so an opener that finds the lock held reads whether the
/// holder is in the act of shutting down (Lean `StoreHandoff`). The append is
/// one small write: a concurrent reader sees none of it (the holder still
/// serves at that read) or a prefix of it, and any byte after the pid line
/// counts as the declaration.
const RELEASING: &[u8] = b"releasing\n";

/// The storage engine's own lock on the data directory (regolith's).
const ENGINE_DIRECTORY_LOCK: &str = "LOCK";

/// The store lock covers the storage engine's own lock on the data
/// directory: every opener takes the store lock before it builds its node,
/// so the store lock is released only once that engine's lock is free (Lean
/// `StoreHandoff.mayRelease`). Regolith keeps the directory locked after its
/// close until the engine drops with the node's last handle, so a store lock
/// dropped while that node lives declares its release and is released from a
/// dedicated thread once the engine has let go; the OS lock is released
/// before the release notice fires, so a notified waiter always finds the
/// lock free.
#[derive(Debug)]
pub struct StoreLock {
    held: Option<HeldStore>,
    path: PathBuf,
    data_dir: PathBuf,
    releasing: AtomicBool,
}

#[derive(Debug)]
struct HeldStore {
    lock: crate::local::file_lock::FileLock,
    released: tokio::sync::watch::Sender<()>,
}

impl HeldStore {
    /// The OS lock first, then the notice.
    fn release(self) {
        let Self { lock, released } = self;
        drop(lock);
        drop(released);
    }
}

impl StoreLock {
    /// A renamed or removed lock file no longer excludes a new holder.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Declares in the lock file that this holder has begun to stop, so a
    /// process that opens the store meanwhile waits for its release instead
    /// of being refused. Call it before the shutdown that ends in the release.
    pub fn declare_releasing(&self) -> std::io::Result<()> {
        match self.held.as_ref() {
            Some(held) => self.declare(held),
            None => Ok(()),
        }
    }

    fn declare(&self, held: &HeldStore) -> std::io::Result<()> {
        use std::io::Write as _;
        if self.releasing.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        // The holder's own cursor stands after its pid line.
        let mut file = held.lock.file();
        file.write_all(RELEASING)?;
        file.flush()
    }

    /// Resolves once this lock has been released, wherever its last owner
    /// drops it and however long its node's engine keeps the directory.
    pub fn release_notice(&self) -> StoreRelease {
        StoreRelease(self.held.as_ref().map(|held| held.released.subscribe()))
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let Some(held) = self.held.take() else {
            return;
        };
        let engine = match engine_directory(&self.data_dir) {
            EngineDirectory::Free => return held.release(),
            EngineDirectory::Held(engine) => engine,
        };
        if let Err(error) = self.declare(&held) {
            tracing::warn!(%error, "declaring the store release failed; an opener meanwhile is refused");
        }
        let data_dir = self.data_dir.clone();
        let spawned = std::thread::Builder::new()
            .name("gents-store-release".into())
            .spawn(move || {
                // A blocking wait, on this thread only, for the engine's
                // directory lock: the node's last handle drops it.
                if let Err(error) = crate::local::file_lock::FileLock::exclusive(engine) {
                    tracing::warn!(%error, data_dir = %data_dir.display(), "waiting for the storage engine to release the data directory failed; releasing the store");
                }
                held.release();
            });
        if let Err(error) = spawned {
            // The closure, and the store it held, dropped with the error.
            tracing::error!(%error, data_dir = %self.data_dir.display(), "starting the wait for the storage engine failed; the store was released at once");
        }
    }
}

/// Lean `StoreHandoff.Directory`, observed on the engine's lock file without
/// waiting. A missing file is a directory no engine opened.
enum EngineDirectory {
    Free,
    Held(fs::File),
}

#[cfg(unix)]
fn engine_directory(data_dir: &Path) -> EngineDirectory {
    let path = data_dir.join(ENGINE_DIRECTORY_LOCK);
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, path = %path.display(), "the storage engine's directory lock cannot be read; releasing the store");
            }
            return EngineDirectory::Free;
        }
    };
    match file.try_lock() {
        Ok(()) => {
            drop(crate::local::file_lock::FileLock::adopt(file));
            EngineDirectory::Free
        }
        Err(fs::TryLockError::WouldBlock) => EngineDirectory::Held(file),
        Err(fs::TryLockError::Error(error)) => {
            tracing::warn!(%error, path = %path.display(), "the storage engine's directory lock cannot be tried; releasing the store");
            EngineDirectory::Free
        }
    }
}

/// Regolith excludes by share mode on Windows, which a waiter cannot block on.
#[cfg(not(unix))]
fn engine_directory(_data_dir: &Path) -> EngineDirectory {
    EngineDirectory::Free
}

/// Wait until the storage engine releases its directory lock after a temporary
/// node shuts down in this process, so the same lock holder can reopen
/// `data_dir` without racing a draining background handle.
pub(crate) async fn wait_for_engine_release(data_dir: &Path) {
    let engine = match engine_directory(data_dir) {
        EngineDirectory::Free => return,
        EngineDirectory::Held(engine) => engine,
    };
    let (send, receive) = tokio::sync::oneshot::channel();
    let data_dir_owned = data_dir.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("gents-engine-release-wait".into())
        .spawn(move || {
            let result = crate::local::file_lock::FileLock::exclusive(engine).map(drop);
            let _ = send.send(result);
        });
    let worker = match spawned {
        Ok(worker) => worker,
        Err(error) => {
            tracing::warn!(%error, data_dir = %data_dir_owned.display(), "starting the wait for the storage engine failed");
            return;
        }
    };
    drop(worker);
    match tokio::time::timeout(STORE_RELEASE_WAIT, receive).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => {
            tracing::warn!(%error, data_dir = %data_dir_owned.display(), "waiting for the storage engine to release the data directory failed");
        }
        Ok(Err(_)) => {
            tracing::warn!(data_dir = %data_dir_owned.display(), "storage engine release waiter stopped before completing");
        }
        Err(_) => {
            tracing::warn!(data_dir = %data_dir_owned.display(), "timed out waiting for the storage engine to release the data directory");
        }
    }
}

/// A holder's stop, retire or quit is acknowledged only after this resolves
/// (Lean `ClientInstallation.acknowledgement_requires_closed_store`).
#[derive(Debug, Clone)]
pub struct StoreRelease(Option<tokio::sync::watch::Receiver<()>>);

impl StoreRelease {
    pub async fn released(self) {
        let Some(mut receiver) = self.0 else {
            return;
        };
        // The sender never sends: `changed` returns only when it drops.
        while receiver.changed().await.is_ok() {}
    }
}

/// How long an opener waits for a previous holder of the store that declared
/// its release, such as the node an earlier operation in the same process
/// shut down or a runtime its supervisor is stopping (Lean
/// `StoreHandoff.decide`); a serving holder is refused at once.
pub const STORE_RELEASE_WAIT: Duration = Duration::from_secs(60);

/// The store lock file of a data directory, and the directory it covers.
#[derive(Debug, Clone)]
struct StoreTarget {
    lock: PathBuf,
    data_dir: PathBuf,
}

impl StoreTarget {
    /// An existing data directory, resolved through any link.
    fn of(data_dir: &Path) -> Result<Self> {
        let canonical = fs::canonicalize(data_dir).map_err(|source| HomeError::ResolveDataDir {
            path: data_dir.to_path_buf(),
            source,
        })?;
        let (Some(parent), Some(name)) = (canonical.parent(), canonical.file_name()) else {
            return Err(HomeError::DataDirRoot { path: canonical });
        };
        let lock = parent.join(format!("{}.lock", name.to_string_lossy()));
        Ok(Self {
            lock,
            data_dir: canonical,
        })
    }

    /// A data directory that may not exist yet: its lock sits beside where it
    /// would be.
    fn at(data_dir: &Path) -> Result<Self> {
        match fs::symlink_metadata(data_dir) {
            Ok(_) => Self::of(data_dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (data_dir.parent(), data_dir.file_name()) else {
                    return Err(HomeError::DataDirRoot {
                        path: data_dir.to_path_buf(),
                    });
                };
                let lock_dir =
                    fs::canonicalize(parent).map_err(|source| HomeError::ResolveParent {
                        path: parent.to_path_buf(),
                        source,
                    })?;
                Ok(Self {
                    lock: lock_dir.join(format!("{}.lock", name.to_string_lossy())),
                    data_dir: lock_dir.join(name),
                })
            }
            Err(source) => Err(HomeError::InspectDataDir {
                path: data_dir.to_path_buf(),
                source,
            }),
        }
    }
}

pub fn lock_store(home_dir: &Path, data_dir: &Path) -> Result<StoreLock> {
    lock_target(home_dir, StoreTarget::of(data_dir)?, false)
}

/// What a try of the store lock decides (Lean `StoreHandoff.decide`).
#[derive(Debug)]
pub enum StoreAttempt {
    Acquired(StoreLock),
    /// The holder declared that it is releasing.
    Wait(StoreLockHeld),
    Refused(StoreLockHeld),
}

pub fn try_store(home_dir: &Path, data_dir: &Path) -> Result<StoreAttempt> {
    try_target(home_dir, StoreTarget::of(data_dir)?)
}

fn try_target(home_dir: &Path, target: StoreTarget) -> Result<StoreAttempt> {
    match lock_target(home_dir, target, false) {
        Ok(lock) => Ok(StoreAttempt::Acquired(lock)),
        Err(HomeError::LockHeld(held)) if held.releasing => Ok(StoreAttempt::Wait(held)),
        Err(HomeError::LockHeld(held)) => Ok(StoreAttempt::Refused(held)),
        Err(error) => Err(error),
    }
}

/// Opens the store for a process that may start while its previous holder is
/// still shutting down. A serving holder is refused at once with its pid and
/// how to stop it. A holder that declared its release is awaited on the OS
/// lock itself, for at most `release_wait`, the opening operation's own
/// deadline; past it the holder is reported as still releasing. The wait runs
/// on a dedicated OS thread (see [`wait_for_store_release`]): an abandoned
/// wait drops the lock it later acquires.
pub async fn acquire_store(
    home_dir: &Path,
    data_dir: &Path,
    release_wait: Duration,
) -> Result<StoreLock> {
    acquire_target(home_dir, StoreTarget::of(data_dir)?, release_wait).await
}

/// [`acquire_store`] for a data directory that may not exist yet, as
/// [`lock_store_at`] takes it.
pub async fn acquire_store_at(
    home_dir: &Path,
    data_dir: &Path,
    release_wait: Duration,
) -> Result<StoreLock> {
    acquire_target(home_dir, StoreTarget::at(data_dir)?, release_wait).await
}

/// [`acquire_store_at`] on the home's default data directory, as
/// [`lock_home_store`] takes it.
pub async fn acquire_home_store(home_dir: &Path, release_wait: Duration) -> Result<StoreLock> {
    acquire_store_at(home_dir, &default_data_dir(home_dir), release_wait).await
}

async fn acquire_target(
    home_dir: &Path,
    target: StoreTarget,
    release_wait: Duration,
) -> Result<StoreLock> {
    match try_target(home_dir, target.clone())? {
        StoreAttempt::Acquired(lock) => Ok(lock),
        StoreAttempt::Refused(held) => Err(HomeError::LockHeld(held)),
        StoreAttempt::Wait(held) => {
            tracing::info!(
                home = %home_dir.display(),
                holder_pid = held.holder_pid,
                "waiting for the store's previous holder to release it"
            );
            let (receive, worker) = start_store_acquire_waiter(home_dir, target)?;
            drop(worker);
            match tokio::time::timeout(release_wait, receive).await {
                Ok(acquired) => acquired.map_err(HomeError::ReleaseWaiterStopped)?,
                Err(_) => Err(HomeError::LockHeld(held)),
            }
        }
    }
}

fn start_store_acquire_waiter(
    home_dir: &Path,
    target: StoreTarget,
) -> Result<(
    tokio::sync::oneshot::Receiver<Result<StoreLock>>,
    std::thread::JoinHandle<()>,
)> {
    let home_dir = home_dir.to_path_buf();
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = std::thread::Builder::new()
        .name("gents-store-acquire-wait".into())
        .spawn(move || {
            let _ = send.send(lock_target(&home_dir, target, true));
        })
        .map_err(HomeError::StartReleaseWaiter)?;
    Ok((receive, worker))
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

pub(super) fn start_store_release_waiter(
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
            let result = StoreTarget::of(&data_dir)
                .and_then(|target| lock_target(&home_dir, target, true))
                .map(drop);
            let _ = send.send(result);
        })
        .map_err(HomeError::StartReleaseWaiter)?;
    Ok((receive, worker))
}

pub fn lock_home_store(home_dir: &Path) -> Result<StoreLock> {
    lock_store_at(home_dir, &default_data_dir(home_dir))
}

pub fn lock_store_at(home_dir: &Path, data_dir: &Path) -> Result<StoreLock> {
    lock_target(home_dir, StoreTarget::at(data_dir)?, false)
}

#[derive(Debug)]
pub struct StoreLockHeld {
    pub home: PathBuf,
    pub holder_pid: Option<u32>,
    /// The holder declared that it is stopping; reported only once the
    /// opener's own deadline passed while it waited for the release.
    pub releasing: bool,
}

impl std::fmt::Display for StoreLockHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let holder = self
            .holder_pid
            .map(|pid| format!(" (process {pid})"))
            .unwrap_or_default();
        let home = self.home.display();
        if self.releasing {
            return write!(
                f,
                "the Gents runtime{holder} that was using {home} is still shutting down and has not released it. Try again once that process has exited; if it does not exit, stop it: `gents runtime service stop --home {home}` if it runs as the background service"
            );
        }
        write!(
            f,
            "another Gents runtime{holder} is already using {home}. Stop it first: `gents runtime service stop --home {home}` if it runs as the background service, or Ctrl-C in the terminal running `gents runtime server`"
        )
    }
}

impl std::error::Error for StoreLockHeld {}

fn lock_target(home_dir: &Path, target: StoreTarget, wait: bool) -> Result<StoreLock> {
    use std::io::{Read as _, Seek as _, Write as _};
    let StoreTarget {
        lock: path,
        data_dir,
    } = target;

    let mut file = open_runtime_lock(&path)?;
    if wait {
        if let Err(source) = file.lock() {
            return Err(HomeError::WaitForLock { path, source });
        }
    } else {
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                let mut declared = Vec::new();
                let _ = (&mut file).take(64).read_to_end(&mut declared);
                let (pid, releasing) = match declared.iter().position(|byte| *byte == b'\n') {
                    Some(end) => (&declared[..end], end + 1 < declared.len()),
                    None => (&declared[..], false),
                };
                return Err(HomeError::LockHeld(StoreLockHeld {
                    home: home_dir.to_path_buf(),
                    holder_pid: std::str::from_utf8(pid)
                        .ok()
                        .and_then(|pid| pid.trim().parse::<u32>().ok()),
                    releasing,
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
        held: Some(HeldStore {
            lock: crate::local::file_lock::FileLock::adopt(file),
            released: tokio::sync::watch::Sender::new(()),
        }),
        path,
        data_dir,
        releasing: AtomicBool::new(false),
    })
}

pub(super) fn open_runtime_lock(path: &Path) -> Result<fs::File> {
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
    _lock: crate::local::file_lock::FileLock,
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

pub(super) fn start_runtime_startup_waiter(
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
                crate::local::file_lock::FileLock::exclusive(file)
                    .map(|lock| RuntimeStartupReservation { _lock: lock })
                    .map_err(HomeError::ReserveStartup)
            })();
            let _ = send.send(result);
        })
        .map_err(HomeError::StartStartupWaiter)?;
    Ok((receive, worker))
}
