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

/// Lean `PluginStoreHome.Outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOutcome {
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
) -> ReadOutcome {
    if !same_home {
        ReadOutcome::Replaced
    } else if !store_present || !lock_present {
        ReadOutcome::Missing
    } else if contended {
        ReadOutcome::Busy
    } else {
        ReadOutcome::Acquired
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
        ReadOutcome::Acquired => Ok(()),
        ReadOutcome::Busy => Err(PublishedPluginReadError::Busy),
        ReadOutcome::Replaced => Err(StoreAbsent::Replaced.into()),
        ReadOutcome::Missing => Err(StoreAbsent::Missing.into()),
    }
}

pub(super) fn is_absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

pub(super) fn absent_or_io(error: std::io::Error) -> PublishedPluginReadError {
    if is_absent(&error) {
        StoreAbsent::Missing.into()
    } else {
        PublishedPluginReadError::Read(error.into())
    }
}

pub(super) fn handle_of(file: &File) -> Result<same_file::Handle, PublishedPluginReadError> {
    same_file::Handle::from_file(file.try_clone().map_err(absent_or_io)?).map_err(absent_or_io)
}

/// Whether anything is at `path`, and whether it is `pinned`.
pub(super) fn at_path(
    path: &Path,
    pinned: &same_file::Handle,
) -> Result<(bool, bool), PublishedPluginReadError> {
    match same_file::Handle::from_path(path) {
        Ok(current) => Ok((true, current == *pinned)),
        Err(error) if is_absent(&error) => Ok((false, true)),
        Err(error) => Err(absent_or_io(error)),
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
pub(super) fn open_store_dir(home_dir: &File, _home: &Path) -> std::io::Result<File> {
    let name = std::ffi::CString::new(gents_store::local::home::PLUGINS_DIR_NAME)
        .map_err(std::io::Error::other)?;
    child(home_dir, &name, true)
}

#[cfg(not(unix))]
pub(super) fn open_store_dir(_home_dir: &File, home: &Path) -> std::io::Result<File> {
    File::open(plugins_root(home))
}

#[cfg(unix)]
pub(super) fn open_lock(store_dir: &File, _home: &Path) -> std::io::Result<File> {
    child(store_dir, c".store.lock", false)
}

#[cfg(not(unix))]
pub(super) fn open_lock(_store_dir: &File, home: &Path) -> std::io::Result<File> {
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
        let home_pin = handle_of(&self.home_dir)?;
        let store_pin = handle_of(&self.store_dir)?;
        let (home_present, home_same) = at_path(&self.home, &home_pin)?;
        let (store_present, store_same) = at_path(&plugins_root(&self.home), &store_pin)?;
        let (lock_present, lock_same) = at_path(&lock_path(&self.home), &self.lock_identity)?;
        Ok(Observation {
            same_home: home_same && store_same && lock_same,
            store_present: home_present && store_present,
            lock_present,
        })
    }

    /// The store directory and lock file this identity pinned.
    pub(super) fn store_handles(
        &self,
    ) -> std::result::Result<(same_file::Handle, &same_file::Handle), PublishedPluginReadError>
    {
        Ok((handle_of(&self.store_dir)?, &self.lock_identity))
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
            pinned: None,
            _lock: lock,
        })
    }
}

/// One runtime's identity of its plugin store. The home is pinned when the
/// runtime starts; the store is pinned by the first read that finds it in
/// that same home, and never refreshed. A home removed or replaced later reads
/// as [`StoreAbsent`] for the rest of the runtime's life, never as the new
/// home at the same path, and the runtime's writers write only into the
/// pinned home ([`Self::try_exclusive`]).
#[derive(Debug, Clone)]
pub struct StoreReadPin(std::sync::Arc<PinShared>);

#[derive(Debug)]
struct PinShared {
    home: PathBuf,
    state: std::sync::Mutex<PinState>,
}

#[derive(Debug)]
struct PinState {
    home: Option<std::sync::Arc<PinnedHome>>,
    store: Option<std::sync::Arc<StoreReadIdentity>>,
}

/// A home directory held open, so its identity outlives a rename or removal
/// of its path and anchored operations stay inside it.
#[derive(Debug)]
pub(super) struct PinnedHome {
    pub(super) dir: File,
    identity: same_file::Handle,
}

impl PinnedHome {
    fn open(path: &Path) -> std::io::Result<Self> {
        let dir = File::open(path)?;
        let identity = same_file::Handle::from_file(dir.try_clone()?)?;
        Ok(Self { dir, identity })
    }

    /// Whether a home is at `path` and whether it is this one.
    pub(super) fn at(&self, path: &Path) -> Result<(bool, bool), PublishedPluginReadError> {
        at_path(path, &self.identity)
    }
}

fn poisoned<T>(_: T) -> PublishedPluginReadError {
    PublishedPluginReadError::Read(anyhow::anyhow!("the plugin store read pin was poisoned"))
}

impl StoreReadPin {
    pub fn capture(home: &Path) -> Self {
        let pin = Self(std::sync::Arc::new(PinShared {
            home: home.to_owned(),
            state: std::sync::Mutex::new(PinState {
                home: PinnedHome::open(home).ok().map(std::sync::Arc::new),
                store: None,
            }),
        }));
        let _ = pin.identity();
        pin
    }

    pub fn home(&self) -> &Path {
        &self.0.home
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, PinState>, PublishedPluginReadError> {
        self.0.state.lock().map_err(poisoned)
    }

    /// The store a read pinned, if one has.
    pub(super) fn pinned_store(
        &self,
    ) -> Result<Option<std::sync::Arc<StoreReadIdentity>>, PublishedPluginReadError> {
        Ok(self.state()?.store.clone())
    }

    /// The pinned home; a pin taken while no home was at its path binds the
    /// home found there now, or `None` while there is still none.
    pub(super) fn bound_home(
        &self,
    ) -> Result<Option<std::sync::Arc<PinnedHome>>, PublishedPluginReadError> {
        if let Some(home) = &self.state()?.home {
            return Ok(Some(home.clone()));
        }
        let opened = match PinnedHome::open(self.home()) {
            Ok(home) => std::sync::Arc::new(home),
            Err(error) if is_absent(&error) => return Ok(None),
            Err(error) => return Err(absent_or_io(error)),
        };
        Ok(Some(self.state()?.home.get_or_insert(opened).clone()))
    }

    fn identity(&self) -> Result<std::sync::Arc<StoreReadIdentity>, PublishedPluginReadError> {
        if let Some(identity) = self.pinned_store()? {
            return Ok(identity);
        }
        let home = self.bound_home()?.ok_or(StoreAbsent::Missing)?;
        match home.at(self.home())? {
            (false, _) => return Err(StoreAbsent::Missing.into()),
            (true, false) => return Err(StoreAbsent::Replaced.into()),
            (true, true) => {}
        }
        let captured = StoreReadIdentity::capture(self.home())?;
        if handle_of(&captured.home_dir)? != home.identity {
            return Err(StoreAbsent::Replaced.into());
        }
        Ok(self
            .state()?
            .store
            .get_or_insert_with(|| std::sync::Arc::new(captured))
            .clone())
    }

    /// Whether a read has found this runtime's store yet.
    pub fn is_captured(&self) -> bool {
        self.state().is_ok_and(|state| state.store.is_some())
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
