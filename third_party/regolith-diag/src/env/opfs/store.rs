//! The filesystem half of [`crate::env::Env`], written once per strategy.
//!
//! Both OPFS strategies expose the same set of operations, so the `Env`
//! implementation in the parent module dispatches through [`OpfsStore`]
//! rather than repeating itself per mode.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::env::{Capabilities, FileMeta, LogFile, ReadDir, ReadFile, WriteFile, WriteMode};

use super::mirror::MirrorFs;
use super::pool::SahPool;

/// The operations an OPFS strategy provides to the `Env` implementation.
pub(super) trait OpfsStore: Send + Sync + std::fmt::Debug {
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>>;
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>>;
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>>;
    /// The write-ahead log at `path`. Both strategies write at an offset
    /// underneath, so the log is that write with no cursor and no lock.
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>>;
    fn metadata(&self, path: &Path) -> io::Result<FileMeta>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn exists(&self, path: &Path) -> bool;
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
    fn capabilities(&self) -> Capabilities;
}

fn short_read(want: usize, got: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        format!("read {got} of {want} bytes"),
    )
}

// ---------------------------------------------------------------------
// Sync access handle pool
// ---------------------------------------------------------------------

/// [`OpfsStore`] over a pool of sync access handles.
#[derive(Debug)]
pub(super) struct PoolStore(pub(super) Arc<SahPool>);

impl OpfsStore for PoolStore {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.0.create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.0.read_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        let (slot, generation) = self.0.open_read(path)?;
        Ok(Box::new(PoolReader {
            pool: Arc::clone(&self.0),
            slot,
            generation,
        }))
    }

    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        // Only `Truncate` discards. `Update` keeps the bytes and writes
        // from the start, so it must not reach the pool's truncating
        // branch: manifest recovery opens `Update` and then shortens,
        // and a truncate here would zero the MANIFEST first and leave
        // `set_len` to extend it back with zeros, losing every SSTable
        // reference the database had.
        let (slot, generation, end) = self
            .0
            .open_write(path, !matches!(mode, WriteMode::Truncate))?;
        Ok(Box::new(PoolWriter {
            pool: Arc::clone(&self.0),
            slot,
            generation,
            offset: match mode {
                WriteMode::Append => end,
                WriteMode::Truncate | WriteMode::Update => 0,
            },
        }))
    }

    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        // As `open_write`: only `Truncate` discards.
        let (slot, generation, _end) = self
            .0
            .open_write(path, !matches!(mode, WriteMode::Truncate))?;
        Ok(Box::new(PoolLog {
            pool: Arc::clone(&self.0),
            slot,
            generation,
        }))
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        let (len, is_dir) = self.0.metadata(path)?;
        Ok(FileMeta { len, is_dir })
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.0.remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.0.rename(from, to)
    }

    fn exists(&self, path: &Path) -> bool {
        self.0.exists(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.0.sync_dir(path)
    }

    fn capabilities(&self) -> Capabilities {
        // No hard links, no threads, no cross-process lock. Rename is
        // atomic through the slot generation counter, and a directory's
        // name bindings live in the slot headers, so both of those are
        // genuinely provided rather than pretended.
        Capabilities::none()
            .with_atomic_rename(true)
            .with_sync_dir(true)
            .with_durable_sync(true)
    }
}

struct PoolReader {
    pool: Arc<SahPool>,
    slot: usize,
    generation: u64,
}

impl ReadFile for PoolReader {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let got = self.pool.read_at(self.slot, self.generation, offset, buf)?;
        if got != buf.len() {
            return Err(short_read(buf.len(), got));
        }
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        self.pool.file_len(self.slot, self.generation)
    }
}

struct PoolWriter {
    pool: Arc<SahPool>,
    slot: usize,
    generation: u64,
    offset: u64,
}

impl WriteFile for PoolWriter {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.pool
            .write_at(self.slot, self.generation, self.offset, buf)?;
        self.offset += buf.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Writes reach the handle immediately; buffering is the caller's.
        Ok(())
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.pool.sync(self.slot, self.generation)
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.pool.set_len(self.slot, self.generation, len)?;
        self.offset = self.offset.min(len);
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        self.pool.file_len(self.slot, self.generation)
    }
}

