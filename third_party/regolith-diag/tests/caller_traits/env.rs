//! `Env` and the files it hands out (`ReadFile`, `WriteFile`, `LogFile`, the
//! directory lock and the join handle): a database opened on a caller's
//! environment makes every one of its calls through that environment, on
//! paths under the directories the caller named.
//!
//! The environment here wraps the standard one and records each method the
//! database calls. One workload reaches every method that has a caller; the
//! test names the few that have none, so a method wired in later, or a new
//! one nothing calls, shows up here.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use regolith::env::{
    Capabilities, DiskSpace, Env, FileLock, FileMeta, JoinHandle, LogFile, ReadDir, ReadFile,
    StdEnv, WriteFile, WriteMode,
};
use regolith::{BackupEngine, Checkpoint, Db, DurabilityMode, Options};

/// What the database called, and anything it called with a path outside the
/// directories it was given.
#[derive(Default)]
struct Calls {
    counts: Mutex<BTreeMap<&'static str, usize>>,
    roots: Mutex<Vec<PathBuf>>,
    outside: Mutex<Vec<(&'static str, PathBuf)>>,
    /// Armed: the next sync of a log fails, once.
    fail_log_sync: AtomicBool,
}

impl Calls {
    fn note(&self, method: &'static str) {
        *self.counts.lock().unwrap().entry(method).or_default() += 1;
    }

    /// Note a call that names `path`, which must be under a root.
    fn at(&self, method: &'static str, path: &Path) {
        self.note(method);
        if !self
            .roots
            .lock()
            .unwrap()
            .iter()
            .any(|root| path.starts_with(root))
        {
            self.outside
                .lock()
                .unwrap()
                .push((method, path.to_path_buf()));
        }
    }

    fn called(&self) -> BTreeSet<&'static str> {
        self.counts.lock().unwrap().keys().copied().collect()
    }

    fn count(&self, method: &'static str) -> usize {
        self.counts
            .lock()
            .unwrap()
            .get(method)
            .copied()
            .unwrap_or(0)
    }
}

/// The standard environment, recording every call. With `own_log` unset it
/// leaves `open_log` to the trait's default, as an environment that has not
/// said how it writes at an offset does.
struct Recording {
    inner: StdEnv,
    calls: Arc<Calls>,
    own_log: bool,
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording").finish_non_exhaustive()
    }
}

struct Read(Box<dyn ReadFile>, Arc<Calls>);

impl ReadFile for Read {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.1.note("ReadFile::read_exact_at");
        self.0.read_exact_at(offset, buf)
    }

    fn len(&self) -> io::Result<u64> {
        self.1.note("ReadFile::len");
        self.0.len()
    }

    fn is_empty(&self) -> io::Result<bool> {
        self.1.note("ReadFile::is_empty");
        self.0.is_empty()
    }
}

struct Write(Box<dyn WriteFile>, Arc<Calls>);

impl WriteFile for Write {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.1.note("WriteFile::write_all");
        self.0.write_all(buf)
    }

    fn write_all_vectored(&mut self, slices: &[&[u8]]) -> io::Result<()> {
        self.1.note("WriteFile::write_all_vectored");
        self.0.write_all_vectored(slices)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.1.note("WriteFile::flush");
        self.0.flush()
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.1.note("WriteFile::sync_all");
        self.0.sync_all()
    }

    fn sync_data(&mut self) -> io::Result<()> {
        self.1.note("WriteFile::sync_data");
        self.0.sync_data()
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.1.note("WriteFile::set_len");
        self.0.set_len(len)
    }

    fn len(&self) -> io::Result<u64> {
        self.1.note("WriteFile::len");
        self.0.len()
    }

    fn is_empty(&self) -> io::Result<bool> {
        self.1.note("WriteFile::is_empty");
        self.0.is_empty()
    }
}

struct Log(Box<dyn LogFile>, Arc<Calls>);

impl LogFile for Log {
    fn write_all_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.1.note("LogFile::write_all_at");
        self.0.write_all_at(offset, buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.1.note("LogFile::sync_data");
        if self.1.fail_log_sync.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("a test's log sync fails"));
        }
        self.0.sync_data()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.1.note("LogFile::set_len");
        self.0.set_len(len)
    }

    fn start_writeback(&self, offset: u64, len: u64) {
        self.1.note("LogFile::start_writeback");
        self.0.start_writeback(offset, len);
    }
}

