#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

//! An [`Env`] backed by the browser's Origin Private File System.
//!
//! `wasm32-unknown-unknown` has no `std::fs` at all: every filesystem call
//! there returns `Unsupported`, so regolith cannot open a database without a
//! storage backend written for the platform. OPFS is that backend.
//!
//! # Two strategies, chosen at mount
//!
//! `FileSystemSyncAccessHandle` is the only synchronous storage primitive
//! a browser offers, and it exists only inside a Worker. The same wasm
//! module can be instantiated on either thread, so the choice is a runtime
//! probe rather than a `cfg`:
//!
//! - [`OpfsMode::Sah`] pre-opens a pool of sync access handles and serves
//!   regolith's synchronous filesystem calls straight from storage. Nothing
//!   beyond regolith's own memtables and block cache is resident, and
//!   `sync_all` is real durability.
//! - [`OpfsMode::Mirror`] holds the whole database in linear memory and
//!   writes it back through `FileSystemWritableFileStream`, which is what
//!   the main thread offers. The database must fit in memory, and nothing is durable until
//!   [`OpfsEnv::persist`] resolves.
//!
//! [`OpfsOptions::force_mode`] pins the choice. Forcing [`OpfsMode::Sah`]
//! off a worker fails at [`OpfsEnv::mount`] with
//! [`OpfsError::SyncHandlesUnavailable`], which names the requirement; it
//! never hangs and never opens a database that cannot work.
//!
//! # Host contract
//!
//! ```ignore
//! use regolith::env::opfs::{OpfsEnv, OpfsOptions};
//! use regolith::{Db, Options};
//!
//! let env = OpfsEnv::mount("my-db", OpfsOptions::default()).await?;
//! let options = Options::embedded().env(env.as_env());
//! let db = Db::open(env.db_path(), options)?;
//!
//! db.put(b"k", b"v")?;
//! db.close()?;
//! // Mirror mode only: `close` is synchronous and cannot await, so the
//! // host is what makes the bytes durable.
//! env.persist().await?;
//! ```
//!
//! Open the database at [`OpfsEnv::db_path`]. regolith records the logical
//! paths it is given, so a database written under one path is not visible
//! under another. Compaction has to run on the calling thread here, which
//! is what [`crate::Options::embedded`] already sets; a browser wasm
//! module has one thread and [`Env::spawn`] reports `Unsupported`.
//!
//! # Testing
//!
//! `tests/wasm_opfs.rs`, `tests/wasm_opfs_main.rs`, and
//! `tests/wasm_opfs_memory.rs` drive this module from a real browser
//! through `wasm-bindgen-test`. There is no substitute: `navigator.storage`
//! only exists in a browser, and `createSyncAccessHandle` only inside a
//! Worker. Each of those files carries the command that runs it.

mod js;
mod mirror;
mod pool;
mod sah;
mod store;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kovan_map::HashMap;
use wasm_bindgen::JsValue;

use crate::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, LogFile, ReadDir, ReadFile, WriteFile,
    WriteMode,
};

use mirror::{Loaded, MirrorFs};
use pool::SahPool;
use store::{MirrorStore, OpfsStore, PoolStore};

/// Turn a thrown JS value into something a log line can carry.
fn describe(value: &JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(value, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| format!("{value:?}"))
}

/// Why an OPFS environment could not be mounted or persisted.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum OpfsError {
    /// The realm exposes no `navigator.storage`. OPFS needs a secure
    /// context: `https://`, or `http://localhost`.
    #[error("OPFS is unavailable in this realm: {0}")]
    Unavailable(String),
    /// `createSyncAccessHandle` was refused. Browsers restrict it to
    /// Worker scopes, so this is what the main thread looks like when
    /// [`OpfsMode::Sah`] was demanded.
    #[error(
        "OPFS sync access handles are unavailable here; they exist only inside a Web Worker: {0}"
    )]
    SyncHandlesUnavailable(String),
    /// [`OpfsMode::Mirror`] holds the whole database in linear memory and
    /// the database on disk is already larger than the configured bound.
    #[error(
        "the stored database is {resident} bytes, over the {limit} byte mirror-mode limit; \
         raise OpfsOptions::max_resident_bytes or open it in a worker, where OpfsMode::Sah \
         streams to storage instead"
    )]
    ResidencyExceeded {
        /// Bytes the stored database occupies.
        resident: usize,
        /// The configured [`OpfsOptions::max_resident_bytes`].
        limit: usize,
    },
    /// An OPFS call rejected or threw.
    #[error("OPFS call failed: {0}")]
    Js(String),
}

