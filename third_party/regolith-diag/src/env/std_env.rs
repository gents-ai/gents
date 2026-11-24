//! The default [`Env`]: `std::fs`, `std::thread`, `std::time`.
//!
//! Every call here is the same call regolith made before the [`Env`]
//! trait existed, in the same order, returning the same error kinds.
//! This is the environment [`crate::Options::default`] installs, so a
//! native user sees no behavior change at all.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use super::DiskSpace;
#[cfg(any(unix, windows, target_os = "wasi"))]
use super::LogFile;
use super::db_lock;
use super::{
    Capabilities, DirEntry, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, WriteFile,
    WriteMode,
};

/// Whether this target has a real directory fsync.
///
/// `cfg!(unix)` alone is not enough: it is `false` on
/// `wasm32-wasip1`, and opening a preopened directory there and
/// calling `sync_all` on it reports `EBADF`. Keeping the constant and
/// the behavior derived from the same expression is what stops
/// [`Capabilities::sync_dir`] from claiming a durability the platform
/// does not provide.
const SUPPORTS_DIR_SYNC: bool = cfg!(unix) && !cfg!(target_family = "wasm");

/// Whether this target can start a thread: see [`super::PLATFORM_THREADS`].
/// Declaring that up front lets a caller choose a single-threaded
/// configuration before an open fails rather than after.
const SUPPORTS_THREADS: bool = super::PLATFORM_THREADS;

/// The host platform as regolith has always used it.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdEnv;

impl StdEnv {
    /// Construct the standard environment.
    ///
    /// It holds no state, so [`crate::env::std_env`] shares one
    /// instance across the whole process rather than allocating per
    /// database.
    pub const fn new() -> Self {
        Self
    }
}

impl Env for StdEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        // `std::fs::ReadDir` is `readdir` itself: one kernel buffer of
        // entries at a time, refilled as the walk goes.
        Ok(Box::new(std::fs::read_dir(path)?.map(|entry| {
            let entry = entry?;
            // An entry whose type cannot be read (it went between the
            // listing and the `lstat` a filesystem without `d_type` needs)
            // is reported as a file: every caller skips what it cannot
            // parse, and one that acts on it meets the same error there.
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Ok(DirEntry {
                path: entry.path(),
                is_dir,
            })
        })))
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        Ok(Box::new(StdReadFile::new(File::open(path)?)))
    }

    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        let file = match mode {
            WriteMode::Truncate => OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?,
            WriteMode::Append => OpenOptions::new().create(true).append(true).open(path)?,
            // `truncate(false)` is stated rather than left to default:
            // keeping the contents is the whole difference between this
            // mode and `Truncate`.
            WriteMode::Update => OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(path)?,
        };
        Ok(Box::new(StdWriteFile { file }))
    }

    /// A log written with positional writes: no cursor, no lock, and no
    /// `O_APPEND`, under which Linux ignores the offset of a `pwrite`.
    ///
    /// Opened with the host's default sharing, as every other file here is:
    /// on Windows a second handle may read the log while this one writes
    /// it, which is how a copy of a live directory is taken.
    #[cfg(any(unix, windows, target_os = "wasi"))]
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(mode == WriteMode::Truncate)
            .open(path)?;
        Ok(Box::new(StdLogFile {
            file,
            #[cfg(target_vendor = "apple")]
            writeback: super::deferred_error::DeferredError::default(),
        }))
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        let meta = std::fs::metadata(path)?;
        Ok(FileMeta {
            len: meta.len(),
            is_dir: meta.is_dir(),
        })
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        std::fs::hard_link(src, dst)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        if SUPPORTS_DIR_SYNC {
            File::open(path)?.sync_all()
        } else {
            Ok(())
        }
    }

    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        db_lock::acquire(path, exclusive)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::posix()
            .with_sync_dir(SUPPORTS_DIR_SYNC)
            .with_file_lock(db_lock::SUPPORTS_FILE_LOCK)
            .with_threads(SUPPORTS_THREADS)
    }

    fn now_micros(&self) -> Option<u64> {
        super::platform_micros()
    }

    fn unix_secs(&self) -> Option<u64> {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            // `SystemTime::now` panics on this target rather than
            // returning an error, so it is never called there.
            None
        }
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        }
    }

    fn spawn(
        &self,
        name: &str,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        let handle = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(body)?;
        Ok(Box::new(StdJoinHandle(handle)))
    }

    #[cfg(unix)]
    fn disk_space(&self, path: &Path) -> io::Result<Option<DiskSpace>> {
        let s = rustix::fs::statvfs(path)?;
        let block = s.f_frsize;
        Ok(Some(DiskSpace::new(
            s.f_bavail.saturating_mul(block),
            s.f_blocks.saturating_mul(block),
            s.f_favail,
            s.f_files,
        )))
    }

    fn drop_page_cache(&self, path: &Path) {
        // Compaction reads and writes whole SSTables sequentially and
        // will not re-read them soon; without this hint the kernel
        // keeps those pages resident and evicts hot foreground data.
        // A failure only means the cache stayed warmer than we wanted,
        // so every error on this path is deliberately dropped.
        if let Ok(file) = OpenOptions::new().read(true).open(path) {
            drop_page_cache_for(&file);
        }
    }
}

