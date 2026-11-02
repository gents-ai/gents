//! A held advisory lock on an open file.
//!
//! An advisory lock belongs to the open file description, which a child
//! forked by any thread of this process shares until it execs. Closing the
//! file alone therefore leaves the lock held for as long as such a child
//! lives; unlocking the description releases it for every copy at once, so a
//! [`FileLock`] unlocks explicitly when dropped.

use std::fs::{File, TryLockError};

#[derive(Debug)]
pub struct FileLock(File);

impl FileLock {
    pub fn shared(file: File) -> std::io::Result<Self> {
        file.lock_shared()?;
        Ok(Self(file))
    }

    pub fn exclusive(file: File) -> std::io::Result<Self> {
        file.lock()?;
        Ok(Self(file))
    }

    pub fn try_exclusive(file: File) -> Result<Self, TryLockError> {
        file.try_lock()?;
        Ok(Self(file))
    }

    pub fn adopt(file: File) -> Self {
        Self(file)
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "releasing a file lock failed; it is released when the file closes");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(path: &std::path::Path) -> File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap()
    }

    #[test]
    fn dropping_releases_the_lock_while_a_duplicate_descriptor_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let file = open(&path);
        let inherited = file.try_clone().unwrap();
        drop(FileLock::exclusive(file).unwrap());
        assert!(FileLock::try_exclusive(open(&path)).is_ok());
        drop(inherited);
    }

    #[test]
    fn a_plain_close_keeps_the_lock_while_a_duplicate_descriptor_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let file = open(&path);
        let inherited = file.try_clone().unwrap();
        file.lock().unwrap();
        drop(file);
        assert!(matches!(
            open(&path).try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        drop(inherited);
    }
}