impl From<OpfsError> for io::Error {
    fn from(error: OpfsError) -> Self {
        match error {
            OpfsError::Unavailable(_) | OpfsError::SyncHandlesUnavailable(_) => {
                io::Error::new(io::ErrorKind::Unsupported, error.to_string())
            }
            _ => io::Error::other(error.to_string()),
        }
    }
}

impl From<OpfsError> for crate::Error {
    fn from(error: OpfsError) -> Self {
        crate::Error::Io(error.into())
    }
}

/// Which OPFS access strategy an [`OpfsEnv`] is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpfsMode {
    /// Synchronous access handles, from a dedicated Web Worker.
    Sah,
    /// The database mirrored in linear memory, written back whole-file.
    Mirror,
}

/// Tuning for [`OpfsEnv::mount`].
///
/// Plain fields, like [`crate::Options`]: build one with
/// `OpfsOptions { initial_slots: 16, ..Default::default() }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpfsOptions {
    /// Handle-pool slots pre-opened at mount in [`OpfsMode::Sah`]. Each
    /// slot is one OPFS file that regolith can assign to a new WAL or SSTable
    /// without an `await`. Running out is reported, not fatal: see
    /// [`OpfsEnv::grow_pool`].
    pub initial_slots: usize,
    /// Refuse to hold more than this in [`OpfsMode::Mirror`], where the
    /// whole database is resident. Wasm linear memory is never returned
    /// to the host, so an unbounded mirror leaks the tab permanently.
    pub max_resident_bytes: usize,
    /// Pin the strategy instead of probing for one.
    pub force_mode: Option<OpfsMode>,
}

impl Default for OpfsOptions {
    fn default() -> Self {
        Self {
            initial_slots: 64,
            max_resident_bytes: 32 * 1024 * 1024,
            force_mode: None,
        }
    }
}

enum Backend {
    Sah(PoolStore),
    Mirror(MirrorStore),
}

struct Inner {
    db_path: PathBuf,
    mode: OpfsMode,
    backend: Backend,
    /// Exclusion between database handles on this mount. Scoped to the
    /// mount, not the process, because two mounts are two independent
    /// OPFS directories.
    open_dirs: Arc<super::db_lock::DirectoryRegistry>,
}

/// An [`Env`] backed by the Origin Private File System.
///
/// Clone it freely: every clone shares one mounted database. Install one
/// on [`crate::Options::env`] with [`OpfsEnv::as_env`] and keep another
/// for [`OpfsEnv::persist`].
#[derive(Clone)]
pub struct OpfsEnv {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for OpfsEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpfsEnv")
            .field("db_path", &self.inner.db_path)
            .field("mode", &self.inner.mode)
            .field("backend", &self.store())
            .finish()
    }
}

