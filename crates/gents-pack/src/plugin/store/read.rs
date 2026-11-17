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
enum Mode {
    Read,
    Bootstrap,
    Writer,
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
fn acquire(mode: Mode, o: Observation) -> Decision {
    if !o.same_home || (mode == Mode::Read && (!o.store_present || !o.lock_present)) {
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
            creates: mode != Mode::Read && (!o.store_present || !o.lock_present),
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

    /// Read-only identity checks cannot recreate a retired/replaced pathname.
    pub fn validate(&self) -> std::result::Result<(), PublishedPluginReadError> {
        let current = same_file::Handle::from_path(&self.home).map_err(absent_or_io)?;
        let pinned = same_file::Handle::from_file(self.home_dir.try_clone().map_err(absent_or_io)?)
            .map_err(absent_or_io)?;
        if current != pinned {
            return Err(StoreAbsent::Replaced.into());
        }
        let current =
            same_file::Handle::from_path(plugins_root(&self.home)).map_err(absent_or_io)?;
        let pinned =
            same_file::Handle::from_file(self.store_dir.try_clone().map_err(absent_or_io)?)
                .map_err(absent_or_io)?;
        if current != pinned {
            return Err(StoreAbsent::Replaced.into());
        }
        let current = same_file::Handle::from_path(lock_path(&self.home)).map_err(absent_or_io)?;
        if current != self.lock_identity {
            return Err(StoreAbsent::Replaced.into());
        }
        Ok(())
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
        self.validate()?;
        let contended = matches!(&lock, Err(std::fs::TryLockError::WouldBlock));
        let decision = acquire(
            Mode::Read,
            Observation {
                same_home: true,
                store_present: true,
                lock_present: true,
                contended,
            },
        );
        debug_assert!(!decision.creates);
        if decision.outcome == Outcome::Busy {
            return Err(PublishedPluginReadError::Busy);
        }
        let lock = lock.map_err(|error| PublishedPluginReadError::Read(error.into()))?;
        Ok(StoreGuard {
            home: self.home.clone(),
            _lock: lock,
        })
    }
}

/// Capturing a failed pin is intentional: a runtime cannot later adopt a new
/// home because the old one happened to be absent at its creation.
#[derive(Debug, Clone)]
pub struct StoreReadPin(std::result::Result<Arc<StoreReadIdentity>, Arc<PublishedPluginReadError>>);
impl StoreReadPin {
    pub fn capture(home: &Path) -> Self {
        Self(
            StoreReadIdentity::capture(home)
                .map(Arc::new)
                .map_err(Arc::new),
        )
    }
    pub fn try_shared(&self) -> std::result::Result<StoreGuard, PublishedPluginReadError> {
        match &self.0 {
            Ok(identity) => identity.try_shared(),
            Err(error) => match error.as_ref() {
                PublishedPluginReadError::StoreAbsent(reason) => Err((*reason).into()),
                _ => Err(PublishedPluginReadError::Pinned(error.clone())),
            },
        }
    }
}

#[cfg(test)]
mod tests;
