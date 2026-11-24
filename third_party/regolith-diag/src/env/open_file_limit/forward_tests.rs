//! The limit sits between a database and its environment, so every method
//! an environment may override has to reach the environment's own. Left to
//! its default body here, such a method would run in place of what the
//! caller's environment wrote for it.

use std::sync::Mutex;

use super::*;
use crate::env::{DiskSpace, LogFile, MemEnv};

/// An `Env` over [`MemEnv`] that overrides every method `Env` gives a
/// default body, and records which of them ran.
#[derive(Debug, Default)]
struct Overrides {
    inner: MemEnv,
    ran: Mutex<Vec<&'static str>>,
}

impl Overrides {
    fn note(&self, method: &'static str) {
        self.ran.lock().unwrap().push(method);
    }

    /// The overrides that ran since the last time this was asked.
    fn take(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.ran.lock().unwrap())
    }
}

impl Env for Overrides {
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        self.note("open_log");
        self.inner.open_log(path, mode)
    }
    fn disk_space(&self, _path: &Path) -> io::Result<Option<DiskSpace>> {
        self.note("disk_space");
        Ok(Some(DiskSpace::new(7, 9, 3, 4)))
    }
    fn exists(&self, path: &Path) -> bool {
        self.note("exists");
        self.inner.exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.note("is_dir");
        self.inner.is_dir(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.note("read");
        self.inner.read(path)
    }
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.note("write");
        self.inner.write(path, data)
    }
    fn drop_page_cache(&self, _path: &Path) {
        self.note("drop_page_cache");
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(path)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.inner.open_write(path, mode)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(path)
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

fn limited() -> (Arc<Overrides>, OpenFileLimit) {
    let inner = Arc::new(Overrides::default());
    let env = OpenFileLimit::new(inner.clone(), 4);
    env.create_dir_all(Path::new("/db")).unwrap();
    (inner, env)
}

#[test]
fn every_method_with_a_default_body_reaches_the_environments_own() {
    let (inner, env) = limited();
    let dir = Path::new("/db");
    let file = dir.join("MANIFEST");

    env.write(&file, b"bytes").unwrap();
    assert_eq!(inner.take(), ["write"]);
    assert_eq!(env.read(&file).unwrap(), b"bytes");
    assert_eq!(inner.take(), ["read"]);
    assert!(env.exists(&file));
    assert_eq!(inner.take(), ["exists"]);
    assert!(env.is_dir(dir));
    assert_eq!(inner.take(), ["is_dir"]);
    assert_eq!(
        env.disk_space(dir).unwrap(),
        Some(DiskSpace::new(7, 9, 3, 4)),
        "the environment's own answer, not the default's none"
    );
    assert_eq!(inner.take(), ["disk_space"]);
    env.drop_page_cache(&file);
    assert_eq!(inner.take(), ["drop_page_cache"]);
    drop(
        env.open_log(&dir.join("000001.log"), WriteMode::Truncate)
            .unwrap(),
    );
    assert_eq!(inner.take(), ["open_log"]);
}

/// A table is the one file the limit reads itself: read whole, it goes
/// through the limit's own handle, so the read counts against the bound
/// like any other read of the table.
#[test]
fn a_whole_table_is_read_through_the_limit() {
    let (inner, env) = limited();
    let table = Path::new("/db").join("000001.sst");
    env.write(&table, b"a table").unwrap();
    assert_eq!(inner.take(), ["write"]);

    assert_eq!(env.read(&table).unwrap(), b"a table");
    assert!(inner.take().is_empty(), "not the environment's own read");
    assert!(env.open_count() <= 4);
}
