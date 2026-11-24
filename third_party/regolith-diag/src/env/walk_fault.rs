//! A test [`Env`] whose walks of one directory fail part way.
//!
//! Every call goes to a [`MemEnv`], except that a walk of the armed
//! directory hands out its first `after` entries and then yields an error,
//! the shape a `readdir` that fails between two buffers takes. The callers
//! that must not act on a walk they could not finish (the backup collection
//! above all) are tested against it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, MemEnv, ReadDir, ReadFile, WriteFile,
    WriteMode,
};

/// A [`MemEnv`] whose walks of one directory fail after a number of
/// entries; see the module docs.
#[derive(Debug, Default)]
pub(crate) struct WalkFault {
    pub(crate) inner: MemEnv,
    /// The directory whose walks fail, and after how many entries.
    armed: Mutex<Option<(PathBuf, usize)>>,
}

impl WalkFault {
    /// From now on, every walk of `dir` fails after `after` entries.
    pub(crate) fn arm(&self, dir: &Path, after: usize) {
        *self.armed.lock().unwrap() = Some((dir.to_path_buf(), after));
    }
}

impl Env for WalkFault {
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        let walk = self.inner.read_dir(path)?;
        let after = match &*self.armed.lock().unwrap() {
            Some((dir, after)) if dir == path => *after,
            _ => return Ok(walk),
        };
        let failure = std::iter::once_with(|| Err(io::Error::other("the walk broke")));
        Ok(Box::new(walk.take(after).chain(failure)))
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(path)
    }
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn super::LogFile>> {
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.inner.open_write(path, mode)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(path)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.inner.hard_link(src, dst)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
    }
    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        self.inner.lock_file(path, exclusive)
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn now_micros(&self) -> Option<u64> {
        self.inner.now_micros()
    }
    fn unix_secs(&self) -> Option<u64> {
        self.inner.unix_secs()
    }
    fn spawn(
        &self,
        name: &str,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.inner.spawn(name, body)
    }
}
