//! External premise: an open directory handle keeps its inode identity
//! across unlink and rename, an `openat` without `O_CREAT` is anchored to that
//! directory and creates nothing, and a shared advisory lock contends with an
//! installer's exclusive one. Lean `PluginStoreHome.acquire` decides from what
//! these handles observe.
use std::fs::File;

use super::{lock_path, plugins_root, PublishedPluginReadError, StoreGuard};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StoreAbsent {
    #[error("plugin store has not been bootstrapped or its home was removed")]
    Missing,
    #[error("plugin store belongs to a replaced home")]
    Replaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Outcome {
    Acquired,
    Missing,
    Replaced,
    Busy,
}

pub(super) fn acquire(
    same_home: bool,
    store_present: bool,
    lock_present: bool,
    contended: bool,
) -> Outcome {
    if !same_home {
        Outcome::Replaced
    } else if !store_present || !lock_present {
        Outcome::Missing
    } else if contended {
        Outcome::Busy
    } else {
        Outcome::Acquired
    }
}

#[derive(Debug, Clone, Copy)]
struct Observation {
    same_home: bool,
    store_present: bool,
    lock_present: bool,
}

fn admit(
    observation: Observation,
    contended: bool,
) -> std::result::Result<(), PublishedPluginReadError> {
    match acquire(
        observation.same_home,
        observation.store_present,
        observation.lock_present,
        contended,
    ) {
        Outcome::Acquired => Ok(()),
        Outcome::Busy => Err(PublishedPluginReadError::Busy),
        Outcome::Replaced => Err(StoreAbsent::Replaced.into()),
        Outcome::Missing => Err(StoreAbsent::Missing.into()),
    }
}

fn absent_or_io(error: std::io::Error) -> PublishedPluginReadError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    ) {
        StoreAbsent::Missing.into()
    } else {
        PublishedPluginReadError::Read(error.into())
    }
}

#[cfg(unix)]
fn child(directory: &File, name: &std::ffi::CStr, is_directory: bool) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if is_directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: `name` is NUL-terminated and `directory` stays open through the
    // call; a non-negative result is a new descriptor this function owns.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: `fd` was just returned by `openat` and is owned by nobody else.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(unix)]
fn open_store_dir(home_dir: &File, _home: &Path) -> std::io::Result<File> {
    let name = std::ffi::CString::new(gents_store::local::home::PLUGINS_DIR_NAME)
        .map_err(std::io::Error::other)?;
    child(home_dir, &name, true)
}

#[cfg(not(unix))]
fn open_store_dir(_home_dir: &File, home: &Path) -> std::io::Result<File> {
    File::open(plugins_root(home))
}

#[cfg(unix)]
fn open_lock(store_dir: &File, _home: &Path) -> std::io::Result<File> {
    child(store_dir, c".store.lock", false)
}

#[cfg(not(unix))]
fn open_lock(_store_dir: &File, home: &Path) -> std::io::Result<File> {
    File::open(lock_path(home))
}

/// The home, store directory and lock file one reader captured. Holding the
/// open directories keeps their identity from being reused, so a home that is
/// removed or replaced afterwards reads as absent, never as a fresh store.
#[derive(Debug)]
pub struct StoreReadIdentity {
    home: PathBuf,
    home_dir: File,
    store_dir: File,
    lock_identity: same_file::Handle,
}