#[cfg(target_os = "linux")]
fn drop_page_cache_for(file: &File) {
    // `rustix::fs::fadvise` is a safe wrapper around
    // `posix_fadvise`; offset 0 with no length means "the whole
    // file".
    let _ = rustix::fs::fadvise(file, 0, None, rustix::fs::Advice::DontNeed);
}

#[cfg(not(target_os = "linux"))]
fn drop_page_cache_for(_file: &File) {
    // macOS offers `F_NOCACHE`, but it must be set before any read
    // happens, which needs a flag threaded through the SSTable reader
    // open path. Nothing to do here until then.
}

/// A `std::fs::File` read positionally.
///
/// Every read names its own offset, so the handle carries no cursor and
/// concurrent readers of one file never serialize or step on each other:
/// `pread` through rustix on Unix and WASI (`fd_pread`), `seek_read` on
/// Windows. Only a target with none of these (none regolith builds for
/// with a filesystem) falls back to seek-then-read, and that needs a lock,
/// because the cursor is then shared by every reader of the handle.
#[derive(Debug)]
struct StdReadFile {
    file: File,
    #[cfg(not(any(unix, windows, target_os = "wasi")))]
    cursor: crate::sync::internal::Mutex<()>,
}

impl StdReadFile {
    fn new(file: File) -> Self {
        Self {
            file,
            #[cfg(not(any(unix, windows, target_os = "wasi")))]
            cursor: crate::sync::internal::Mutex::new(()),
        }
    }
}

/// Fill `buf` from `offset` with positional reads, retrying a short read and
/// an interrupted one. `read_at` is one positional read; `Ok(0)` is the end
/// of the file.
#[cfg(any(unix, windows, target_os = "wasi"))]
fn read_exact_positional(
    mut offset: u64,
    mut buf: &mut [u8],
    mut read_at: impl FnMut(u64, &mut [u8]) -> io::Result<usize>,
) -> io::Result<()> {
    while !buf.is_empty() {
        match read_at(offset, buf) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ));
            }
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

impl ReadFile for StdReadFile {
    #[cfg(any(unix, target_os = "wasi"))]
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_exact_positional(offset, buf, |at, chunk| {
            rustix::io::pread(&self.file, chunk, at).map_err(io::Error::from)
        })
    }

    #[cfg(windows)]
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        read_exact_positional(offset, buf, |at, chunk| self.file.seek_read(chunk, at))
    }

    #[cfg(not(any(unix, windows, target_os = "wasi")))]
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        let _guard = self.cursor.lock();
        let mut file = &self.file;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }
}

/// A `std::fs::File` opened for appending.
#[derive(Debug)]
struct StdWriteFile {
    file: File,
}

