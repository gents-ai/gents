//! Cross-process locking of a database directory, for [`StdEnv`].
//!
//! [`super::StdEnv`] excludes other processes with `flock` on unix
//! and with an exclusive share mode on Windows. Both take a `LOCK`
//! file inside the database directory and both release it when the
//! guard drops.
//!
//! # Where there is no flock
//!
//! Every other target gets [`DirectoryRegistry`]: exclusion between
//! [`crate::Db`] handles recorded **in process memory**, with no file
//! on disk. That is a deliberate decision, not a cfg accident, and the
//! narrower guarantee is reported honestly through
//! [`super::Capabilities::file_lock`], which is `false` there and which
//! [`crate::Db::capabilities`] hands back to the caller.
//!
//! The registry is a complete guarantee exactly where it is used. wasm
//! is one process with one linear memory holding one copy of regolith's
//! state, so a second writer can only come from a second [`crate::Db`]
//! in that same process, which the registry sees. A [`super::MemEnv`]
//! and an OPFS mount are likewise scoped to one process, so each keeps
//! its own registry rather than sharing the process-wide one: two
//! `MemEnv`s are two different filesystems and must not collide on a
//! shared path.
//!
//! The alternative regolith used to ship was a `create_new(true)` proxy:
//! opening failed when a `LOCK` file already existed. That has the
//! severe failure mode a registry does not - a crash or an unclean
//! unload leaves the file behind, and every later open fails
//! permanently with a message blaming a second process that cannot
//! exist. Registry state dies with the process that holds it, so a
//! crash can never leave a stale lock.
//!
//! Two `Db` handles opened on one path inside a single process are
//! rejected on every target: `flock` rejects them because regolith opens a
//! separate descriptor per handle, and the registry rejects them
//! because it records the path. What neither prevents is a second
//! *process* on a target with no file locking, and there regolith says so
//! through `Capabilities::file_lock` instead of pretending.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kovan_map::HopscotchMap;

use super::FileLock;

/// In-process exclusion between database handles on one directory.
///
/// Used by every environment whose
/// [`super::Capabilities::file_lock`] is `false`. Holding a guard
/// marks the directory taken; dropping it releases the mark. One
/// writer excludes every other holder, and any number of readers
/// coexist.
///
/// A lock-free map, with no mutex around it: each acquire and each release
/// is one atomic read-modify-write of the directory's entry
/// ([`HopscotchMap::compute`]), which sees the holders the entry has then
/// and writes the next state in the same step. So two opens of one
/// directory never both see it free, and an open and a close of different
/// directories never wait on each other.
#[derive(Debug, Default)]
pub(super) struct DirectoryRegistry {
    /// Who holds each path. An absent entry means nobody does.
    held: HopscotchMap<PathBuf, Holders>,
}

/// The holders of one directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holders {
    /// One read-write handle.
    Writer,
    /// This many read-only handles, at least one.
    Readers(usize),
}

impl DirectoryRegistry {
    /// Take `db_dir`, exclusively when `exclusive`.
    ///
    /// Reports [`std::io::ErrorKind::AlreadyExists`] when a
    /// conflicting handle is already open on the same path in this
    /// process, matching what `flock` reports on unix.
    pub(super) fn acquire(
        self: &Arc<Self>,
        db_dir: &Path,
        exclusive: bool,
    ) -> io::Result<Box<dyn FileLock>> {
        let path = db_dir.to_path_buf();
        let mut taken = false;
        // The closure runs once, on the entry as it is at that instant. A
        // conflict writes the entry back as it found it.
        self.held.compute(path.clone(), |held| {
            let next = match (held, exclusive) {
                (None, true) => Holders::Writer,
                (None, false) => Holders::Readers(1),
                (Some(Holders::Readers(readers)), false) => Holders::Readers(readers + 1),
                (Some(conflicting), _) => return Some(*conflicting),
            };
            taken = true;
            Some(next)
        });
        if !taken {
            return Err(conflict(&path));
        }
        Ok(Box::new(RegistryLock {
            registry: Arc::clone(self),
            path,
            exclusive,
        }))
    }

