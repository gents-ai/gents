//! External premise: an open directory handle keeps its inode identity
//! across unlink and rename, `mkdirat` and `openat(O_CREAT)` create only
//! inside the directory they are anchored to, and an exclusive advisory lock
//! contends with every other holder. Lean `PluginStoreHome.write` decides
//! from what these handles observe.
use std::fs::File;
use std::path::Path;

use super::read::{
    absent_or_io, at_path, handle_of, is_absent, open_lock, open_store_dir, PinnedHome,
};
use super::{
    lock_path, plugins_root, PublishedPluginReadError, StoreAbsent, StoreGuard, StoreReadIdentity,
    StoreReadPin,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Writer {
    Offline,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WriteOutcome {
    Acquired,
    Created,
    Missing,
    Replaced,
    Busy,
}

pub(super) fn write(
    writer: Writer,
    home_present: bool,
    same_home: bool,
    store_pinned: bool,
    store_present: bool,
    contended: bool,
) -> WriteOutcome {
    match writer {
        Writer::Runtime if !home_present => WriteOutcome::Missing,
        Writer::Runtime if !same_home => WriteOutcome::Replaced,
        Writer::Runtime if store_pinned && !store_present => WriteOutcome::Missing,
        _ if contended => WriteOutcome::Busy,
        _ if !store_present => WriteOutcome::Created,
        _ => WriteOutcome::Acquired,
    }
}

pub(super) fn admit_write(outcome: WriteOutcome) -> Result<(), PublishedPluginReadError> {
    match outcome {
        WriteOutcome::Acquired | WriteOutcome::Created => Ok(()),
        WriteOutcome::Busy => Err(PublishedPluginReadError::Busy),
        WriteOutcome::Missing => Err(StoreAbsent::Missing.into()),
        WriteOutcome::Replaced => Err(StoreAbsent::Replaced.into()),
    }
}

fn locked(
    lock: Result<gents_store::local::file_lock::FileLock, std::fs::TryLockError>,
    path: &Path,
) -> Result<gents_store::local::file_lock::FileLock, PublishedPluginReadError> {
    lock.map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => PublishedPluginReadError::Busy,
        std::fs::TryLockError::Error(error) => PublishedPluginReadError::Read(
            anyhow::Error::new(error).context(format!("locking {}", path.display())),
        ),
    })
}

/// The offline writer's lease: it names its home, so it initializes a store
/// there when none exists.
pub(super) fn try_lock_offline(home: &Path) -> Result<StoreGuard, PublishedPluginReadError> {
    let path = lock_path(home);
    let store_present = path
        .try_exists()
        .map_err(|error| PublishedPluginReadError::Read(error.into()))?;
    super::bootstrap_store(home)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            PublishedPluginReadError::Read(
                anyhow::Error::new(error).context(format!("opening {}", path.display())),
            )
        })?;
    let lock = gents_store::local::file_lock::FileLock::try_exclusive(file);
    let contended = matches!(&lock, Err(std::fs::TryLockError::WouldBlock));
    admit_write(write(
        Writer::Offline,
        true,
        true,
        false,
        store_present,
        contended,
    ))?;
    Ok(StoreGuard {
        home: home.to_owned(),
        pinned: None,
        _lock: locked(lock, &path)?,
    })
}

#[cfg(unix)]
fn create_store_dir(home_dir: &File, home: &Path) -> std::io::Result<File> {
    use std::os::fd::AsRawFd;
    let name = std::ffi::CString::new(gents_store::local::home::PLUGINS_DIR_NAME)
        .map_err(std::io::Error::other)?;
    let mode: libc::mode_t = 0o777;
    // SAFETY: `name` is NUL-terminated and `home_dir` stays open through the
    // call, which creates at most one directory inside it.
    if unsafe { libc::mkdirat(home_dir.as_raw_fd(), name.as_ptr(), mode) } < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    open_store_dir(home_dir, home)
}

#[cfg(not(unix))]
fn create_store_dir(home_dir: &File, home: &Path) -> std::io::Result<File> {
    std::fs::create_dir_all(plugins_root(home))?;
    open_store_dir(home_dir, home)
}

#[cfg(unix)]
fn create_lock(store_dir: &File, _home: &Path) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let mode: libc::mode_t = 0o666;
    // SAFETY: the name is NUL-terminated and `store_dir` stays open through
    // the call; a non-negative result is a new descriptor this function owns.
    let fd = unsafe {
        libc::openat(
            store_dir.as_raw_fd(),
            c".store.lock".as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            libc::c_uint::from(mode),
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: `fd` was just returned by `openat` and is owned by nobody else.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(not(unix))]
fn create_lock(_store_dir: &File, home: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(home))
}