impl WriteFile for StdWriteFile {
    /// One `writev` for the whole record. `write_all_vectored` on a POSIX file
    /// is the syscall this exists for: a WAL record's header and its payload
    /// leave in one syscall, with no copy to join them.
    fn write_all_vectored(&mut self, slices: &[&[u8]]) -> io::Result<()> {
        use std::io::IoSlice;
        // A WAL record is a header and a payload, so a few slices cover
        // every caller on the write path; they stay on the stack, and a
        // longer list pays one allocation.
        const INLINE: usize = 4;
        let mut inline = [IoSlice::new(&[]); INLINE];
        let mut heap = Vec::new();
        let filled = slices.iter().filter(|s| !s.is_empty());
        let bufs: &mut [IoSlice<'_>] = if slices.len() <= INLINE {
            let mut n = 0;
            for s in filled {
                inline[n] = IoSlice::new(s);
                n += 1;
            }
            &mut inline[..n]
        } else {
            heap.extend(filled.map(|s| IoSlice::new(s)));
            &mut heap[..]
        };
        let mut rest = bufs;
        while !rest.is_empty() {
            let n = self.file.write_vectored(rest)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "vectored write made no progress",
                ));
            }
            // A short vectored write is normal: advance past the slices it
            // consumed and re-issue the remainder.
            IoSlice::advance_slices(&mut rest, n);
        }
        Ok(())
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.file.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn sync_data(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.file.set_len(len)?;
        // `set_len` does not move the cursor, so without this the next
        // write lands past the new end and leaves a hole behind it.
        self.file.seek(SeekFrom::Start(len))?;
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }
}

/// A `std::fs::File` written positionally: the log behind
/// [`StdEnv::open_log`].
///
/// Every write names its own offset, so any number of threads write one
/// handle at once and none of them waits for another: `pwrite` on Unix and
/// WASI, `seek_write` on Windows (which moves a cursor nothing here reads).
#[cfg(any(unix, windows, target_os = "wasi"))]
#[derive(Debug)]
struct StdLogFile {
    file: File,
    /// An error the log writeback lane's `fsync` got, for the next sync to
    /// report (`deferred_error.rs`).
    #[cfg(target_vendor = "apple")]
    writeback: super::deferred_error::DeferredError,
}

#[cfg(all(any(unix, target_os = "wasi"), not(miri)))]
impl StdLogFile {
    /// One vectored positional write: how many bytes it took. Through the C
    /// library's `pwritev64`, whose offset is 64 bits on 32-bit hosts too, so
    /// the fault shim the power-loss tests preload sees it.
    #[cfg(all(target_os = "linux", any(target_env = "gnu", target_env = "musl")))]
    fn pwritev(&self, offset: u64, bufs: &[std::io::IoSlice<'_>]) -> io::Result<usize> {
        use std::os::fd::AsRawFd;
        let offset = i64::try_from(offset).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "a log offset past what the host can address",
            )
        })?;
        // At most what one call takes; the caller writes the rest after.
        let count = bufs.len().min(1024);
        // SAFETY: `IoSlice` is ABI-compatible with `iovec` on Unix, and the
        // `count` slices it points at are borrowed for the call, which only
        // reads them. The descriptor is this handle's own and stays open.
        #[allow(unsafe_code)]
        let wrote = unsafe {
            libc::pwritev64(
                self.file.as_raw_fd(),
                bufs.as_ptr().cast::<libc::iovec>(),
                count as libc::c_int,
                offset,
            )
        };
        usize::try_from(wrote).map_err(|_| io::Error::last_os_error())
    }

    /// One vectored positional write: how many bytes it took.
    #[cfg(not(all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))))]
    fn pwritev(&self, offset: u64, bufs: &[std::io::IoSlice<'_>]) -> io::Result<usize> {
        rustix::io::pwritev(&self.file, bufs, offset).map_err(io::Error::from)
    }
}