    fn release(&self, path: &Path, exclusive: bool) {
        // `None` removes the entry: the writer left, or the last reader did.
        self.held.compute(path.to_path_buf(), |held| match held {
            Some(Holders::Readers(readers)) if !exclusive && *readers > 1 => {
                Some(Holders::Readers(readers - 1))
            }
            _ => None,
        });
    }
}

/// The process-wide registry, for [`super::StdEnv`] on a target with
/// no `flock`. One registry for the process is right there because the
/// paths it records are real filesystem paths that every `StdEnv` in
/// the process shares.
#[cfg(not(any(unix, windows)))]
pub(super) fn process_registry() -> &'static Arc<DirectoryRegistry> {
    static REGISTRY: std::sync::OnceLock<Arc<DirectoryRegistry>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(Arc::default)
}

fn conflict(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "database directory is already open in this process: {}",
            path.display()
        ),
    )
}

/// A held entry in a [`DirectoryRegistry`]. Releases on drop.
#[derive(Debug)]
struct RegistryLock {
    registry: Arc<DirectoryRegistry>,
    path: PathBuf,
    exclusive: bool,
}

impl FileLock for RegistryLock {}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        self.registry.release(&self.path, self.exclusive);
    }
}

/// Whether this target has real cross-process file locking.
pub(super) const SUPPORTS_FILE_LOCK: bool = cfg!(any(unix, windows));

/// Acquire the directory lock for `db_dir`.
///
/// `exclusive` takes a write lock (read-write open); otherwise a
/// shared lock (read-only open). Reports
/// [`std::io::ErrorKind::AlreadyExists`] when another process holds a
/// conflicting lock.
pub(super) fn acquire(db_dir: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
    #[cfg(any(unix, windows))]
    {
        real::acquire(db_dir, exclusive)
    }
    #[cfg(not(any(unix, windows)))]
    {
        process_registry().acquire(db_dir, exclusive)
    }
}

