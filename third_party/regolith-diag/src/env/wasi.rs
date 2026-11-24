//! The [`Env`] for WASI: `wasm32-wasip1` and `wasm32-wasip2`.
//!
//! WASI is not a filesystem-less target. Given a host preopen
//! (`wasmtime run --dir=HOSTDIR::/data ...`) `std::fs` works, and it
//! was measured working here rather than assumed: create, open, read,
//! write, append after reopen, `set_len`, seek from the end, rename
//! over an existing entry, hard link, and `read_dir` all behave as
//! they do on Linux, and so does `File::sync_all`. regolith therefore runs
//! on real files on this target, not on an in-memory mirror.
//!
//! So `WasiEnv` delegates every filesystem call to [`StdEnv`] rather
//! than restating it, and states only what genuinely differs:
//!
//! - **No threads.** `std::thread::spawn` reports `Unsupported`
//!   (`os error 58`). [`WasiEnv::spawn`] returns that error with a
//!   message saying so, and [`Capabilities::threads`] is `false`.
//!   `Options::default()` already sets
//!   [`crate::Options::max_background_compactions`] to `0` on this
//!   target, so compaction runs on the calling thread and an open
//!   with stock options succeeds; a caller who raises that field
//!   anyway gets the explanation instead of a bare `Unsupported`
//!   raised several frames inside the compaction scheduler. That
//!   explanation is the main reason this type exists.
//! - **No directory fsync.** Opening a preopened directory and calling
//!   `sync_all` on it reports `EBADF`. [`Capabilities::sync_dir`] is
//!   `false` and regolith reports the narrower crash guarantee through
//!   [`crate::Db::capabilities`] instead of claiming one it does not
//!   provide. [`StdEnv`] already gets this right, because it derives
//!   the flag and the behavior from one expression rather than from
//!   `cfg!(unix)`, which is `false` here for unrelated reasons.
//! - **No file locking.** WASI has no `flock`, so the lock guard
//!   excludes nothing and creates no file. A create-exclusive `LOCK`
//!   file would be worse than nothing: a module that traps leaves it
//!   behind and every later open then fails permanently, blaming a
//!   second process that cannot exist on a host with one.
//! - **No page-cache hint.** There is no `posix_fadvise`, so
//!   [`WasiEnv::drop_page_cache`] does nothing at all rather than
//!   opening each compacted file to call a hint that is already a
//!   no-op on this target.
//!
//! # Positional reads
//!
//! `fd_pread` sits behind the unstable `wasi_ext` feature, so
//! [`StdEnv`]'s fallback seeks and reads under a lock. That lock is
//! uncontended by construction: this target has one thread.

use std::io;
use std::path::Path;

use super::std_env::StdEnv;
use super::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, WriteFile, WriteMode,
};

/// The WASI host: `std::fs` over a preopened directory, with no
/// threads, no directory fsync, no file locking, and no page-cache
/// hint.
///
/// Installed automatically by [`crate::env::std_env`] when the target
/// is WASI, so a caller that never mentions an [`Env`] still gets the
/// right one.
#[derive(Debug, Default, Clone, Copy)]
pub struct WasiEnv {
    fs: StdEnv,
}

impl WasiEnv {
    /// Construct the WASI environment. It holds no state.
    pub const fn new() -> Self {
        Self { fs: StdEnv::new() }
    }
}

impl Env for WasiEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.fs.create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.fs.read_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.fs.open_read(path)
    }

    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.fs.open_write(path, mode)
    }

    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn super::LogFile>> {
        self.fs.open_log(path, mode)
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.fs.metadata(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.fs.remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.fs.rename(from, to)
    }

    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.fs.hard_link(src, dst)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.fs.sync_dir(path)
    }

    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        self.fs.lock_file(path, exclusive)
    }

    /// [`StdEnv`] already reports the right thing on WASI, because it
    /// derives every flag from an expression that names the platform
    /// rather than from `cfg!(unix)`: hard links and rename-over work,
    /// `sync_all` on a file is durable, and `sync_dir`, `file_lock`,
    /// and `threads` are all `false`.
    fn capabilities(&self) -> Capabilities {
        self.fs.capabilities()
    }

    fn now_micros(&self) -> Option<u64> {
        self.fs.now_micros()
    }

    fn unix_secs(&self) -> Option<u64> {
        self.fs.unix_secs()
    }

    /// Always fails, saying why rather than returning a bare
    /// `Unsupported` that the caller has to interpret.
    ///
    /// The remedy is deliberately not repeated here: the compaction
    /// scheduler appends "set `max_background_compactions` to 0" to
    /// whatever an [`Env`] reports, because that advice is the same on
    /// every host without threads. This message carries only the part
    /// that is specific to WASI.
    fn spawn(
        &self,
        name: &str,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        drop(body);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("cannot start thread {name}: WASI has no threads"),
        ))
    }

    /// Nothing to do: WASI has no `posix_fadvise`.
    fn drop_page_cache(&self, path: &Path) {
        let _ = path;
    }
}