#[cfg(any(unix, windows, target_os = "wasi"))]
impl LogFile for StdLogFile {
    // Through `std`, which calls the C library's `pwrite`: the fault shim
    // the power-loss tests preload sees every log write.
    #[cfg(unix)]
    fn write_all_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        std::os::unix::fs::FileExt::write_all_at(&self.file, buf, offset)
    }

    #[cfg(not(unix))]
    fn write_all_at(&self, mut offset: u64, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            #[cfg(windows)]
            let wrote = std::os::windows::fs::FileExt::seek_write(&self.file, buf, offset);
            #[cfg(target_os = "wasi")]
            let wrote = rustix::io::pwrite(&self.file, buf, offset).map_err(io::Error::from);
            match wrote {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "a positional write made no progress",
                    ));
                }
                Ok(n) => {
                    buf = &buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    // One `pwritev` for the run of records the log's write unit gathered,
    // re-issued for what a short write left; one `pwrite` for a run of one. On Linux through the C
    // library's `pwritev`, as `write_all_at` goes through its `pwrite`, so
    // the fault shim the power-loss tests preload sees every log write. Miri
    // cannot call into the C library, and takes the default.
    #[cfg(all(any(unix, target_os = "wasi"), not(miri)))]
    fn write_all_vectored_at(
        &self,
        mut offset: u64,
        bufs: &[std::io::IoSlice<'_>],
    ) -> io::Result<()> {
        // A run of one record is a plain positional write: the kernel
        // copies no vector for it.
        if let [one] = bufs {
            return self.write_all_at(offset, one);
        }
        // The slices the call is handed are the caller's; a short write
        // advances a copy of the list, which stays on the stack for a run of
        // up to `INLINE` records and takes one allocation past that.
        const INLINE: usize = 64;
        let mut inline = [std::io::IoSlice::new(&[]); INLINE];
        let mut heap = Vec::new();
        let mut rest: &mut [std::io::IoSlice<'_>] = if bufs.len() <= INLINE {
            inline[..bufs.len()].copy_from_slice(bufs);
            &mut inline[..bufs.len()]
        } else {
            heap.extend_from_slice(bufs);
            &mut heap[..]
        };
        while !rest.is_empty() {
            match self.pwritev(offset, rest) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "a positional write made no progress",
                    ));
                }
                Ok(n) => {
                    offset += n as u64;
                    std::io::IoSlice::advance_slices(&mut rest, n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    #[cfg(not(target_vendor = "apple"))]
    fn sync_data(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    // `F_FULLFSYNC` on Apple. It runs after any `fsync` the writeback lane has
    // in flight, and reports that one's error first if it got one: the host
    // may report a failed page to one sync and not the next.
    #[cfg(target_vendor = "apple")]
    fn sync_data(&self) -> io::Result<()> {
        self.writeback.sync(|| self.file.sync_data())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }

    // `sync_file_range` with `SYNC_FILE_RANGE_WRITE` alone starts the
    // writeback of the range's dirty pages and waits for none of it. It does
    // not move the file's writeback error past this handle, so `fdatasync`
    // still reports a page that failed; a writeback it could not start leaves
    // the pages dirty for that sync. So its result is not needed. Miri cannot
    // call into the C library.
    // Apple has no call that starts writeback without waiting for it, so the
    // lane, on its own thread, writes the pages out with a plain `fsync`: no
    // drive cache flush, which the seal's `F_FULLFSYNC` adds. The whole file,
    // whatever the range. A failed one is kept for the next sync of this
    // handle to report, never dropped (`deferred_error.rs`).
    #[cfg(target_vendor = "apple")]
    fn start_writeback(&self, _offset: u64, _len: u64) {
        self.writeback.writeback(|| {
            #[cfg(test)]
            if log_tests::FAIL_WRITEBACK.with(std::cell::Cell::take) {
                return Err(io::Error::other("injected writeback failure"));
            }
            rustix::fs::fsync(&self.file).map_err(io::Error::from)
        });
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    fn start_writeback(&self, offset: u64, len: u64) {
        use std::os::fd::AsRawFd;
        let (Ok(offset), Ok(len)) = (i64::try_from(offset), i64::try_from(len)) else {
            return;
        };
        // SAFETY: the descriptor is this handle's own and stays open for the
        // call, which reads and writes no memory of this process.
        #[allow(unsafe_code)]
        let _ = unsafe {
            libc::sync_file_range(
                self.file.as_raw_fd(),
                offset,
                len,
                libc::SYNC_FILE_RANGE_WRITE,
            )
        };
    }
}

struct StdJoinHandle(std::thread::JoinHandle<()>);

impl JoinHandle for StdJoinHandle {
    fn join(self: Box<Self>) {
        // A worker asked to join itself is the one tearing the database
        // down: it held the last reference, inside a job whose caller
        // dropped its handle without waiting. It cannot wait for its own
        // exit, which follows as soon as the job returns to its loop and
        // the loop finds the shutdown it just signalled; the handle is let
        // go instead. The directory stays locked until then: the worker
        // holds a share of the lock and drops it as it exits
        // (`CompactionScheduler::start`).
        if self.0.thread().id() == std::thread::current().id() {
            return;
        }
        // A worker that panicked is joined like any other: the panic
        // has already been reported on its own thread, and shutdown
        // must not turn it into a second one here.
        let _ = self.0.join();
    }
}

#[cfg(test)]
mod update_mode_tests {
    use super::*;
    use crate::env::WriteMode;

    /// The regression this guards: manifest recovery trims a torn tail
    /// with `set_len`, and it used to do that through the same append
    /// handle it then wrote through. On Windows an append handle is
    /// opened without `FILE_WRITE_DATA`, which `SetEndOfFile` needs, so
    /// every reopen of a database whose MANIFEST had a torn tail failed
    /// with "Access is denied" - which is to say, every reopen after the
    /// ordinary kind of crash.
    #[test]
    fn a_file_opened_for_update_keeps_its_contents_and_can_be_shortened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("log");
        let env = StdEnv::new();

        {
            let mut w = Env::open_write(&env, &path, WriteMode::Truncate).expect("create");
            w.write_all(b"0123456789").expect("write");
            w.sync_all().expect("sync");
        }

        {
            let mut w = Env::open_write(&env, &path, WriteMode::Update).expect("open update");
            w.set_len(4).expect(
                "shortening through an Update handle is what manifest recovery does on every \
                 reopen after a crash",
            );
            w.sync_all().expect("sync");
        }

        assert_eq!(
            Env::read(&env, &path).expect("read"),
            b"0123456789"[..4].to_vec(),
            "Update must keep what it did not remove",
        );

        // And the log is still appendable afterwards, which is the next
        // thing recovery does.
        {
            let mut w = Env::open_write(&env, &path, WriteMode::Append).expect("open append");
            w.write_all(b"xy").expect("append");
            w.sync_all().expect("sync");
        }
        assert_eq!(Env::read(&env, &path).expect("read"), b"0123xy".to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::std_env;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Many threads read one handle at their own offsets at once. Each read
    /// names its offset, so none lands at another's: a shared cursor moved
    /// by a seek and then a read would hand a thread another thread's bytes.
    #[test]
    fn concurrent_positional_reads_of_one_handle_never_cross() {
        const BLOCKS: u64 = 256;
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("blocks");
        let bytes: Vec<u8> = (0..BLOCKS).flat_map(|b| [b as u8; 64]).collect();
        env.write(&path, &bytes).unwrap();
        let file: Arc<dyn ReadFile> = Arc::from(env.open_read(&path).unwrap());
        let readers: Vec<_> = (0..8u64)
            .map(|t| {
                let file = Arc::clone(&file);
                std::thread::spawn(move || {
                    for round in 0..2_000u64 {
                        let block = (t * 31 + round * 7) % BLOCKS;
                        let mut buf = [0u8; 64];
                        file.read_exact_at(block * 64, &mut buf).unwrap();
                        assert_eq!(buf, [block as u8; 64], "thread {t} read block {block}");
                    }
                })
            })
            .collect();
        for reader in readers {
            reader.join().unwrap();
        }
        let mut past_end = [0u8; 65];
        assert_eq!(
            file.read_exact_at((BLOCKS - 1) * 64, &mut past_end)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn write_read_round_trips_through_the_trait() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("round.trip");
        env.write(&path, b"payload").unwrap();
        assert_eq!(env.read(&path).unwrap(), b"payload");
        assert_eq!(env.metadata(&path).unwrap().len, 7);
        assert!(env.exists(&path));
        env.remove_file(&path).unwrap();
        assert!(!env.exists(&path));
    }

    #[test]
    fn append_mode_keeps_prior_contents() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("appended");
        env.write(&path, b"first").unwrap();
        let mut w = env.open_write(&path, WriteMode::Append).unwrap();
        w.write_all(b"second").unwrap();
        w.flush().unwrap();
        drop(w);
        assert_eq!(env.read(&path).unwrap(), b"firstsecond");
    }

    #[test]
    fn truncate_mode_discards_prior_contents() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("truncated");
        env.write(&path, b"0123456789").unwrap();
        env.write(&path, b"ab").unwrap();
        assert_eq!(env.read(&path).unwrap(), b"ab");
    }

    #[test]
    fn positional_reads_do_not_disturb_each_other() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("positional");
        env.write(&path, b"0123456789").unwrap();
        let file = env.open_read(&path).unwrap();
        let mut a = [0u8; 3];
        let mut b = [0u8; 3];
        file.read_exact_at(6, &mut a).unwrap();
        file.read_exact_at(0, &mut b).unwrap();
        assert_eq!(&a, b"678");
        assert_eq!(&b, b"012");
        assert_eq!(file.len().unwrap(), 10);
    }

    #[test]
    fn read_past_the_end_is_an_unexpected_eof() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("short");
        env.write(&path, b"ab").unwrap();
        let file = env.open_read(&path).unwrap();
        let mut buf = [0u8; 8];
        let err = file.read_exact_at(0, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_dir_lists_files_and_directories() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        env.create_dir_all(&dir.path().join("sub")).unwrap();
        env.write(&dir.path().join("file"), b"x").unwrap();

        let mut entries: Vec<_> = env
            .read_dir(dir.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].file_name(), "file");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[1].file_name(), "sub");
        assert!(entries[1].is_dir);
    }

    /// A directory of many `readdir` buffers is walked whole, each entry
    /// once, while the walker removes every entry it was handed.
    #[test]
    fn a_large_walk_yields_each_entry_once_while_the_walker_removes_them() {
        const FILES: usize = 5_000;
        let dir = TempDir::new().unwrap();
        let env = std_env();
        for i in 0..FILES {
            env.write(&dir.path().join(format!("{i:06}.sst")), b"x")
                .unwrap();
        }
        let mut seen = std::collections::HashSet::new();
        for entry in env.read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            assert!(!entry.is_dir);
            assert!(seen.insert(entry.path.clone()), "{:?} twice", entry.path);
            env.remove_file(&entry.path).unwrap();
        }
        assert_eq!(seen.len(), FILES);
        assert_eq!(env.read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_walk_of_a_missing_directory_is_not_found() {
        let dir = TempDir::new().unwrap();
        let err = std_env()
            .read_dir(&dir.path().join("missing"))
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn rename_replaces_the_destination() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        env.write(&from, b"new").unwrap();
        env.write(&to, b"old").unwrap();
        env.rename(&from, &to).unwrap();
        assert_eq!(env.read(&to).unwrap(), b"new");
        assert!(!env.exists(&from));
    }

    #[test]
    fn sync_dir_matches_the_declared_capability() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        env.sync_dir(dir.path()).unwrap();
        assert_eq!(env.capabilities().sync_dir, SUPPORTS_DIR_SYNC);
    }

    #[test]
    fn hard_link_shares_content() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        env.write(&src, b"linked").unwrap();
        env.hard_link(&src, &dst).unwrap();
        assert_eq!(env.read(&dst).unwrap(), b"linked");
    }

    #[test]
    fn spawn_runs_the_body_and_join_waits_for_it() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let env = std_env();
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let handle = env
            .spawn(
                "regolith-env-test",
                Box::new(move || flag.store(true, Ordering::Release)),
            )
            .unwrap();
        handle.join();
        assert!(done.load(Ordering::Acquire));
    }

    #[test]
    fn the_clocks_report_something_on_a_hosted_target() {
        let env = std_env();
        assert!(env.now_micros().is_some());
        assert!(env.unix_secs().is_some());
    }

    #[test]
    fn set_len_truncates_the_file() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("trunc");
        env.write(&path, b"0123456789").unwrap();
        // `Update`, not `Append`: shortening needs write access that an
        // append handle does not carry on Windows.
        let mut w = env.open_write(&path, WriteMode::Update).unwrap();
        w.set_len(3).unwrap();
        w.sync_all().unwrap();
        assert_eq!(w.len().unwrap(), 3);
        drop(w);
        assert_eq!(env.read(&path).unwrap(), b"012");
    }

    #[test]
    fn drop_page_cache_on_a_real_file_does_not_panic() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("hint.bin");
        env.write(&path, &vec![0u8; 4096]).unwrap();
        env.drop_page_cache(&path);
    }

    #[test]
    fn drop_page_cache_on_a_missing_path_is_silent() {
        let dir = TempDir::new().unwrap();
        std_env().drop_page_cache(&dir.path().join("never_existed"));
    }
}