impl OpfsEnv {
    /// Mount an OPFS-backed environment on the OPFS directory `db_name`.
    ///
    /// Probes for synchronous access handles unless
    /// [`OpfsOptions::force_mode`] pins the strategy: browsers refuse
    /// `createSyncAccessHandle` outside a Worker, and that refusal
    /// selects [`OpfsMode::Mirror`]. Call this before [`crate::Db::open`]
    /// and install the result on [`crate::Options::env`].
    pub async fn mount(db_name: &str, options: OpfsOptions) -> Result<Self, OpfsError> {
        let root = js::root_directory()
            .await
            .map_err(|e| OpfsError::Unavailable(describe(&e)))?;
        let directory = js::directory_handle(&root, db_name, true)
            .await
            .map_err(|e| OpfsError::Js(describe(&e)))?;

        let db_path = PathBuf::from(db_name);

        if options.force_mode != Some(OpfsMode::Mirror) {
            // Never open fewer slots than the directory already holds:
            // the files above the cut would be unreachable, and growing
            // back into them would overwrite live data.
            let existing = sah::existing_slot_count(&directory)
                .await
                .map_err(|e| OpfsError::Js(describe(&e)))?;
            let slots = options.initial_slots.max(1).max(existing);
            match sah::open_slots(&directory, 0..slots).await {
                Ok(opened) => {
                    let (handles, headers): (Vec<_>, Vec<_>) = opened.into_iter().unzip();
                    let mount = sah::register_mount(directory, handles);
                    let pool = Arc::new(SahPool::new(mount, headers));
                    return Ok(Self {
                        inner: Arc::new(Inner {
                            db_path,
                            mode: OpfsMode::Sah,
                            backend: Backend::Sah(PoolStore(pool)),
                            open_dirs: Arc::default(),
                        }),
                    });
                }
                Err(e) if options.force_mode == Some(OpfsMode::Sah) => {
                    return Err(OpfsError::SyncHandlesUnavailable(describe(&e)));
                }
                Err(e) => {
                    tracing::warn!(
                        reason = %describe(&e),
                        "OPFS sync access handles unavailable; falling back to mirror mode, \
                         which is durable only across OpfsEnv::persist"
                    );
                }
            }
        }

        let loaded = match MirrorFs::load(&directory, options.max_resident_bytes)
            .await
            .map_err(|e| OpfsError::Js(describe(&e)))?
        {
            Loaded::Files(files) => files,
            Loaded::OverLimit(resident) => {
                return Err(OpfsError::ResidencyExceeded {
                    resident,
                    limit: options.max_resident_bytes,
                });
            }
        };
        let mount = sah::register_mount(directory, Vec::new());
        let fs = Arc::new(MirrorFs::new(mount, loaded, options.max_resident_bytes));
        Ok(Self {
            inner: Arc::new(Inner {
                db_path,
                mode: OpfsMode::Mirror,
                backend: Backend::Mirror(MirrorStore(fs)),
                open_dirs: Arc::default(),
            }),
        })
    }

    /// Which strategy is in force.
    pub fn mode(&self) -> OpfsMode {
        self.inner.mode
    }

    /// The path to hand [`crate::Db::open`].
    pub fn db_path(&self) -> &Path {
        &self.inner.db_path
    }

    /// This environment as an [`Env`] handle for [`crate::Options::env`].
    pub fn as_env(&self) -> Arc<dyn Env> {
        Arc::new(self.clone())
    }

    /// Write every dirty file back to OPFS.
    ///
    /// A no-op in [`OpfsMode::Sah`], where writes already reached storage.
    /// In [`OpfsMode::Mirror`] this is the only thing that makes data
    /// durable: [`crate::Db::close`] is synchronous and cannot await, so
    /// call this on commit, on `visibilitychange`, and before unload.
    ///
    /// Mirror calls serialize before capturing their batch. Writes made while a
    /// batch is awaiting browser I/O remain dirty for the next call. A failed
    /// call keeps its work retryable, including failed file removals. Dropping
    /// the caller's future does not cancel a batch already handed to the browser;
    /// that batch finishes before the next one starts.
    pub async fn persist(&self) -> Result<(), OpfsError> {
        match &self.inner.backend {
            Backend::Sah(_) => Ok(()),
            Backend::Mirror(store) => store
                .0
                .persist()
                .await
                .map_err(|e| OpfsError::Js(describe(&e))),
        }
    }

    /// Bytes waiting for [`OpfsEnv::persist`]. Always `0` in
    /// [`OpfsMode::Sah`].
    pub fn pending_bytes(&self) -> usize {
        match &self.inner.backend {
            Backend::Sah(_) => 0,
            Backend::Mirror(store) => store.0.pending_bytes(),
        }
    }

    /// Bytes the database occupies in linear memory. Always `0` in
    /// [`OpfsMode::Sah`], where files live in storage rather than memory.
    pub fn resident_bytes(&self) -> usize {
        match &self.inner.backend {
            Backend::Sah(_) => 0,
            Backend::Mirror(store) => store.0.resident_bytes(),
        }
    }

    /// Pool slots with no logical file assigned. Always `0` in
    /// [`OpfsMode::Mirror`].
    pub fn free_slots(&self) -> usize {
        match &self.inner.backend {
            Backend::Sah(store) => store.0.free_slots(),
            Backend::Mirror(_) => 0,
        }
    }