/// A log over one slot of the pool: a sync access handle writes at an
/// offset, so every write names its own and none moves a cursor.
struct PoolLog {
    pool: Arc<SahPool>,
    slot: usize,
    generation: u64,
}

impl LogFile for PoolLog {
    fn write_all_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.pool.write_at(self.slot, self.generation, offset, buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.pool.sync(self.slot, self.generation)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.pool.set_len(self.slot, self.generation, len)
    }
}

// ---------------------------------------------------------------------
// In-memory mirror
// ---------------------------------------------------------------------

/// [`OpfsStore`] over an in-memory mirror of the database.
#[derive(Debug)]
pub(super) struct MirrorStore(pub(super) Arc<MirrorFs>);

impl OpfsStore for MirrorStore {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.0.create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.0.read_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.0.file_len(path)?;
        Ok(Box::new(MirrorReader {
            fs: Arc::clone(&self.0),
            path: path.to_path_buf(),
        }))
    }

    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        let offset = self.0.create(path, matches!(mode, WriteMode::Truncate))?;
        Ok(Box::new(MirrorWriter {
            fs: Arc::clone(&self.0),
            path: path.to_path_buf(),
            offset: match mode {
                WriteMode::Append => offset,
                // `Update` keeps the contents and writes from the
                // start, which is what a caller that is about to
                // `set_len` needs.
                WriteMode::Truncate | WriteMode::Update => 0,
            },
        }))
    }

    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        self.0.create(path, matches!(mode, WriteMode::Truncate))?;
        Ok(Box::new(MirrorLog {
            fs: Arc::clone(&self.0),
            path: path.to_path_buf(),
        }))
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        let (len, is_dir) = self.0.metadata(path)?;
        Ok(FileMeta { len, is_dir })
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.0.remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.0.rename(from, to)
    }

    fn exists(&self, path: &Path) -> bool {
        self.0.exists(path)
    }

    fn sync_dir(&self, _path: &Path) -> io::Result<()> {
        // Nothing here is durable until `OpfsEnv::persist` runs, which is
        // what `Capabilities::sync_dir == false` reports.
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        // Rename is a map operation, so it is atomic within the mirror.
        // Everything about durability waits for `persist`.
        Capabilities::none().with_atomic_rename(true)
    }
}

struct MirrorReader {
    fs: Arc<MirrorFs>,
    path: PathBuf,
}

impl ReadFile for MirrorReader {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let got = self.fs.read_at(&self.path, offset, buf)?;
        if got != buf.len() {
            return Err(short_read(buf.len(), got));
        }
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        self.fs.file_len(&self.path)
    }
}

struct MirrorWriter {
    fs: Arc<MirrorFs>,
    path: PathBuf,
    offset: u64,
}

impl WriteFile for MirrorWriter {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.fs.write_at(&self.path, self.offset, buf)?;
        self.offset += buf.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn sync_all(&mut self) -> io::Result<()> {
        // Deliberately not an error: the engine syncs on every WAL write,
        // and failing here would make mirror mode unusable. The honest
        // statement is `Capabilities::durable_sync == false`, which
        // `Options::validate` turns into a refusal of
        // `DurabilityMode::Immediate` at open.
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.fs.set_len(&self.path, len)?;
        self.offset = self.offset.min(len);
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        self.fs.file_len(&self.path)
    }
}

/// A log over one file of the mirror, written at an offset. A write above
/// the file's end leaves zeros below it, as a hole on a host file does,
/// until the write that belongs there arrives.
struct MirrorLog {
    fs: Arc<MirrorFs>,
    path: PathBuf,
}

impl LogFile for MirrorLog {
    fn write_all_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.fs.write_at(&self.path, offset, buf)
    }

    /// As [`MirrorWriter::sync_all`]: nothing is durable until
    /// `OpfsEnv::persist`, which `Capabilities::durable_sync` says.
    fn sync_data(&self) -> io::Result<()> {
        Ok(())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.fs.set_len(&self.path, len)
    }
}
