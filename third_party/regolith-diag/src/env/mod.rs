//! The host platform, behind one trait.
//!
//! The engine performs no `std::fs`, `std::thread`, or `std::time`
//! call of its own. Everything that touches the host goes through the
//! [`Env`] installed on [`crate::Options::env`], which defaults to
//! [`StdEnv`]: the same `std::fs` + `std::thread` + `std::time` calls
//! regolith made before this trait existed, in the same order, with the
//! same error kinds.
//!
//! # Why a trait object
//!
//! `Options` already carries `Arc<dyn CompactionFilter>`,
//! `Arc<dyn PrefixExtractor>`, `Arc<dyn MergeOperator>` and friends,
//! so `Arc<dyn Env>` extends the pattern that is already here rather
//! than introducing a generic parameter that would infect `Db`,
//! `Snapshot`, `Iter`, and every signature that mentions them. The
//! indirect call costs a few nanoseconds against a syscall that costs
//! hundreds, and the cached read path never reaches an `Env` at all:
//! a block-cache hit returns before any file is touched.
//!
//! # What a backend must decide
//!
//! Not every host has hard links, directory fsync, cross-process file
//! locking, or threads. [`Capabilities`] is how a backend says so and
//! how regolith reports it back through [`crate::Db::capabilities`],
//! rather than claiming a guarantee it is not providing.
//!
//! # Internal naming convention
//!
//! Engine constructors that touch the host take the `Env` as their
//! first argument and carry an `_in` suffix (`Wal::create_in`,
//! `SsTableReader::open_in`). The unsuffixed names remain as
//! `#[cfg(test)]` shims over [`std_env`] so the module-local unit
//! tests keep exercising the same code through the standard
//! environment.

mod buffered;
pub(crate) mod db_lock;
#[cfg(any(target_vendor = "apple", test))]
mod deferred_error;
mod mem_env;
pub(crate) mod mem_file;
mod mem_log;
pub(crate) mod open_file_limit;
mod types;
// The browser's Origin Private File System. Only compiled for
// `wasm32-unknown-unknown`, the one target with no `std::fs` at all;
// it is also the only target that has `js-sys` as a dependency.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod opfs;
// Pure, so it is unit-tested on the host as well as built for OPFS.
#[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
mod persist_order;
mod serial_log;
mod std_env;
#[cfg(test)]
pub(crate) mod walk_fault;
// WASI has `std::fs` but no threads. `WasiEnv` delegates every
// filesystem call to `StdEnv` and says so where it matters: a spawn
// that explains itself instead of a bare `Unsupported`, and no
// pointless open per compacted file for a page-cache hint the
// platform does not have. See `src/env/wasi.rs`.
#[cfg(target_os = "wasi")]
mod wasi;

pub use mem_env::MemEnv;
pub use std_env::StdEnv;
pub use types::{Capabilities, DirEntry, DiskSpace, FileMeta, WriteMode};
#[cfg(target_os = "wasi")]
pub use wasi::WasiEnv;

pub(crate) use buffered::BufferedWriter;
pub(crate) use open_file_limit::{OpenFileLimit, is_removed_table};

use std::io;
use std::path::Path;
use std::sync::Arc;

/// A walk over the entries directly under one directory, from
/// [`Env::read_dir`]: each item is one entry, or the error that stopped the
/// walk. It may borrow the environment, so it lives no longer than that.
pub type ReadDir<'a> = Box<dyn Iterator<Item = io::Result<DirEntry>> + 'a>;

/// The entries directly under `dir` in a flat namespace that keeps its
/// files and its directories as two sets of whole paths, files first.
///
/// Lazy, so a walk holds what the two set iterators hold (one bucket of a
/// kovan map each), never the listing. Shared by [`MemEnv`] and the OPFS
/// stores, which keep their namespace that way.
pub(crate) fn flat_children<'a>(
    dir: &Path,
    files: impl Iterator<Item = std::path::PathBuf> + 'a,
    dirs: impl Iterator<Item = std::path::PathBuf> + 'a,
) -> ReadDir<'a> {
    let dir = dir.to_path_buf();
    let under = move |path: &std::path::PathBuf| path.parent() == Some(dir.as_path());
    let files = files.filter(under.clone()).map(|path| DirEntry {
        path,
        is_dir: false,
    });
    let dirs = dirs
        .filter(under)
        .map(|path| DirEntry { path, is_dir: true });
    Box::new(files.chain(dirs).map(Ok))
}