#[cfg(test)]
mod log_tests {
    use crate::env::{WriteMode, std_env};
    use tempfile::TempDir;

    #[cfg(target_vendor = "apple")]
    std::thread_local! {
        /// Set: the next writeback this thread starts on a log fails.
        pub(super) static FAIL_WRITEBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// On Apple a writeback is a plain `fsync`, and an error it got is
    /// reported by the next sync of the handle, once; one that succeeds
    /// leaves the next sync as it was.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_failed_writeback_is_reported_by_the_next_sync_of_the_log() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let log = env
            .open_log(&dir.path().join("log"), WriteMode::Truncate)
            .unwrap();
        log.write_all_at(0, b"0123").unwrap();
        log.start_writeback(0, 4);
        log.sync_data().unwrap();

        log.write_all_at(4, b"4567").unwrap();
        FAIL_WRITEBACK.with(|fail| fail.set(true));
        log.start_writeback(0, 8);
        let err = log.sync_data().unwrap_err();
        assert!(err.to_string().contains("injected"), "{err}");
        log.sync_data().unwrap();
        assert_eq!(env.read(&dir.path().join("log")).unwrap(), b"01234567");
    }

    #[test]
    fn positional_writes_land_at_their_offsets_in_any_order() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("log");
        let log = env.open_log(&path, WriteMode::Truncate).unwrap();
        log.write_all_at(8, b"89ab").unwrap();
        log.write_all_at(0, b"0123").unwrap();
        log.write_all_at(4, b"4567").unwrap();
        // The same bytes again: the contract every helper relies on.
        log.write_all_at(4, b"4567").unwrap();
        log.sync_data().unwrap();
        assert_eq!(env.read(&path).unwrap(), b"0123456789ab");
    }

    #[test]
    fn a_log_opened_for_update_keeps_its_bytes_and_is_cut_by_set_len() {
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("log");
        env.write(&path, b"stamp+tail").unwrap();
        let log = env.open_log(&path, WriteMode::Update).unwrap();
        log.set_len(5).unwrap();
        log.write_all_at(5, b"=next").unwrap();
        assert_eq!(env.read(&path).unwrap(), b"stamp=next");
    }

    /// Many threads write disjoint records of one log at once, each record
    /// twice: nothing is lost and nothing lands in another record's range.
    #[test]
    fn concurrent_positional_writes_do_not_disturb_each_other() {
        const THREADS: usize = 8;
        const RECORDS: usize = 200;
        const LEN: usize = 37;
        let dir = TempDir::new().unwrap();
        let env = std_env();
        let path = dir.path().join("log");
        let log: std::sync::Arc<dyn crate::env::LogFile> =
            std::sync::Arc::from(env.open_log(&path, WriteMode::Truncate).unwrap());
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let log = std::sync::Arc::clone(&log);
                scope.spawn(move || {
                    for r in (0..RECORDS).filter(|r| r % THREADS == t) {
                        let bytes = vec![(r % 251) as u8; LEN];
                        log.write_all_at((r * LEN) as u64, &bytes).unwrap();
                        log.write_all_at((r * LEN) as u64, &bytes).unwrap();
                    }
                });
            }
        });
        let got = env.read(&path).unwrap();
        assert_eq!(got.len(), RECORDS * LEN);
        for (r, chunk) in got.chunks(LEN).enumerate() {
            assert!(chunk.iter().all(|b| *b == (r % 251) as u8), "record {r}");
        }
    }
}
