use std::fs;
use std::path::{Path, PathBuf};

use super::{default_data_dir, HomeError, Result, RUNTIME_STARTUP_LOCK_FILE_NAME};

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
                crate::file_lock::FileLock::exclusive(file)
                    .map(|lock| RuntimeStartupReservation { _lock: lock })
                    .map_err(HomeError::ReserveStartup)
            })();
            let _ = send.send(result);
        })
        .map_err(HomeError::StartStartupWaiter)?;
    Ok((receive, worker))
}