#[cfg(unix)]
pub(crate) mod diag {
    use std::backtrace::Backtrace;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, Weak};
    use std::time::Instant;

    use crate::engine::RegolithEngine;

    static ENGINES: Mutex<Vec<(PathBuf, Weak<RegolithEngine>)>> = Mutex::new(Vec::new());
    static WATCH: Mutex<Vec<((u64, u64), Instant)>> = Mutex::new(Vec::new());

    pub(crate) fn register_engine(db_dir: &Path, engine: &Arc<RegolithEngine>) {
        let mut engines = ENGINES.lock().unwrap_or_else(|e| e.into_inner());
        engines.retain(|(_, weak)| weak.strong_count() > 0);
        engines.push((db_dir.to_path_buf(), Arc::downgrade(engine)));
    }

    pub(crate) fn emit(text: &str) {
        eprint!("{text}");
        if let Some(dir) = std::env::var_os("LOCKDIAG_DIR") {
            use std::io::Write;
            let path = std::path::Path::new(&dir).join(format!("lockdiag-{}.log", std::process::id()));
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = file.write_all(text.as_bytes());
            }
        }
    }

    pub(crate) fn thread_label() -> String {
        let t = std::thread::current();
        format!("{:?}/{}", t.id(), t.name().unwrap_or("?"))
    }

    pub(crate) fn watched(id: (u64, u64)) -> Option<Instant> {
        WATCH
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(w, _)| *w == id)
            .map(|(_, t)| *t)
    }

    fn file_id(path: &Path) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.dev(), meta.ino()))
    }

    fn read(path: &str) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// Everything visible at the instant a conflicting lock is seen.
    pub(crate) fn on_conflict(db_dir: &Path, exclusive: bool) {
        let lock_path = db_dir.join("LOCK");
        let id = file_id(&lock_path);
        let started = Instant::now();
        if let Some(id) = id {
            WATCH
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((id, started));
        }
        let pid = std::process::id();
        let mut out = String::new();
        use std::fmt::Write;
        let _ = writeln!(
            out,
            "LOCKDIAG conflict pid={pid} thread={} exclusive_request={exclusive} path={} id={id:?}",
            thread_label(),
            lock_path.display()
        );
        // /proc/locks: lines for this inode.
        if let Some((dev, ino)) = id {
            let major = (dev >> 8) & 0xfff;
            let minor = (dev & 0xff) | ((dev >> 12) & 0xfff00);
            let needle = format!(":{ino} ");
            let needle_alt = format!("{major:02x}:{minor:02x}:{ino} ");
            for line in read("/proc/locks").lines() {
                if line.contains(&needle_alt) || line.contains(&needle) {
                    let _ = writeln!(out, "LOCKDIAG /proc/locks: {line}");
                }
            }
        }
        // Every process with an fd on this LOCK inode.
        if let Ok(procs) = std::fs::read_dir("/proc") {
            for proc_entry in procs.flatten() {
                let name = proc_entry.file_name();
                let Some(p) = name.to_str().filter(|n| n.chars().all(|c| c.is_ascii_digit())) else {
                    continue;
                };
                let Ok(fds) = std::fs::read_dir(proc_entry.path().join("fd")) else {
                    continue;
                };
                for fd in fds.flatten() {
                    let Ok(target) = std::fs::read_link(fd.path()) else {
                        continue;
                    };
                    if target == lock_path || file_id(&fd.path()) == id {
                        let status = read(&format!("/proc/{p}/status"));
                        let ppid = status
                            .lines()
                            .find(|l| l.starts_with("PPid:"))
                            .unwrap_or("");
                        let fdinfo = read(&format!(
                            "/proc/{p}/fdinfo/{}",
                            fd.file_name().to_string_lossy()
                        ));
                        let _ = writeln!(
                            out,
                            "LOCKDIAG fd holder pid={p} comm={} {ppid} fd={} target={} fdinfo={}",
                            read(&format!("/proc/{p}/comm")).trim(),
                            fd.file_name().to_string_lossy(),
                            target.display(),
                            fdinfo.replace('\n', " | ")
                        );
                    }
                }
            }
        }
        // Live engines registered on this directory.
        {
            let engines = ENGINES.lock().unwrap_or_else(|e| e.into_inner());
            for (dir, weak) in engines.iter() {
                if file_id(&dir.join("LOCK")) != id {
                    continue;
                }
                match weak.upgrade() {
                    Some(engine) => {
                        let _ = writeln!(
                            out,
                            "LOCKDIAG live engine dir={} strong={} weak={} close_state={}",
                            dir.display(),
                            Arc::strong_count(&engine) - 1,
                            Arc::weak_count(&engine),
                            engine.diag_close_state()
                        );
                    }
                    None => {
                        let _ = writeln!(
                            out,
                            "LOCKDIAG dead engine dir={} (strong=0)",
                            dir.display()
                        );
                    }
                }
            }
        }
        // Threads of this process.
        if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
            let mut names: Vec<String> = tasks
                .flatten()
                .map(|t| {
                    let tid = t.file_name().to_string_lossy().into_owned();
                    format!(
                        "{tid}:{}:{}:{}",
                        read(&format!("/proc/self/task/{tid}/comm")).trim(),
                        read(&format!("/proc/self/task/{tid}/wchan")).trim(),
                        read(&format!("/proc/self/task/{tid}/stat"))
                            .split(") ")
                            .nth(1)
                            .and_then(|s| s.split(' ').next())
                            .unwrap_or("")
                    )
                })
                .collect();
            names.sort();
            let _ = writeln!(out, "LOCKDIAG threads ({}): {}", names.len(), names.join(" "));
        }
        let _ = writeln!(
            out,
            "LOCKDIAG conflicting caller:\n{}",
            Backtrace::force_capture()
        );
        emit(&out);
        // Hold the conflicting caller's thread until the lock is free, so
        // the holder's drop reports itself before this caller fails.
        let released = loop {
            if let Ok(file) = std::fs::OpenOptions::new().read(true).open(&lock_path) {
                if rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockShared)
                    .is_ok()
                {
                    let _ = rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock);
                    break true;
                }
            }
            if started.elapsed() > std::time::Duration::from_secs(30) {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        emit(&format!(
            "LOCKDIAG conflict wait pid={pid} path={} released_while_caller_blocked={released} after={:?}\n",
            lock_path.display(),
            started.elapsed()
        ));
    }
}