impl StoreReadIdentity {
    pub fn capture(home: &Path) -> std::result::Result<Self, PublishedPluginReadError> {
        let home_dir = File::open(home).map_err(absent_or_io)?;
        let store_dir = open_store_dir(&home_dir, home).map_err(absent_or_io)?;
        let lock = open_lock(&store_dir, home).map_err(absent_or_io)?;
        let identity = Self {
            home: home.to_owned(),
            home_dir,
            store_dir,
            lock_identity: same_file::Handle::from_file(lock).map_err(absent_or_io)?,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    fn observation(&self) -> std::result::Result<Observation, PublishedPluginReadError> {
        fn current(
            path: &Path,
            pinned: &same_file::Handle,
        ) -> std::result::Result<(bool, bool), PublishedPluginReadError> {
            match same_file::Handle::from_path(path) {
                Ok(current) => Ok((true, current == *pinned)),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    Ok((false, true))
                }
                Err(error) => Err(absent_or_io(error)),
            }
        }
        let home_pin =
            same_file::Handle::from_file(self.home_dir.try_clone().map_err(absent_or_io)?)
                .map_err(absent_or_io)?;
        let store_pin =
            same_file::Handle::from_file(self.store_dir.try_clone().map_err(absent_or_io)?)
                .map_err(absent_or_io)?;
        let (home_present, home_same) = current(&self.home, &home_pin)?;
        let (store_present, store_same) = current(&plugins_root(&self.home), &store_pin)?;
        let (lock_present, lock_same) = current(&lock_path(&self.home), &self.lock_identity)?;
        Ok(Observation {
            same_home: home_same && store_same && lock_same,
            store_present: home_present && store_present,
            lock_present,
        })
    }

    pub fn validate(&self) -> std::result::Result<(), PublishedPluginReadError> {
        admit(self.observation()?, false)
    }

    pub fn try_shared(&self) -> std::result::Result<StoreGuard, PublishedPluginReadError> {
        self.validate()?;
        let file = open_lock(&self.store_dir, &self.home).map_err(absent_or_io)?;
        let opened = same_file::Handle::from_file(file.try_clone().map_err(absent_or_io)?)
            .map_err(absent_or_io)?;
        if opened != self.lock_identity {
            return Err(StoreAbsent::Replaced.into());
        }
        let lock = gents_store::local::file_lock::FileLock::try_shared(file);
        let contended = matches!(&lock, Err(std::fs::TryLockError::WouldBlock));
        admit(self.observation()?, contended)?;
        let lock = lock.map_err(|error| match error {
            std::fs::TryLockError::Error(error) => PublishedPluginReadError::Read(error.into()),
            std::fs::TryLockError::WouldBlock => PublishedPluginReadError::Busy,
        })?;
        Ok(StoreGuard {
            home: self.home.clone(),
            _lock: lock,
        })
    }
}

/// One runtime's read identity of its plugin store. The home is pinned when
/// the runtime starts; the store is pinned by the first read that finds it in
/// that same home, and never refreshed. A home removed or replaced later reads
/// as [`StoreAbsent`] for the rest of the runtime's life, never as the new
/// home at the same path.
#[derive(Debug, Clone)]
pub struct StoreReadPin(std::sync::Arc<std::sync::Mutex<PinState>>);

#[derive(Debug)]
struct PinState {
    home: PathBuf,
    home_identity: Option<std::sync::Arc<same_file::Handle>>,
    store: Option<std::sync::Arc<StoreReadIdentity>>,
}

fn poisoned<T>(_: T) -> PublishedPluginReadError {
    PublishedPluginReadError::Read(anyhow::anyhow!("the plugin store read pin was poisoned"))
}

impl StoreReadPin {
    pub fn capture(home: &Path) -> Self {
        let pin = Self(std::sync::Arc::new(std::sync::Mutex::new(PinState {
            home: home.to_owned(),
            home_identity: same_file::Handle::from_path(home)
                .ok()
                .map(std::sync::Arc::new),
            store: None,
        })));
        let _ = pin.identity();
        pin
    }

    pub fn home(&self) -> Result<PathBuf, PublishedPluginReadError> {
        Ok(self.0.lock().map_err(poisoned)?.home.clone())
    }

    fn identity(&self) -> Result<std::sync::Arc<StoreReadIdentity>, PublishedPluginReadError> {
        let (home, home_identity) = {
            let state = self.0.lock().map_err(poisoned)?;
            if let Some(identity) = &state.store {
                return Ok(identity.clone());
            }
            (state.home.clone(), state.home_identity.clone())
        };
        let current =
            std::sync::Arc::new(same_file::Handle::from_path(&home).map_err(absent_or_io)?);
        if home_identity
            .as_ref()
            .is_some_and(|pinned| **pinned != *current)
        {
            return Err(StoreAbsent::Replaced.into());
        }
        let captured = StoreReadIdentity::capture(&home).and_then(|identity| {
            let captured_home =
                same_file::Handle::from_file(identity.home_dir.try_clone().map_err(absent_or_io)?)
                    .map_err(absent_or_io)?;
            if captured_home != *current {
                return Err(StoreAbsent::Replaced.into());
            }
            Ok(std::sync::Arc::new(identity))
        });
        let mut state = self.0.lock().map_err(poisoned)?;
        if let Some(identity) = &state.store {
            return Ok(identity.clone());
        }
        if state
            .home_identity
            .as_ref()
            .is_some_and(|pinned| **pinned != *current)
        {
            return Err(StoreAbsent::Replaced.into());
        }
        state.home_identity = Some(current);
        let identity = captured?;
        state.store = Some(identity.clone());
        Ok(identity)
    }

    /// Whether a read has found this runtime's store yet.
    pub fn is_captured(&self) -> bool {
        self.0.lock().is_ok_and(|state| state.store.is_some())
    }

    pub fn validate(&self) -> Result<(), PublishedPluginReadError> {
        self.identity()?.validate()
    }

    pub fn try_shared(&self) -> Result<StoreGuard, PublishedPluginReadError> {
        self.identity()?.try_shared()
    }
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