    /// Add `additional` slots to the handle pool.
    ///
    /// A no-op in [`OpfsMode::Mirror`]. Call it when [`OpfsEnv::free_slots`]
    /// runs low, or after a write failed naming the exhausted pool:
    /// opening a slot needs an `await`, which the engine's synchronous
    /// write path cannot do for itself.
    pub async fn grow_pool(&self, additional: usize) -> Result<(), OpfsError> {
        let Backend::Sah(store) = &self.inner.backend else {
            return Ok(());
        };
        let mount = store.0.mount_id();
        let directory = sah::mount_directory(mount).map_err(|e| OpfsError::Js(e.to_string()))?;
        let first = store.0.slot_count();

        // Growth reads each new slot's header exactly as mount does, so a
        // physical file that already carries a logical path is adopted
        // rather than blanked.
        let opened = sah::open_slots(&directory, first..first + additional)
            .await
            .map_err(|e| OpfsError::Js(describe(&e)))?;
        let (handles, headers): (Vec<_>, Vec<_>) = opened.into_iter().unzip();

        sah::extend_mount(mount, handles).map_err(|e| OpfsError::Js(e.to_string()))?;
        store.0.adopt_slots(headers);
        Ok(())
    }

    fn store(&self) -> &dyn OpfsStore {
        match &self.inner.backend {
            Backend::Sah(store) => store,
            Backend::Mirror(store) => store,
        }
    }
}

impl Env for OpfsEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.store().create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.store().read_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.store().open_read(path)
    }

    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.store().open_write(path, mode)
    }

    /// Written at an offset of the file's sync access handle, or of its
    /// mirror in memory: no cursor and no lock.
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        self.store().open_log(path, mode)
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.store().metadata(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.store().remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.store().rename(from, to)
    }

    fn exists(&self, path: &Path) -> bool {
        self.store().exists(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.store().sync_dir(path)
    }

    fn hard_link(&self, _src: &Path, _dst: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "OPFS has no hard links; Capabilities::hard_link is false, so regolith copies instead",
        ))
    }

    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        // A wasm module is one process with one linear memory holding one
        // copy of regolith's state, so an in-process registry excludes every
        // writer that can exist. No LOCK file is created: an unclean
        // unload would leave a stale one behind and turn a crash into an
        // unopenable database, while registry state dies with the module.
        self.inner.open_dirs.acquire(path, exclusive)
    }

    fn capabilities(&self) -> Capabilities {
        self.store().capabilities()
    }

    fn now_micros(&self) -> Option<u64> {
        js::monotonic_ms().map(|ms| (ms.max(0.0) * 1000.0) as u64)
    }

    fn unix_secs(&self) -> Option<u64> {
        let ms = js::wall_clock_ms();
        if ms.is_finite() && ms >= 0.0 {
            Some((ms / 1000.0) as u64)
        } else {
            None
        }
    }

    fn spawn(
        &self,
        _name: &str,
        _body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this wasm target has no threads; set Options::max_background_compactions = 0 \
             to run compaction on the calling thread",
        ))
    }
}

/// A file's ancestors, for the virtual directory set (a lock-free map used
/// as a set). Stops at the first ancestor already present: the chain is
/// always inserted from the leaf up, so a present directory implies its
/// parents are, or are being, inserted too.
pub(super) fn register_ancestors(dirs: &HashMap<PathBuf, ()>, path: &Path) {
    let mut cursor = path.parent();
    while let Some(dir) = cursor {
        if dir.as_os_str().is_empty() || dirs.insert_if_absent(dir.to_path_buf(), ()).is_some() {
            break;
        }
        cursor = dir.parent();
    }
}

/// The error every OPFS backend reports for a path it does not hold.
pub(super) fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("no such file or directory: {}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

    // The unit tests of this module and its children are pure logic, but
    // they only exist on a target whose test harness is a browser. One
    // configuration line for the whole `regolith` lib test target.
    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn ancestors_register_the_whole_chain() {
        let dirs = HashMap::with_capacity(64);
        register_ancestors(&dirs, Path::new("db/sst/000001.sst"));
        assert!(dirs.contains_key(Path::new("db")));
        assert!(dirs.contains_key(Path::new("db/sst")));
        assert!(!dirs.contains_key(Path::new("db/sst/000001.sst")));
    }

    #[wasm_bindgen_test]
    fn default_options_bound_residency() {
        let options = OpfsOptions::default();
        assert_eq!(options.initial_slots, 64);
        assert_eq!(options.max_resident_bytes, 32 * 1024 * 1024);
        assert_eq!(options.force_mode, None);
    }
}