/// Whether the target has more than one thread at all. A wasm module built
/// without the `atomics` target feature has exactly one, and
/// `std::thread::spawn` there reports [`std::io::ErrorKind::Unsupported`]:
/// nothing another thread would do can happen while the one thread waits.
/// [`Capabilities::threads`] says whether an [`Env`] spawns threads, which
/// is a different question: an in-memory env spawns none on a host that has
/// many.
pub(crate) const PLATFORM_THREADS: bool =
    !cfg!(all(target_family = "wasm", not(target_feature = "atomics")));

/// Filesystem, clock, and scheduling for one database.
///
/// An implementation is shared across threads (a compaction worker
/// holds the same `Arc` a foreground writer does), so `Send + Sync`
/// are required on every target. `Debug` is required so a bug report
/// can name which environment was installed.
///
/// Paths are whatever the host understands. regolith only ever joins
/// names onto the database directory it was given, so a backend with
/// a flat namespace is free to treat the whole path as an opaque key.
///
/// One default is worth replacing in an environment of your own:
/// [`Env::open_log`]. It is correct on any environment, and gets there by
/// putting every write of the write-ahead log behind one lock; the method
/// says what that costs.
pub trait Env: Send + Sync + std::fmt::Debug {
    /// Create `path` and every missing parent. Succeeds when the
    /// directory already exists.
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;

    /// Walk the entries directly under `path`, in unspecified order.
    ///
    /// The walk streams: an implementation holds a bounded batch of
    /// entries at a time, whatever the directory holds (`readdir`'s
    /// buffer, one bucket of an in-memory map), never the whole listing.
    /// Every caller in regolith acts on each entry as it comes or keeps a
    /// bounded subset of them, so no walk costs memory in proportion to
    /// the directory. An implementation that collected the directory
    /// before yielding would break that bound for every caller.
    ///
    /// An entry present for the whole walk is yielded exactly once. One
    /// created, removed or renamed while the walk runs may or may not be
    /// yielded, as POSIX `readdir` has it, so a caller may remove an entry
    /// it was handed and keep walking.
    ///
    /// Opening a walk of a directory that does not exist fails with
    /// [`std::io::ErrorKind::NotFound`]. An error part way through is
    /// yielded as an `Err` item.
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>>;

    /// Open `path` for positional reading.
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>>;

    /// Open `path` for writing, creating it if it does not exist.
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>>;