/// The directory lock: the trait has no method, so what is recorded is that
/// the database let go of it.
struct Lock(#[allow(dead_code)] Box<dyn FileLock>, Arc<Calls>);

impl FileLock for Lock {}

impl Drop for Lock {
    fn drop(&mut self) {
        self.1.note("FileLock dropped");
    }
}

struct Join(Box<dyn JoinHandle>, Arc<Calls>);

impl JoinHandle for Join {
    fn join(self: Box<Self>) {
        self.1.note("JoinHandle::join");
        self.0.join();
    }
}

impl Env for Recording {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.calls.at("Env::create_dir_all", path);
        self.inner.create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.calls.at("Env::read_dir", path);
        self.inner.read_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.calls.at("Env::open_read", path);
        let file = self.inner.open_read(path)?;
        Ok(Box::new(Read(file, self.calls.clone())))
    }

    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.calls.at("Env::open_write", path);
        let file = self.inner.open_write(path, mode)?;
        Ok(Box::new(Write(file, self.calls.clone())))
    }

    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        self.calls.at("Env::open_log", path);
        let log = if self.own_log {
            self.inner.open_log(path, mode)?
        } else {
            // What the trait's default body does, which a method that
            // records the call cannot leave to it: a log over `open_write`.
            DefaultLog(self).open_log(path, mode)?
        };
        Ok(Box::new(Log(log, self.calls.clone())))
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.calls.at("Env::metadata", path);
        self.inner.metadata(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.calls.at("Env::remove_file", path);
        self.inner.remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.calls.at("Env::rename", from);
        self.calls.at("Env::rename", to);
        self.inner.rename(from, to)
    }

    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.calls.at("Env::hard_link", src);
        self.calls.at("Env::hard_link", dst);
        self.inner.hard_link(src, dst)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.calls.at("Env::sync_dir", path);
        self.inner.sync_dir(path)
    }

    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        self.calls.at("Env::lock_file", path);
        let lock = self.inner.lock_file(path, exclusive)?;
        Ok(Box::new(Lock(lock, self.calls.clone())))
    }

    fn capabilities(&self) -> Capabilities {
        self.calls.note("Env::capabilities");
        self.inner.capabilities()
    }

    fn now_micros(&self) -> Option<u64> {
        self.calls.note("Env::now_micros");
        self.inner.now_micros()
    }

    fn unix_secs(&self) -> Option<u64> {
        self.calls.note("Env::unix_secs");
        self.inner.unix_secs()
    }

    fn spawn(
        &self,
        name: &str,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.calls.note("Env::spawn");
        let handle = self.inner.spawn(name, body)?;
        Ok(Box::new(Join(handle, self.calls.clone())))
    }

    fn disk_space(&self, path: &Path) -> io::Result<Option<DiskSpace>> {
        self.calls.at("Env::disk_space", path);
        self.inner.disk_space(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.calls.at("Env::exists", path);
        self.inner.exists(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.calls.at("Env::is_dir", path);
        self.inner.is_dir(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.calls.at("Env::read", path);
        // Through this environment's own handle, as the default body does.
        let file = self.open_read(path)?;
        let mut bytes = vec![0; file.len()? as usize];
        file.read_exact_at(0, &mut bytes)?;
        Ok(bytes)
    }

    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.calls.at("Env::write", path);
        self.inner.write(path, data)
    }

    fn drop_page_cache(&self, path: &Path) {
        self.calls.at("Env::drop_page_cache", path);
        self.inner.drop_page_cache(path);
    }
}

/// An environment that is `Recording` in everything but `open_log`, which
/// it does not define: calling `open_log` on it runs the trait's default.
struct DefaultLog<'a>(&'a Recording);

impl std::fmt::Debug for DefaultLog<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Env for DefaultLog<'_> {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.0.create_dir_all(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.0.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.0.open_read(path)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.0.open_write(path, mode)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.0.metadata(path)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.0.remove_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.0.rename(from, to)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.0.hard_link(src, dst)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.0.sync_dir(path)
    }
    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        self.0.lock_file(path, exclusive)
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn now_micros(&self) -> Option<u64> {
        self.0.now_micros()
    }
    fn unix_secs(&self) -> Option<u64> {
        self.0.unix_secs()
    }
    fn spawn(
        &self,
        name: &str,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.0.spawn(name, body)
    }
}

