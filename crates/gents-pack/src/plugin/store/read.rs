//! Native premises: directory handles retain inode identity; a noncreating
//! openat is anchored to that directory, and shared advisory locks contend
//! with the existing exclusive lock. Lean models the decisions, not syscalls.
use super::*;
use std::fs::File;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StoreAbsent {
    #[error("plugin store has not been bootstrapped or its home was removed")]
    Missing,
    #[error("plugin store belongs to a replaced home")]
    Replaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Outcome {
    Acquired,
    Absent,
    Busy,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    same_home: bool,
    store_present: bool,
    lock_present: bool,
    contended: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Decision {
    outcome: Outcome,
    creates: bool,
}
fn acquire(o: Observation) -> Decision {
    if !o.same_home || !o.store_present || !o.lock_present {
        Decision {
            outcome: Outcome::Absent,
            creates: false,
        }
    } else {
        Decision {
            outcome: if o.contended {
                Outcome::Busy
            } else {
                Outcome::Acquired
            },
            creates: false,
        }
    }
}

/// Captured once per runtime, never in process-global state. Holding the open
/// directories prevents their identity from being reused after unlink/rename.
#[derive(Debug)]
pub struct StoreReadIdentity {
    home: PathBuf,
    home_dir: File,
    store_dir: File,
    lock_identity: same_file::Handle,
}

fn admit(observation: Observation) -> std::result::Result<(), PublishedPluginReadError> {
    match acquire(observation).outcome {
        Outcome::Acquired => Ok(()),
        Outcome::Busy => Err(PublishedPluginReadError::Busy),
        Outcome::Absent if !observation.same_home => Err(StoreAbsent::Replaced.into()),
        Outcome::Absent => Err(StoreAbsent::Missing.into()),
    }
}

fn absent_or_io(error: std::io::Error) -> PublishedPluginReadError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    ) {
        admit(Observation {
            same_home: true,
            store_present: false,
            lock_present: false,
            contended: false,
        })
        .err()
        .expect("missing read is absent")
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
    // SAFETY: name is NUL-terminated, fd is held through the call; on success
    // openat returns a new owned fd. No O_CREAT or write access is requested.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

impl StoreReadIdentity {
    pub fn capture(home: &Path) -> std::result::Result<Self, PublishedPluginReadError> {
        let home_dir = File::open(home).map_err(absent_or_io)?;
        #[cfg(unix)]
        let store_dir = child(&home_dir, c"plugins", true).map_err(absent_or_io)?;
        #[cfg(not(unix))]
        let store_dir = File::open(plugins_root(home)).map_err(absent_or_io)?;
        #[cfg(unix)]
        let lock = child(&store_dir, c".store.lock", false).map_err(absent_or_io)?;
        #[cfg(not(unix))]
        let lock = File::open(lock_path(home)).map_err(absent_or_io)?;
        let identity = Self {
            home: home.to_owned(),
            home_dir,
            store_dir,
            lock_identity: same_file::Handle::from_file(lock).map_err(absent_or_io)?,
        };
        identity.validate()?;
        Ok(identity)
    }

    /// Observe native state without deciding admission. The same decision
    /// replayed against Lean consumes this observation in validate/try_shared.
    fn observation(
        &self,
        contended: bool,
    ) -> std::result::Result<Observation, PublishedPluginReadError> {
        fn matches(
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
        let (home_present, home_same) = matches(&self.home, &home_pin)?;
        let (store_present, store_same) = matches(&plugins_root(&self.home), &store_pin)?;
        let (lock_present, lock_same) = matches(&lock_path(&self.home), &self.lock_identity)?;
        Ok(Observation {
            same_home: home_same && store_same && lock_same,
            store_present: home_present && store_present,
            lock_present,
            contended,
        })
    }

    pub fn validate(&self) -> std::result::Result<(), PublishedPluginReadError> {
        admit(self.observation(false)?)
    }

    pub fn try_shared(&self) -> std::result::Result<StoreGuard, PublishedPluginReadError> {
        self.validate()?;
        #[cfg(unix)]
        let file = child(&self.store_dir, c".store.lock", false).map_err(absent_or_io)?;
        #[cfg(not(unix))]
        let file = File::open(lock_path(&self.home)).map_err(absent_or_io)?;
        let opened = same_file::Handle::from_file(file.try_clone().map_err(absent_or_io)?)
            .map_err(absent_or_io)?;
        if opened != self.lock_identity {
            return Err(StoreAbsent::Replaced.into());
        }
        let lock = gents_store::local::file_lock::FileLock::try_shared(file);
        let contended = matches!(&lock, Err(std::fs::TryLockError::WouldBlock));
        admit(self.observation(contended)?)?;
        let lock = lock.map_err(|error| PublishedPluginReadError::Read(error.into()))?;
        Ok(StoreGuard {
            home: self.home.clone(),
            _lock: lock,
        })
    }
}

/// The original home handle is retained even before a plugin store exists.
/// Only a never-successful store capture can retry, and only in that same home.
#[derive(Debug, Clone)]
pub struct StoreReadPin(Arc<std::sync::Mutex<PinState>>);
#[derive(Debug)]
struct PinState {
    home: PathBuf,
    home_identity: Option<same_file::Handle>,
    store: Option<Arc<StoreReadIdentity>>,
}
impl StoreReadPin {
    pub fn capture(home: &Path) -> Self {
        let pin = Self(Arc::new(std::sync::Mutex::new(PinState {
            home: home.to_owned(),
            home_identity: same_file::Handle::from_path(home).ok(),
            store: None,
        })));
        let _ = pin.identity();
        pin
    }
    fn identity(&self) -> std::result::Result<Arc<StoreReadIdentity>, PublishedPluginReadError> {
        let mut state = self.0.lock().map_err(|_| {
            PublishedPluginReadError::Read(anyhow::anyhow!("store identity mutex poisoned"))
        })?;
        if let Some(identity) = &state.store {
            return Ok(identity.clone());
        }
        let current = same_file::Handle::from_path(&state.home).map_err(absent_or_io)?;
        if state
            .home_identity
            .as_ref()
            .is_some_and(|home| *home != current)
        {
            return Err(StoreAbsent::Replaced.into());
        }
        if state.home_identity.is_none() {
            state.home_identity = Some(current);
        }
        let identity = Arc::new(StoreReadIdentity::capture(&state.home)?);
        let captured_home =
            same_file::Handle::from_file(identity.home_dir.try_clone().map_err(absent_or_io)?)
                .map_err(absent_or_io)?;
        if state
            .home_identity
            .as_ref()
            .is_some_and(|home| *home != captured_home)
        {
            return Err(StoreAbsent::Replaced.into());
        }
        state.store = Some(identity.clone());
        Ok(identity)
    }
    pub fn is_captured(&self) -> bool {
        self.0
            .lock()
            .map(|state| state.store.is_some())
            .unwrap_or(false)
    }
    pub fn validate(&self) -> std::result::Result<(), PublishedPluginReadError> {
        self.identity()?.validate()
    }
    pub fn try_shared(&self) -> std::result::Result<StoreGuard, PublishedPluginReadError> {
        self.identity()?.try_shared()
    }
}

#[cfg(test)]
mod tests;