    /// Open `path` as a write-ahead log written at explicit offsets,
    /// creating it if it does not exist.
    ///
    /// [`WriteMode::Truncate`] starts an empty log. Any other mode keeps
    /// what the file holds, so writes continue above it.
    ///
    /// The default is for an environment outside this crate that has not
    /// said how it writes at an offset. It opens the file with
    /// [`Env::open_write`] and puts the writes in offset order behind a
    /// lock, holding a write that arrives ahead of a gap until the gap is
    /// filled. That is always correct, and it is the one place a committing
    /// thread waits for another to write.
    ///
    /// What the default costs: one lock covers the file.
    /// The log writes of concurrent commits run one at a time, and a sync of
    /// the log holds the lock for as long as the device takes, so no commit
    /// writes its record while a sync is in flight. A write that arrives
    /// ahead of a gap is copied and kept in memory until the gap is filled,
    /// which is bounded by the commits in flight. A thread that finds the
    /// lock taken parks until it is let go, inside
    /// [`IoQueue::poll`](crate::IoQueue::poll) or
    /// [`commit_nowait`](crate::Transaction::commit_nowait) as anywhere
    /// else, so those calls lose their promise never to wait on another
    /// thread. An environment that takes commits from more than one thread,
    /// or whose callers must never block, should override this method; one
    /// that a single thread drives loses nothing by keeping the default.
    ///
    /// No environment of this crate uses the default. Every one a database
    /// runs on has a log no write of which waits for another: [`StdEnv`]
    /// and the WASI environment write at an offset on the host, [`MemEnv`]
    /// appends without a lock, and the OPFS environment writes at an
    /// offset of its handle or its mirror. An environment that wraps
    /// another forwards to the one it wraps, and wraps the log it is handed
    /// when it counts, fails or records writes. An environment of your own
    /// that can write at an offset should override this too.
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        Ok(Box::new(serial_log::SerialLog::open(self, path, mode)?))
    }

    /// Size and kind of the entry at `path`.
    fn metadata(&self, path: &Path) -> io::Result<FileMeta>;

    /// Remove the file at `path`.
    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// Rename `from` to `to`, replacing `to` if it exists.
    ///
    /// regolith uses this for its write-new-then-rename update of the
    /// MANIFEST and of backup metadata, so a backend whose rename is
    /// not crash-atomic must report that through
    /// [`Capabilities::atomic_rename`].
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Create a hard link at `dst` pointing at `src`.
    ///
    /// Only reached when [`Capabilities::hard_link`] is `true`.
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()>;

    /// Make a directory's entries durable, so a file created,
    /// renamed, or removed inside it survives a crash once its
    /// contents are synced.
    ///
    /// An environment whose [`Capabilities::sync_dir`] is `false`
    /// returns `Ok(())` without doing anything. That flag, not this
    /// return value, is what tells the truth about the guarantee.
    fn sync_dir(&self, path: &Path) -> io::Result<()>;

    /// Exclude other processes from the database directory `path`.
    ///
    /// The lock is released when the returned guard is dropped. An
    /// environment whose [`Capabilities::file_lock`] is `false`
    /// returns a guard that excludes nothing and creates no file.
    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>>;

    /// What this environment can actually do.
    fn capabilities(&self) -> Capabilities;

    /// Monotonic microseconds from an arbitrary origin, for measuring
    /// durations.
    ///
    /// `None` on a platform with no monotonic clock. regolith then records
    /// no timing at all rather than recording a zero that reads like a
    /// measurement.
    fn now_micros(&self) -> Option<u64>;

    /// Seconds since the Unix epoch.
    ///
    /// `None` on a platform with no wall clock. regolith then reports the
    /// affected timestamps as absent rather than as the epoch.
    fn unix_secs(&self) -> Option<u64>;

    /// Run `body` on another thread.
    ///
    /// A single-threaded host reports
    /// [`std::io::ErrorKind::Unsupported`] here, and regolith's caller
    /// turns that into an open error rather than a panic.
    fn spawn(
        &self,
        name: &str,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>>;

    /// Free and total space on the filesystem holding `path`.
    ///
    /// `Ok(None)` where the environment has no filesystem to ask about,
    /// which is the default. regolith uses it to warn at open when the
    /// disk is close to full and to report `regolith.disk-available-*`
    /// properties; it never gates an operation on the answer.
    fn disk_space(&self, path: &Path) -> io::Result<Option<DiskSpace>> {
        let _ = path;
        Ok(None)
    }

    /// Whether anything exists at `path`.
    fn exists(&self, path: &Path) -> bool {
        self.metadata(path).is_ok()
    }

    /// Whether `path` is a directory. `false` when it does not exist.
    fn is_dir(&self, path: &Path) -> bool {
        self.metadata(path).map(|m| m.is_dir).unwrap_or(false)
    }

    /// Read a whole file into memory.
    ///
    /// The length comes from the host, so it is bounded with
    /// `try_reserve_exact`: a file larger than the available heap
    /// reports [`std::io::ErrorKind::OutOfMemory`] instead of
    /// aborting the process.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        read_whole(&*self.open_read(path)?, path)
    }

    /// Replace the whole contents of `path`, creating it if needed.
    ///
    /// Buffered like `std::fs::write`: the bytes have reached the
    /// host when this returns, but durability still needs a
    /// [`WriteFile::sync_all`] on a handle the caller keeps.
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut file = self.open_write(path, WriteMode::Truncate)?;
        file.write_all(data)?;
        file.flush()
    }

    /// Hint that the host may evict any cache backing `path`.
    ///
    /// Best effort by definition: regolith is correct whether or not the
    /// hint is honored, so failures are swallowed. The default does
    /// nothing.
    fn drop_page_cache(&self, path: &Path) {
        let _ = path;
    }
}