/// Every method of the five traits, and the drop of the lock, which is all
/// a `FileLock` does.
const EVERY_METHOD: [&str; 38] = [
    "Env::create_dir_all",
    "Env::read_dir",
    "Env::open_read",
    "Env::open_write",
    "Env::open_log",
    "Env::metadata",
    "Env::remove_file",
    "Env::rename",
    "Env::hard_link",
    "Env::sync_dir",
    "Env::lock_file",
    "Env::capabilities",
    "Env::now_micros",
    "Env::unix_secs",
    "Env::spawn",
    "Env::disk_space",
    "Env::exists",
    "Env::is_dir",
    "Env::read",
    "Env::write",
    "Env::drop_page_cache",
    "ReadFile::read_exact_at",
    "ReadFile::len",
    "ReadFile::is_empty",
    "WriteFile::write_all",
    "WriteFile::write_all_vectored",
    "WriteFile::flush",
    "WriteFile::sync_all",
    "WriteFile::sync_data",
    "WriteFile::set_len",
    "WriteFile::len",
    "WriteFile::is_empty",
    "LogFile::write_all_at",
    "LogFile::sync_data",
    "LogFile::set_len",
    "LogFile::start_writeback",
    "FileLock dropped",
    "JoinHandle::join",
];

/// The methods no part of regolith calls. All three are conveniences the
/// traits give a default body, so an implementor writes none of them.
const NO_CALLER: [&str; 3] = ["Env::write", "ReadFile::is_empty", "WriteFile::is_empty"];

fn key(i: usize) -> Vec<u8> {
    format!("key-{i:03}").into_bytes()
}

/// A database's whole life on `env`, in `dir`: writes made durable one by
/// one, a flush, reads, a compaction on the worker, a log sync that fails, a
/// checkpoint, a backup and its restore; then a reopen that replays the
/// log, drops its damaged end and writes the next log, a clean close, and a
/// read-only open.
fn live_a_whole_life(env: &Arc<Recording>, dir: &Path) {
    let calls = &env.calls;
    let db_dir = dir.join("db");
    let options = || {
        let env: Arc<dyn Env> = env.clone();
        Options::default()
            .env(env)
            .durability(DurabilityMode::Immediate)
            .evict_compaction_data_from_page_cache(true)
    };

    let db = Db::open(&db_dir, options()).unwrap();
    for i in 0..3 {
        db.put(&key(i), b"first").unwrap();
    }
    db.flush().unwrap();
    assert_eq!(db.get(&key(0)).unwrap(), Some(b"first".to_vec()));
    db.put(&key(1), b"second").unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(db.get(&key(1)).unwrap(), Some(b"second".to_vec()));

    // A log sync that fails: the write is refused, and its record is cut
    // back out of the log. The next write goes through.
    calls.fail_log_sync.store(true, Ordering::SeqCst);
    assert!(db.put(b"refused", b"v").is_err());
    db.put(b"after", b"v").unwrap();
    assert_eq!(db.get(b"refused").unwrap(), None);

    // The database asks its environment on every platform; the standard one
    // answers from the file system on Unix, and on Windows, where the
    // standard library has no call for free space, it answers nothing.
    let asked = calls.count("Env::disk_space");
    let available = db.get_int_property("regolith.disk-available-bytes");
    assert!(calls.count("Env::disk_space") > asked);
    #[cfg(unix)]
    assert!(available.is_some());
    #[cfg(windows)]
    assert!(available.is_none());
    Checkpoint::new(&db)
        .unwrap()
        .create(dir.join("checkpoint"))
        .wait()
        .unwrap();
    let backup_env: Arc<dyn Env> = env.clone();
    let mut backups = BackupEngine::open_with_env(dir.join("backups"), backup_env).unwrap();
    let backup = backups.create_backup(&db).unwrap();
    backups.restore(backup, dir.join("restored"), None).unwrap();

    // Left in the log, and large enough that the reopen writes it to the
    // next log as one vectored write. The write after it is the record the
    // damage below lands in.
    db.put(b"large", &vec![7u8; 6 * 1024]).unwrap();
    db.put(b"last", b"v").unwrap();
    drop(db);

    // The newest log ends in a damaged record: the open cuts it off.
    let mut logs: Vec<PathBuf> = std::fs::read_dir(db_dir.join("wal"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    logs.sort();
    let newest = logs.pop().expect("a log");
    let mut bytes = std::fs::read(&newest).unwrap();
    *bytes.last_mut().unwrap() ^= 0xFF;
    std::fs::write(&newest, &bytes).unwrap();

    let db = Db::open(&db_dir, options()).unwrap();
    assert_eq!(db.get(&key(2)).unwrap(), Some(b"first".to_vec()));
    assert_eq!(db.get(b"after").unwrap(), Some(b"v".to_vec()));
    db.close().unwrap();
    drop(db);

    let db = Db::open_read_only(&db_dir, options()).unwrap();
    assert_eq!(db.get(&key(0)).unwrap(), Some(b"first".to_vec()));
    drop(db);

    // The log writeback lane, asked for (`Options::log_writeback`): on Linux
    // and Apple, with a worker, the log is handed to the environment's log as it grows
    // past each eighth of the write buffer, here each MiB. The lane runs on
    // a thread of its own, so the call is waited for.
    let db = Db::open(
        dir.join("lane"),
        options().write_buffer_size(8 << 20).log_writeback(true),
    )
    .unwrap();
    let value = vec![9u8; 256 * 1024];
    for i in 0..6 {
        db.put(&key(i), value.as_slice()).unwrap();
    }
    if cfg!(any(target_os = "linux", target_vendor = "apple")) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut pause = std::time::Duration::from_micros(50);
        while calls.count("LogFile::start_writeback") == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the lane handed nothing over in 20 s"
            );
            std::thread::sleep(pause);
            pause = (pause * 2).min(std::time::Duration::from_millis(20));
        }
    }
    db.close().unwrap();
}

