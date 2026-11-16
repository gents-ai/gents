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

/// Fields drop in order: the OS lock is released before the release notice
/// fires, so a notified waiter always finds the lock free.
#[derive(Debug)]
pub struct StoreLock {
    lock: crate::local::file_lock::FileLock,
    path: PathBuf,
    releasing: AtomicBool,
    released: tokio::sync::watch::Sender<()>,
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
        use std::io::Write as _;
        if self.releasing.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        // The holder's own cursor stands after its pid line.
        let mut file = self.lock.file();
        file.write_all(RELEASING)?;
        file.flush()
    }

    /// Resolves once this lock has been released, wherever its last owner
    /// drops it.
    pub fn release_notice(&self) -> StoreRelease {
        StoreRelease(self.released.subscribe())
    }
}

/// A holder's stop, retire or quit is acknowledged only after this resolves
/// (Lean `ClientInstallation.acknowledgement_requires_closed_store`).
#[derive(Debug)]
pub struct StoreRelease(tokio::sync::watch::Receiver<()>);

impl StoreRelease {
    pub async fn released(mut self) {
        // The sender never sends: `changed` returns only when it drops.
        while self.0.changed().await.is_ok() {}
    }
}

pub fn lock_store(home_dir: &Path, data_dir: &Path) -> Result<StoreLock> {
    lock_store_with_wait(home_dir, data_dir, false)
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
    match lock_store(home_dir, data_dir) {
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
    match try_store(home_dir, data_dir)? {
        StoreAttempt::Acquired(lock) => Ok(lock),
        StoreAttempt::Refused(held) => Err(HomeError::LockHeld(held)),
        StoreAttempt::Wait(held) => {
            tracing::info!(
                home = %home_dir.display(),
                holder_pid = held.holder_pid,
                "waiting for the store's previous holder to release it"
            );
            let (receive, worker) = start_store_acquire_waiter(home_dir, data_dir)?;
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
    data_dir: &Path,
) -> Result<(
    tokio::sync::oneshot::Receiver<Result<StoreLock>>,
    std::thread::JoinHandle<()>,
)> {
    let home_dir = home_dir.to_path_buf();
    let data_dir = data_dir.to_path_buf();
    let (send, receive) = tokio::sync::oneshot::channel();
    let worker = std::thread::Builder::new()
        .name("gents-store-acquire-wait".into())
        .spawn(move || {
            let _ = send.send(lock_store_with_wait(&home_dir, &data_dir, true));
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
        lock: crate::local::file_lock::FileLock::adopt(file),
        path,
        releasing: AtomicBool::new(false),
        released: tokio::sync::watch::Sender::new(()),
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
