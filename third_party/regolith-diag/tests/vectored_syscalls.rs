//! Proves the WAL emits ONE host write per record instead of one per field.
//!
//! Throughput is not asserted here: it is not measurable on a loaded host, and
//! this project has already had to retract two load-contaminated claims. A
//! syscall COUNT is deterministic regardless of load, so that is what is
//! pinned. Fewer host writes per record is the mechanism; whether it is faster
//! on a given device is a separate, measured question.
#![cfg(not(target_arch = "wasm32"))]

use regolith::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, StdEnv, WriteFile,
    WriteMode,
};
use regolith::{Db, Options, WriteBatch, WriteOptions};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

/// Counts host-level writes, separating vectored from sequential.
#[derive(Default)]
struct Counters {
    writes: AtomicUsize,
    vectored: AtomicUsize,
}

#[derive(Debug)]
struct CountingEnv {
    inner: StdEnv,
    counters: Arc<Counters>,
}

impl std::fmt::Debug for Counters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Counters").finish_non_exhaustive()
    }
}

struct CountingWrite {
    inner: Box<dyn WriteFile>,
    counters: Arc<Counters>,
}

impl WriteFile for CountingWrite {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.counters.writes.fetch_add(1, Ordering::Relaxed);
        self.inner.write_all(buf)
    }
    fn write_all_vectored(&mut self, slices: &[&[u8]]) -> io::Result<()> {
        self.counters.vectored.fetch_add(1, Ordering::Relaxed);
        self.inner.write_all_vectored(slices)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
}

/// The log the host hands out, with every write at an offset counted as
/// the one host write it is.
struct CountingLog {
    inner: Box<dyn regolith::env::LogFile>,
    counters: Arc<Counters>,
}

impl regolith::env::LogFile for CountingLog {
    fn write_all_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.counters.writes.fetch_add(1, Ordering::Relaxed);
        self.inner.write_all_at(offset, buf)
    }
    fn sync_data(&self) -> io::Result<()> {
        self.inner.sync_data()
    }
    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
}

impl Env for CountingEnv {
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        let inner = self.inner.open_write(path, mode)?;
        Ok(Box::new(CountingWrite {
            inner,
            counters: Arc::clone(&self.counters),
        }))
    }
    fn open_log(
        &self,
        path: &Path,
        mode: WriteMode,
    ) -> io::Result<Box<dyn regolith::env::LogFile>> {
        Ok(Box::new(CountingLog {
            inner: self.inner.open_log(path, mode)?,
            counters: Arc::clone(&self.counters),
        }))
    }
    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir_all(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(p)
    }
    fn open_read(&self, p: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(p)
    }
    fn metadata(&self, p: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(p)
    }
    fn remove_file(&self, p: &Path) -> io::Result<()> {
        self.inner.remove_file(p)
    }
    fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.rename(a, b)
    }
    fn hard_link(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.hard_link(a, b)
    }
    fn sync_dir(&self, p: &Path) -> io::Result<()> {
        self.inner.sync_dir(p)
    }
    fn lock_file(&self, p: &Path, ex: bool) -> io::Result<Box<dyn FileLock>> {
        self.inner.lock_file(p, ex)
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
        f: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.inner.spawn(name, f)
    }
}

/// A record whose payload is at or above the buffer capacity bypasses the
/// buffer. It must leave as ONE vectored host write, not as a small header
/// write followed by a payload write.
#[test]
fn a_large_wal_record_costs_one_host_write() {
    let dir = TempDir::new().unwrap();
    let counters = Arc::new(Counters::default());
    let opts = Options::default().env(Arc::new(CountingEnv {
        inner: StdEnv,
        counters: Arc::clone(&counters),
    }));
    let db = Db::open(dir.path(), opts).unwrap();

    // Settle: opening writes MANIFEST records of its own.
    counters.writes.store(0, Ordering::Relaxed);
    counters.vectored.store(0, Ordering::Relaxed);

    let wo = WriteOptions {
        sync: false,
        ..Default::default()
    };
    const RECORDS: usize = 64;
    let value = vec![0xABu8; 64 * 1024]; // far above the 8 KiB buffer
    for i in 0..RECORDS {
        let mut batch = WriteBatch::new();
        batch.put(format!("k{i:06}").as_bytes(), &value);
        db.write_opt(&wo, batch).unwrap();
    }

    let vectored = counters.vectored.load(Ordering::Relaxed);
    let plain = counters.writes.load(Ordering::Relaxed);
    println!("  {RECORDS} large records -> {vectored} vectored, {plain} sequential host writes");

    // The property is the syscall count, not which call makes it. Group
    // commit stages a whole group into one contiguous buffer and hands it
    // to the host in a single `write_all`, which subsumes what the
    // vectored path was for: written field by field, each record forced
    // its 5-byte header out in a syscall of its own, so 64 records cost
    // at least 128 writes.
    let total = vectored + plain;
    assert!(
        total <= RECORDS,
        "{RECORDS} oversized records cost {total} host writes ({vectored} vectored, \
         {plain} sequential); each record's header is still leaving in a syscall \
         of its own"
    );
    assert!(
        total > 0,
        "the counting environment saw no writes at all, so it is measuring nothing"
    );
}