#[cfg(any(unix, windows))]
mod real {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::path::Path;

    use super::FileLock;

    /// Name of the lock file inside the database directory.
    const LOCK_FILE: &str = "LOCK";

    /// Content stamped into the lock file.
    ///
    /// The lock itself is advisory and needs no bytes, but an unmarked
    /// zero-length `LOCK` is indistinguishable from any other tool's,
    /// and from a stray file in a directory someone pointed regolith at by
    /// mistake. The stamp is checked on the exclusive path, so regolith
    /// refuses to write a database into a directory that is not its own
    /// rather than taking it over.
    const LOCK_STAMP: &[u8; 8] = b"REGOLOCK";

    /// A held `flock` (unix) or exclusive share mode (Windows) on the
    /// database directory's `LOCK` file.
    #[derive(Debug)]
    pub(super) struct DbDirectoryLock {
        file: File,
        #[cfg(unix)]
        diag: (Option<(u64, u64)>, bool, String, std::backtrace::Backtrace),
    }

    impl FileLock for DbDirectoryLock {}

    impl Drop for DbDirectoryLock {
        fn drop(&mut self) {
            #[cfg(unix)]
            if let Some(id) = self.diag.0 {
                if let Some(since) = super::diag::watched(id) {
                    super::diag::emit(&format!(
                        "LOCKDIAG holder released id={id:?} exclusive={} {:?} after the conflict, dropped on thread {}\nLOCKDIAG holder drop site:\n{}\nLOCKDIAG holder was acquired on thread {} at:\n{}\n",
                        self.diag.1,
                        since.elapsed(),
                        super::diag::thread_label(),
                        std::backtrace::Backtrace::force_capture(),
                        self.diag.2,
                        self.diag.3
                    ));
                }
            }
            unlock(&self.file);
        }
    }

    pub(super) fn acquire(db_dir: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        let path = db_dir.join(LOCK_FILE);
        let file = if exclusive {
            std::fs::create_dir_all(db_dir)?;
            open_lock_file(&path).map_err(|e| lock_error(&path, e))?
        } else {
            OpenOptions::new()
                .read(true)
                .open(&path)
                .map_err(|e| lock_error(&path, e))?
        };

        #[cfg(unix)]
        let conflict = |e: io::Error| {
            if is_lock_conflict(&e) {
                super::diag::on_conflict(db_dir, exclusive);
            }
            lock_error(&path, e)
        };
        #[cfg(not(unix))]
        let conflict = |e: io::Error| lock_error(&path, e);
        if exclusive {
            lock_exclusive(&file).map_err(conflict)?;
            // Stamped only once the lock is held, so two processes
            // cannot race to write it, and only after any existing
            // content has been vetted.
            stamp_lock_file(&file).map_err(|e| lock_error(&path, e))?;
        } else {
            lock_shared(&file).map_err(conflict)?;
        }

        #[cfg(unix)]
        let diag = {
            use std::os::unix::fs::MetadataExt;
            (
                file.metadata().ok().map(|m| (m.dev(), m.ino())),
                exclusive,
                super::diag::thread_label(),
                std::backtrace::Backtrace::force_capture(),
            )
        };
        Ok(Box::new(DbDirectoryLock {
            file,
            #[cfg(unix)]
            diag,
        }))
    }