/// Every byte of `file`, which was opened at `path`: the body of
/// [`Env::read`], for an environment that reads through a handle of its own.
pub(crate) fn read_whole(file: &dyn ReadFile, path: &Path) -> io::Result<Vec<u8>> {
    let len = usize::try_from(file.len()?).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is too large to address", path.display()),
        )
    })?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!("cannot allocate {len} bytes to read {}", path.display()),
        )
    })?;
    buf.resize(len, 0);
    file.read_exact_at(0, &mut buf)?;
    Ok(buf)
}

/// A file opened for reading.
///
/// Positional only. regolith never relies on a shared cursor, so an
/// implementation needs no seek state and no lock around one - which
/// is why an `SsTableReader` can serve concurrent readers without
/// serializing them.
pub trait ReadFile: Send + Sync {
    /// Fill `buf` with the bytes at `offset`, or fail.
    ///
    /// Reading past the end reports
    /// [`std::io::ErrorKind::UnexpectedEof`].
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// Length of the file in bytes.
    fn len(&self) -> io::Result<u64>;

    /// Whether the file holds no bytes.
    fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// A file opened for writing. Append-only apart from
/// [`WriteFile::set_len`], which only manifest recovery uses.
pub trait WriteFile: Send {
    /// Append every byte of `buf`.
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;

    /// Append every byte of every slice, in order, as one operation where
    /// the host supports it.
    ///
    /// The default implementation writes the slices one at a time, which is
    /// always correct. An environment backed by a POSIX file overrides this
    /// with `writev` so a record assembled from a header and a payload costs
    /// one syscall rather than one per piece. That matters on the WAL path,
    /// which appends a large record that way when a log is rewritten.
    ///
    /// This is a throughput and syscall-count optimization, NOT an atomicity
    /// guarantee: a vectored write can still be torn by a crash, exactly as a
    /// sequential one can. Recovery must still tolerate a torn tail.
    fn write_all_vectored(&mut self, slices: &[&[u8]]) -> io::Result<()> {
        for slice in slices {
            self.write_all(slice)?;
        }
        Ok(())
    }

    /// Push buffered bytes to the host. Does not imply durability.
    fn flush(&mut self) -> io::Result<()>;

    /// Make every byte written so far durable.
    ///
    /// An environment whose [`Capabilities::durable_sync`] is `false`
    /// may return `Ok(())` without providing durability.
    fn sync_all(&mut self) -> io::Result<()>;

    /// Make the file's *data* durable, without necessarily flushing
    /// metadata that no reader depends on.
    ///
    /// This is `fdatasync` rather than `fsync`. A log that is appended
    /// to and fsynced on every commit otherwise pays an inode update per
    /// commit for a size field nothing reads back: recovery finds the
    /// end of the log from the records themselves, not from the length.
    /// On an NVMe device that is a second round trip to the drive on the
    /// critical path of every durable write.
    ///
    /// Defaults to [`WriteFile::sync_all`], which is always correct and
    /// merely slower, so an environment that cannot separate the two
    /// needs no implementation.
    fn sync_data(&mut self) -> io::Result<()> {
        self.sync_all()
    }

    /// Truncate or extend the file to exactly `len` bytes, leaving the
    /// write position at `len`.
    ///
    /// The handle must have been opened [`WriteMode::Truncate`] or
    /// [`WriteMode::Update`]. Shortening through a
    /// [`WriteMode::Append`] handle is not supported: Windows opens an
    /// append handle without `FILE_WRITE_DATA`, which the truncate
    /// needs, so it fails there with "Access is denied" while
    /// succeeding on unix.
    ///
    /// The position matters: WAL rollback truncates a partly written
    /// group and then appends again, so a writer that left its cursor
    /// past the new end would write into a hole. An implementation whose
    /// writes always append satisfies this for free; one that carries an
    /// explicit offset has to move it.
    fn set_len(&mut self, len: u64) -> io::Result<()>;