#[derive(Debug, Clone, Copy)]
struct Observation {
    home_present: bool,
    same_home: bool,
    store_present: bool,
}

const ABSENT: Observation = Observation {
    home_present: false,
    same_home: false,
    store_present: false,
};

fn present(opened: std::io::Result<File>) -> Result<Option<File>, PublishedPluginReadError> {
    match opened {
        Ok(file) => Ok(Some(file)),
        Err(error) if is_absent(&error) => Ok(None),
        Err(error) => Err(absent_or_io(error)),
    }
}

/// Whether `store_dir` and `lock` are the store a read pinned, when one did.
fn pinned_store_is(
    pinned: Option<&StoreReadIdentity>,
    store_dir: &File,
    lock: &File,
) -> Result<bool, PublishedPluginReadError> {
    let Some(pinned) = pinned else {
        return Ok(true);
    };
    let (store, lock_identity) = pinned.store_handles()?;
    Ok(handle_of(store_dir)? == store && handle_of(lock)? == *lock_identity)
}

impl StoreReadPin {
    fn observe_before_lock(
        &self,
        home: &PinnedHome,
        pinned: Option<&StoreReadIdentity>,
    ) -> Result<Observation, PublishedPluginReadError> {
        let (home_present, same_home) = home.at(self.home())?;
        let store_dir = present(open_store_dir(&home.dir, self.home()))?;
        let lock = match &store_dir {
            Some(store_dir) => present(open_lock(store_dir, self.home()))?,
            None => None,
        };
        let same_store = match (&store_dir, &lock) {
            (Some(store_dir), Some(lock)) => pinned_store_is(pinned, store_dir, lock)?,
            _ => true,
        };
        Ok(Observation {
            home_present,
            same_home: same_home && same_store,
            store_present: lock.is_some(),
        })
    }

    fn observe_after_lock(
        &self,
        home: &PinnedHome,
        pinned: Option<&StoreReadIdentity>,
        store_dir: &File,
        lock: &File,
    ) -> Result<(bool, bool), PublishedPluginReadError> {
        let (home_present, same_home) = home.at(self.home())?;
        let (store_present, same_store) =
            at_path(&plugins_root(self.home()), &handle_of(store_dir)?)?;
        let (lock_present, same_lock) = at_path(&lock_path(self.home()), &handle_of(lock)?)?;
        Ok((
            home_present,
            same_home
                && store_present
                && same_store
                && lock_present
                && same_lock
                && pinned_store_is(pinned, store_dir, lock)?,
        ))
    }

    /// This runtime's exclusive writer lease. It creates the store only
    /// inside the home pinned when the runtime started, anchored to that
    /// home's open directory, and confirms after locking that the path still
    /// names it: a removed or replaced home is [`StoreAbsent`], never `Busy`,
    /// and nothing is created at its path.
    pub fn try_exclusive(&self) -> Result<StoreGuard, PublishedPluginReadError> {
        let pinned = self.pinned_store()?;
        let home = self.bound_home()?;
        let before = match &home {
            Some(home) => self.observe_before_lock(home, pinned.as_deref())?,
            None => ABSENT,
        };
        let decide = |home_present, same_home, contended| {
            admit_write(write(
                Writer::Runtime,
                home_present,
                same_home,
                pinned.is_some(),
                before.store_present,
                contended,
            ))
        };
        decide(before.home_present, before.same_home, false)?;
        let home = home.ok_or(StoreAbsent::Missing)?;
        let store_dir = create_store_dir(&home.dir, self.home()).map_err(absent_or_io)?;
        let file = create_lock(&store_dir, self.home()).map_err(absent_or_io)?;
        let opened = file.try_clone().map_err(absent_or_io)?;
        let lock = gents_store::local::file_lock::FileLock::try_exclusive(file);
        let contended = matches!(&lock, Err(std::fs::TryLockError::WouldBlock));
        let (home_present, same_home) =
            self.observe_after_lock(&home, pinned.as_deref(), &store_dir, &opened)?;
        decide(home_present, same_home, contended)?;
        Ok(StoreGuard {
            home: self.home().to_owned(),
            pinned: Some(home),
            _lock: locked(lock, &lock_path(self.home()))?,
        })
    }
}