/// An environment recording into fresh `Calls`, allowed the paths under
/// `dir`.
fn recording(dir: &Path, own_log: bool) -> Arc<Recording> {
    let calls = Arc::new(Calls::default());
    calls.roots.lock().unwrap().push(dir.to_path_buf());
    Arc::new(Recording {
        inner: StdEnv,
        calls,
        own_log,
    })
}

/// The methods of `EVERY_METHOD` that `calls` never saw.
fn never_called(calls: &Calls) -> BTreeSet<&'static str> {
    let called = calls.called();
    for method in &called {
        assert!(EVERY_METHOD.contains(method), "{method} is not listed");
    }
    EVERY_METHOD
        .into_iter()
        .filter(|method| !called.contains(method))
        .collect()
}

#[test]
fn a_database_calls_every_method_of_its_environment_under_its_own_directories() {
    let dir = tempfile::tempdir().unwrap();
    let env = recording(dir.path(), true);
    live_a_whole_life(&env, dir.path());
    let calls = &env.calls;

    // The database calls each method on every platform, whatever the
    // standard environment does with it there: `sync_dir` syncs the
    // directory on Unix and returns at once on Windows, which has no
    // directory sync; `drop_page_cache` advises the kernel on Linux and does
    // nothing elsewhere; positional reads and log writes are `pread` and
    // `pwrite` on Unix, `seek_read` and `seek_write` on Windows, with no
    // shared cursor or lock on either.
    //
    // An environment with a log of its own is never asked the length of a
    // file it writes: only the default log asks (the test below).
    let mut expected: BTreeSet<&str> = NO_CALLER.into_iter().collect();
    expected.insert("WriteFile::len");
    // The log writeback lane runs on Linux and Apple alone, the hosts with a
    // way to start the writeback.
    if !cfg!(any(target_os = "linux", target_vendor = "apple")) {
        expected.insert("LogFile::start_writeback");
    }
    assert_eq!(never_called(calls), expected);
    let outside = calls.outside.lock().unwrap();
    assert!(
        outside.is_empty(),
        "paths outside the directories: {outside:?}"
    );

    // One worker was started and joined, the lock was taken for each open
    // and let go, and the one failed sync cut its record back.
    assert_eq!(calls.count("Env::spawn"), calls.count("JoinHandle::join"));
    assert_eq!(
        calls.count("Env::lock_file"),
        calls.count("FileLock dropped")
    );
    assert_eq!(calls.count("LogFile::set_len"), 1);
}

/// An environment that leaves `open_log` to the trait's default serves the
/// same life: the log is then written through `open_write`, and the default
/// asks that file its length.
#[test]
fn the_default_log_serves_a_database_through_open_write() {
    let dir = tempfile::tempdir().unwrap();
    let env = recording(dir.path(), false);
    live_a_whole_life(&env, dir.path());
    let calls = &env.calls;

    // The log writeback lane runs on Linux and Apple alone, the hosts with a
    // way to start the writeback.
    let mut expected: BTreeSet<&str> = NO_CALLER.into_iter().collect();
    if !cfg!(any(target_os = "linux", target_vendor = "apple")) {
        expected.insert("LogFile::start_writeback");
    }
    assert_eq!(never_called(calls), expected);
    let outside = calls.outside.lock().unwrap();
    assert!(
        outside.is_empty(),
        "paths outside the directories: {outside:?}"
    );
    assert!(calls.count("WriteFile::len") > 0);
}