    /// Length of the file in bytes, counting only what has reached
    /// the host.
    fn len(&self) -> io::Result<u64>;

    /// Whether the file holds no bytes.
    fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// A write-ahead log file, written at explicit offsets by any number of
/// threads at once. Opened by [`Env::open_log`].
///
/// regolith's commit pipeline gives every commit its byte range in the log
/// before anything is written, and then lets whichever thread gets there
/// first write that range. So unlike a [`WriteFile`], this handle has no
/// cursor and every method takes `&self`.
///
/// # Contract
///
/// - **Idempotent writes.** Two calls that name the same offset always carry
///   the same bytes, and may run at the same time. An implementation may
///   perform both or only one; the range must end up holding those bytes.
/// - **Out of order.** A write may land above a range nothing has written
///   yet. That gap is filled by a later write before any
///   [`sync_data`](LogFile::sync_data) is expected to cover the bytes above
///   it: a sync makes durable every byte below the lowest offset never
///   written, the contiguous written prefix, and may cover more.
/// - **No write returns before its bytes are held.** Once
///   [`write_all_at`](LogFile::write_all_at) returns `Ok`, a sync that covers
///   the range makes it durable, and a reader opened on the path sees the
///   bytes once every lower offset is written too.
/// - **Truncation.** [`set_len`](LogFile::set_len) is called only to cut the
///   file back to an offset where a write began, and only while no write
///   that reaches that offset or beyond is in flight. A write that lies
///   wholly below the new length may run at the same time, and its range
///   holds its bytes afterwards as if the cut had not happened.
///
/// # Misuse
///
/// Two writes of different bytes to one offset, or a `set_len` racing a
/// write that reaches the new length, leave the range holding either write
/// or a mixture; replay then reads the range as a torn tail.
pub trait LogFile: Send + Sync {
    /// Write every byte of `buf` at `offset`, extending the file as needed.
    fn write_all_at(&self, offset: u64, buf: &[u8]) -> io::Result<()>;