    /// Verify the stamp when the file already carries content, write it
    /// when it does not.
    ///
    /// An empty file is what a build older than the stamp left behind,
    /// so it is adopted rather than refused: the directory is regolith's, it
    /// simply predates the stamp. Content that is neither empty nor the
    /// stamp belongs to something else.
    fn stamp_lock_file(file: &File) -> io::Result<()> {
        use std::io::{Read, Seek, SeekFrom, Write};

        let mut existing = Vec::new();
        (&*file).seek(SeekFrom::Start(0))?;
        (&*file).read_to_end(&mut existing)?;

        if !existing.is_empty() && existing.as_slice() != LOCK_STAMP {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LOCK exists but was not written by regolith: refusing to open a database here",
            ));
        }
        if existing.is_empty() {
            (&*file).seek(SeekFrom::Start(0))?;
            (&*file).write_all(LOCK_STAMP)?;
            file.sync_data()?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn open_lock_file(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
    }

    #[cfg(windows)]
    fn open_lock_file(path: &Path) -> io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;

        // Deny read and write, but not delete. The share mode is the
        // whole exclusion here, because `lock_exclusive` is a no-op on
        // Windows, and a second opener asking for read or write still
        // gets a sharing violation. Denying delete as well would also
        // make removing the database directory fail while a handle is
        // open, which no other platform does and which `flock` on unix
        // never did either.
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;

        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_DELETE)
            .open(path)
    }

    fn lock_error(path: &Path, err: io::Error) -> io::Error {
        if is_lock_conflict(&err) {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "database directory is already locked for read-write access: {}",
                    path.display()
                ),
            )
        } else {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to lock database directory {}: {err}",
                    path.display()
                ),
            )
        }
    }

    fn is_lock_conflict(err: &io::Error) -> bool {
        err.kind() == io::ErrorKind::WouldBlock
            || err.kind() == io::ErrorKind::AlreadyExists
            || is_windows_share_violation(err)
    }

    #[cfg(windows)]
    fn is_windows_share_violation(err: &io::Error) -> bool {
        const ERROR_SHARING_VIOLATION: i32 = 32;
        err.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
    }

    #[cfg(not(windows))]
    fn is_windows_share_violation(_err: &io::Error) -> bool {
        false
    }

    #[cfg(unix)]
    fn lock_exclusive(file: &File) -> io::Result<()> {
        rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    }

    #[cfg(unix)]
    fn lock_shared(file: &File) -> io::Result<()> {
        rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockShared)
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    }

    #[cfg(unix)]
    fn unlock(file: &File) {
        let _ = rustix::fs::flock(file, rustix::fs::FlockOperation::Unlock);
    }

    #[cfg(windows)]
    fn lock_exclusive(_file: &File) -> io::Result<()> {
        Ok(())
    }

    #[cfg(windows)]
    fn lock_shared(_file: &File) -> io::Result<()> {
        Ok(())
    }

    #[cfg(windows)]
    fn unlock(_file: &File) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Box<dyn FileLock>` is not `Debug`, so `unwrap_err` cannot be
    /// used on an acquire result. Take the error kind directly.
    fn kind(result: io::Result<Box<dyn FileLock>>) -> Option<io::ErrorKind> {
        result.err().map(|e| e.kind())
    }

    #[test]
    fn a_second_exclusive_holder_is_rejected_on_every_target() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = acquire(dir.path(), true).unwrap();
        assert_eq!(
            kind(acquire(dir.path(), true)),
            Some(io::ErrorKind::AlreadyExists),
            "a second exclusive lock must fail"
        );
        drop(first);
        assert!(acquire(dir.path(), true).is_ok());
    }

    #[test]
    fn the_registry_excludes_a_writer_and_admits_concurrent_readers() {
        let registry: Arc<DirectoryRegistry> = Arc::default();
        let dir = Path::new("/registry/db");

        let writer = registry.acquire(dir, true).unwrap();
        assert_eq!(
            kind(registry.acquire(dir, true)),
            Some(io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            kind(registry.acquire(dir, false)),
            Some(io::ErrorKind::AlreadyExists)
        );
        drop(writer);

        let first_reader = registry.acquire(dir, false).unwrap();
        let second_reader = registry.acquire(dir, false).unwrap();
        assert_eq!(
            kind(registry.acquire(dir, true)),
            Some(io::ErrorKind::AlreadyExists),
            "a writer must not join live readers"
        );
        drop(first_reader);
        assert!(
            registry.acquire(dir, true).is_err(),
            "one reader still holds the path"
        );
        drop(second_reader);
        assert!(registry.acquire(dir, true).is_ok());
    }

    /// Opens racing on one directory: at no instant does a writer share it,
    /// and when every handle is dropped the directory is free again.
    #[test]
    fn racing_opens_of_one_directory_never_admit_a_writer_with_anyone() {
        use std::sync::atomic::{AtomicIsize, Ordering};

        let registry: Arc<DirectoryRegistry> = Arc::default();
        let dir = Path::new("/registry/raced");
        // Readers inside count up; a writer inside is -1.
        let inside = AtomicIsize::new(0);
        std::thread::scope(|scope| {
            for t in 0..6 {
                let (registry, inside) = (&registry, &inside);
                scope.spawn(move || {
                    // Fewer rounds under miri, which runs each a hundred times
                    // slower.
                    for round in 0..if cfg!(miri) { 40 } else { 2_000 } {
                        let exclusive = (t + round) % 3 == 0;
                        let Ok(lock) = registry.acquire(dir, exclusive) else {
                            continue;
                        };
                        if exclusive {
                            assert_eq!(inside.swap(-1, Ordering::SeqCst), 0, "a writer shared");
                            std::hint::spin_loop();
                            assert_eq!(inside.swap(0, Ordering::SeqCst), -1, "a writer shared");
                        } else {
                            assert!(
                                inside.fetch_add(1, Ordering::SeqCst) >= 0,
                                "beside a writer"
                            );
                            std::hint::spin_loop();
                            inside.fetch_sub(1, Ordering::SeqCst);
                        }
                        drop(lock);
                    }
                });
            }
        });
        assert!(
            registry.acquire(dir, true).is_ok(),
            "a holder was left behind"
        );
    }

    /// Opens of different directories do not disturb each other's entries.
    #[test]
    fn many_directories_are_tracked_independently() {
        let registry: Arc<DirectoryRegistry> = Arc::default();
        std::thread::scope(|scope| {
            for t in 0..4 {
                let registry = &registry;
                scope.spawn(move || {
                    for round in 0..if cfg!(miri) { 20 } else { 500 } {
                        let dir = PathBuf::from(format!("/registry/{t}/{round}"));
                        let writer = registry.acquire(&dir, true).unwrap();
                        assert!(registry.acquire(&dir, false).is_err());
                        drop(writer);
                        let reader = registry.acquire(&dir, false).unwrap();
                        assert!(registry.acquire(&dir, true).is_err());
                        drop(reader);
                    }
                });
            }
        });
        assert!(
            registry.held.is_empty(),
            "every entry is removed with its last holder"
        );
    }

    #[test]
    fn separate_registries_do_not_collide_on_one_path() {
        let one: Arc<DirectoryRegistry> = Arc::default();
        let two: Arc<DirectoryRegistry> = Arc::default();
        let dir = Path::new("/db");
        let _held = one.acquire(dir, true).unwrap();
        assert!(
            two.acquire(dir, true).is_ok(),
            "two MemEnvs are two filesystems and must not share exclusion"
        );
    }
}