    /// Write every byte of `bufs` at `offset`, one buffer right after the
    /// other, extending the file as needed: several whole records the log
    /// writes with one call. The contract is [`write_all_at`]'s for the
    /// concatenated bytes; an error may leave any part of them written.
    ///
    /// The default writes each buffer in turn with
    /// [`write_all_at`](LogFile::write_all_at). An implementation that has
    /// a vectored positional write (`pwritev`) should use it: the log then
    /// takes one call, and on most filesystems one turn of the file's lock,
    /// for many records.
    ///
    /// [`write_all_at`]: LogFile::write_all_at
    fn write_all_vectored_at(&self, offset: u64, bufs: &[io::IoSlice<'_>]) -> io::Result<()> {
        let mut at = offset;
        for buf in bufs {
            self.write_all_at(at, buf)?;
            at += buf.len() as u64;
        }
        Ok(())
    }

    /// Make the contiguous written prefix durable, data only (`fdatasync`),
    /// as [`WriteFile::sync_data`] does.
    ///
    /// An environment whose [`Capabilities::durable_sync`] is `false` may
    /// return `Ok(())` without providing durability.
    fn sync_data(&self) -> io::Result<()>;

    /// Cut the file to exactly `len` bytes.
    fn set_len(&self, len: u64) -> io::Result<()>;

    /// Start writing the `len` bytes at `offset`, all written already, back
    /// to the device: a hint that leaves less for the next
    /// [`sync_data`](LogFile::sync_data) to write. Called on a thread of its
    /// own (`Options::log_writeback`), so it may wait for the writes, as the
    /// standard environment's does on Apple.
    ///
    /// It makes nothing durable and returns nothing. An implementation
    /// that cannot start the writeback does nothing, as the default does;
    /// the bytes are then written by the next sync, which reports any
    /// failure. The hint must not hide a failure from that sync: one it
    /// meets is reported by the next sync of the same handle.
    fn start_writeback(&self, offset: u64, len: u64) {
        let _ = (offset, len);
    }
}

/// A held directory lock. The lock is released when this is dropped.
pub trait FileLock: Send + Sync {}

/// A handle to a thread started by [`Env::spawn`].
pub trait JoinHandle: Send {
    /// Wait for the thread to finish. A thread that panicked is
    /// joined like any other; the panic does not propagate.
    fn join(self: Box<Self>);
}

/// The default [`Env`] for the target being compiled.
#[cfg(not(target_os = "wasi"))]
type DefaultEnv = StdEnv;

/// The default [`Env`] for the target being compiled.
#[cfg(target_os = "wasi")]
type DefaultEnv = WasiEnv;

/// The process-wide default environment: [`StdEnv`] everywhere except
/// WASI, where `WasiEnv` takes over. (`WasiEnv` only exists when
/// compiling for WASI, so it cannot be linked from a native build.)
///
/// Both are stateless, so one instance serves every database in the
/// process and [`crate::Options::default`] hands out a clone of this
/// `Arc` rather than allocating per open. A caller that never mentions
/// an [`Env`] therefore still gets the right one for the target,
/// including on `wasm32-wasip1`.
pub fn std_env() -> Arc<dyn Env> {
    static ENV: std::sync::OnceLock<Arc<DefaultEnv>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| Arc::new(DefaultEnv::new())).clone()
}

/// Monotonic microseconds from the platform clock, or `None` where
/// the platform has none.
///
/// The opt-in timing paths (performance counters, statistics
/// histograms, the token-bucket rate limiter) use this rather than an
/// [`Env`], because a caller constructs them directly and they never
/// see one. `None` disables the timing; it never records a zero.
pub(crate) fn platform_micros() -> Option<u64> {
    platform_nanos().map(|nanos| nanos / 1_000)
}

/// The same clock as [`platform_micros`], in nanoseconds.
///
/// The performance counters report nanoseconds, so they read this
/// directly rather than losing three digits and reading zero for
/// every sub-microsecond scope.
pub(crate) fn platform_nanos() -> Option<u64> {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        // `std::time::Instant::now` panics on this target, and a
        // panic on a path reachable from a public API is not an
        // acceptable way to report "no clock here".
        None
    }
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let origin = ORIGIN.get_or_init(std::time::Instant::now);
        Some(origin.elapsed().as_nanos() as u64)
    }
}

/// Microseconds elapsed since `start`, or `None` when `env` has no
/// monotonic clock.
///
/// `None` means "not measured". Callers skip the recording rather
/// than publishing a zero that reads like a measurement. This is the
/// one place that arithmetic lives, so the engine and the compaction
/// worker cannot disagree about it.
pub(crate) fn elapsed_micros(env: &dyn Env, start: Option<u64>) -> Option<u64> {
    let start = start?;
    Some(env.now_micros()?.saturating_sub(start))
}

/// Sync the directory that contains `path`.
///
/// A path with no parent (the filesystem root) has nothing to sync
/// and succeeds.
pub(crate) fn sync_parent_dir(env: &dyn Env, path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => env.sync_dir(parent),
        None => Ok(()),
    }
}

/// Remove `path`, then make its removal durable.
pub(crate) fn remove_file_and_sync_parent(env: &dyn Env, path: &Path) -> io::Result<()> {
    env.remove_file(path)?;
    sync_parent_dir(env, path)
}

/// A sequential [`std::io::Read`] cursor over a [`ReadFile`].
///
/// Bridges the positional read model to the byte-stream helpers that
/// hash and copy whole files, and to WAL replay.
///
/// Generic over how the file is held rather than fixed to a borrow, so
/// a caller that must outlive the borrow (WAL replay owns its file for
/// the life of the iterator) uses `ReadFileCursor<Box<dyn ReadFile>>`
/// and a caller that has one to hand uses `ReadFileCursor<&dyn
/// ReadFile>`. One implementation serves both, so the two cannot
/// disagree about what a short read means.
pub(crate) struct ReadFileCursor<F> {
    file: F,
    offset: u64,
    end: u64,
}

impl<F: std::ops::Deref<Target = dyn ReadFile>> ReadFileCursor<F> {
    /// Read the whole file, from byte zero.
    pub(crate) fn new(file: F) -> io::Result<Self> {
        let end = file.len()?;
        Ok(Self {
            file,
            offset: 0,
            end,
        })
    }

    /// Length of the underlying file, read once at construction.
    ///
    /// Lets a caller that also needs the length avoid a second
    /// `ReadFile::len` call, which for OPFS is a call into JS.
    pub(crate) fn len(&self) -> u64 {
        self.end
    }

    /// The file under the cursor, for a positioned read that must not
    /// move it.
    pub(crate) fn file(&self) -> &dyn ReadFile {
        &*self.file
    }
}

impl<F: std::ops::Deref<Target = dyn ReadFile>> io::Read for ReadFileCursor<F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.end.saturating_sub(self.offset);
        if remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = remaining.min(buf.len() as u64) as usize;
        self.file.read_exact_at(self.offset, &mut buf[..want])?;
        self.offset += want as u64;
        Ok(want)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_env_is_shared_not_reallocated() {
        assert!(Arc::ptr_eq(&std_env(), &std_env()));
    }

    #[test]
    fn platform_micros_is_monotonic_where_it_exists() {
        if let (Some(a), Some(b)) = (platform_micros(), platform_micros()) {
            assert!(b >= a);
        }
    }

    #[test]
    fn sync_parent_dir_accepts_existing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("file");
        env.write(&path, b"data").unwrap();

        sync_parent_dir(&*env, &path).unwrap();
    }

    #[test]
    fn remove_file_and_sync_parent_removes_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("file");
        env.write(&path, b"data").unwrap();

        remove_file_and_sync_parent(&*env, &path).unwrap();

        assert!(!env.exists(&path));
    }

    #[test]
    fn read_file_cursor_streams_the_whole_file() {
        use std::io::Read;

        let dir = tempfile::TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("streamed");
        env.write(&path, b"0123456789").unwrap();

        let file = env.open_read(&path).unwrap();
        let mut cursor = ReadFileCursor::new(&*file).unwrap();
        let mut out = Vec::new();
        cursor.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"0123456789");
    }

    #[test]
    fn elapsed_micros_is_none_without_a_clock() {
        let env = MemEnv::new();
        env.set_clocks(None, None);
        assert_eq!(elapsed_micros(&env, Some(5)), None);
        env.set_clocks(Some(1_000), Some(0));
        assert_eq!(elapsed_micros(&env, Some(400)), Some(600));
        assert_eq!(elapsed_micros(&env, None), None);
    }

    #[test]
    fn flat_children_are_the_direct_entries_files_first() {
        let path = std::path::PathBuf::from;
        let files = [
            path("db/MANIFEST"),
            path("db/sst/000001.sst"),
            path("other/MANIFEST"),
        ];
        let dirs = [path("db"), path("db/sst"), path("db/sst/deeper")];
        let listed: Vec<DirEntry> =
            flat_children(Path::new("db"), files.into_iter(), dirs.into_iter())
                .map(Result::unwrap)
                .collect();
        assert_eq!(
            listed,
            vec![
                DirEntry {
                    path: path("db/MANIFEST"),
                    is_dir: false,
                },
                DirEntry {
                    path: path("db/sst"),
                    is_dir: true,
                },
            ]
        );
    }

    #[test]
    fn flat_children_pull_the_namespace_only_as_far_as_the_walk_goes() {
        let pulled = std::cell::Cell::new(0);
        let files = (0..1_000).map(|i| {
            pulled.set(pulled.get() + 1);
            std::path::PathBuf::from(format!("db/{i}"))
        });
        let mut walk = flat_children(Path::new("db"), files, std::iter::empty());
        assert_eq!(walk.next().unwrap().unwrap().file_name(), "0");
        assert_eq!(pulled.get(), 1, "the walk read ahead of its caller");
    }
}
