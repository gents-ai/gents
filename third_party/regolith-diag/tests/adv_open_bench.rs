//! Proves `Db::open` reads SSTable metadata, not SSTable data.
//!
//! Open touches, per SSTable: a footer, an index block, a bloom filter
//! (all three a function of file count and key count), and - measured
//! here, not assumed - exactly one data block per file, because
//! `Db::open` seeks the merging iterator across the reserved
//! column-family-registry key range to discover any persisted CF names
//! (`Db::load_cf_registry`). That seek lands on each file's first data
//! block and never reads a second one, so it costs one `block_size`
//! chunk per file, not per entry or per on-disk byte. None of this is
//! observable as a byte count anywhere in the public API, so this test
//! installs a `CountingEnv` (the same decorator pattern as
//! `vectored_syscalls.rs`) that wraps `StdEnv` and sums `read_exact_at`
//! bytes, bucketed by directory (`sst/`, `MANIFEST`, `wal/`) from the
//! path each `ReadFile` was opened with. Counters are reset immediately
//! before `Db::open` and read immediately after, so the window is
//! exactly the open call.
//!
//! Two databases, DB-A and DB-B, are built with the same file count and
//! the same keys; the only difference is DB-B's values are ~8x larger,
//! which puts its on-disk `sst/` bytes at ~6.5x DB-A's (measured; see
//! the comment on the premise check). If open's cost is bounded per
//! file rather than per byte, the bytes it reads at open barely move
//! between the two; if open ever streamed data proportional to file
//! size, the read bytes would track the on-disk bytes instead. That
//! gives three assertions, all byte counts and ratios captured in one
//! run, none of them wall-clock:
//!
//! 1. A premise self-check: the on-disk byte ratio really is large and
//!    the decorator really measured something nonzero, so the test
//!    cannot pass by comparing two things that are actually the same,
//!    or by measuring nothing.
//! 2. An absolute budget: bytes read from `sst/` at open must be a
//!    minority of the on-disk `sst/` total, for both databases. The
//!    per-file data block pushes this well above what footer/index/
//!    bloom alone would cost, so the bound below is pinned to what was
//!    actually measured, not to that smaller arithmetic estimate.
//! 3. The invariance leg: the ratio of sst-bytes-read-at-open between
//!    DB-B and DB-A must stay far below the on-disk byte ratio between
//!    them. This is the metadata-vs-data distinction, expressed as a
//!    same-run ratio so machine speed and disk never enter it. The
//!    per-file data block does not break this: it is one `block_size`
//!    chunk regardless of value size, so it is close to the same
//!    absolute cost in both databases and barely moves the ratio.
//!
//! `Db::open`'s wall-clock cost is still measured and printed as a
//! reported-only figure, in the same spirit as the vectored write-count
//! test: informative, never asserted.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use regolith::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, StdEnv, WriteFile,
    WriteMode,
};
use regolith::{Db, Options};
use tempfile::TempDir;

/// SSTables to flush, one per batch. Small enough to keep the fixture
/// under a few seconds on a loaded runner, large enough that a single
/// stray block read would be a rounding error the assertions would
/// still catch.
const FILES: usize = 8;
/// Keys per file. At `bloom_bits_per_key` 10 this is the term that sets
/// the metadata floor (bloom bits scale with key count, not value
/// size), so it has to be big enough that the metadata is not "zero
/// either way".
const ENTRIES_PER_FILE: usize = 2000;
/// `Db::open` reps per database, for the reported (not asserted) timing
/// line.
const OPEN_REPS: usize = 3;

/// Sums bytes read through `read_exact_at`, bucketed by which directory
/// the file lives under. `Debug` is required by `Env`; it deliberately
/// omits the counts themselves so the derived `Options: Debug` impl
/// stays cheap to print.
#[derive(Default)]
struct ByteCounters {
    sst: AtomicU64,
    manifest: AtomicU64,
    wal: AtomicU64,
}

impl std::fmt::Debug for ByteCounters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ByteCounters").finish_non_exhaustive()
    }
}

impl ByteCounters {
    fn reset(&self) {
        self.sst.store(0, Ordering::Relaxed);
        self.manifest.store(0, Ordering::Relaxed);
        self.wal.store(0, Ordering::Relaxed);
    }

    /// Classifies by path component, not by a `/`-joined string match,
    /// so the `sst` and `wal` buckets are exact on Windows too.
    fn add(&self, path: &Path, bytes: usize) {
        let bytes = bytes as u64;
        if path.file_name().is_some_and(|n| n == "MANIFEST") {
            self.manifest.fetch_add(bytes, Ordering::Relaxed);
        } else if path.components().any(|c| c.as_os_str() == "sst") {
            self.sst.fetch_add(bytes, Ordering::Relaxed);
        } else if path.components().any(|c| c.as_os_str() == "wal") {
            self.wal.fetch_add(bytes, Ordering::Relaxed);
        }
    }
}

#[derive(Debug)]
struct CountingEnv {
    inner: StdEnv,
    counters: Arc<ByteCounters>,
}

struct CountingRead {
    inner: Box<dyn ReadFile>,
    path: PathBuf,
    counters: Arc<ByteCounters>,
}

impl ReadFile for CountingRead {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact_at(offset, buf)?;
        self.counters.add(&self.path, buf.len());
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
}

impl Env for CountingEnv {
    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir_all(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(p)
    }
    fn open_read(&self, p: &Path) -> io::Result<Box<dyn ReadFile>> {
        let inner = self.inner.open_read(p)?;
        Ok(Box::new(CountingRead {
            inner,
            path: p.to_path_buf(),
            counters: Arc::clone(&self.counters),
        }))
    }
    fn open_log(&self, p: &Path, mode: WriteMode) -> io::Result<Box<dyn regolith::env::LogFile>> {
        self.inner.open_log(p, mode)
    }
    fn open_write(&self, p: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.inner.open_write(p, mode)
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

fn bench_opts(env: Arc<dyn Env>) -> Options {
    Options::default()
        .env(env)
        .max_background_compactions(0)
        .l0_compaction_trigger(1_000_000)
        .level0_slowdown_writes_trigger(1_000_000)
        .level0_stop_writes_trigger(1_000_000)
}

/// Deterministic, poorly-compressible bytes: a seeded xorshift denies
/// LZ4 an easy win, so a "512-byte value" really costs close to 512
/// bytes on disk instead of collapsing to a run-length code.
fn seeded_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Builds a database of `FILES` SSTables, `ENTRIES_PER_FILE` keys each,
/// with `value_len`-byte values. One `Db::flush()` per batch makes the
/// file count exact rather than a function of `write_buffer_size`; with
/// `max_background_compactions: 0` and the huge L0 triggers, nothing
/// ever merges those files back together. `db.close()` before returning
/// flushes the active memtable and advances the WAL's replay floor, so
/// a later `Db::open` for measurement replays nothing.
fn build_fixture(dir: &Path, value_len: usize) {
    let opts = bench_opts(Arc::new(StdEnv));
    let db = Db::open(dir, opts).unwrap();
    for file in 0..FILES {
        for entry in 0..ENTRIES_PER_FILE {
            let key = format!("k_{file:04}_{entry:08}");
            let value = seeded_bytes((file * ENTRIES_PER_FILE + entry) as u64, value_len);
            db.put(key.as_bytes(), &value).unwrap();
        }
        db.flush().unwrap();
    }
    db.close().unwrap();
    drop(db);
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.metadata().unwrap().len())
        .sum()
}

fn file_count(dir: &Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

struct Measured {
    label: &'static str,
    files: usize,
    on_disk_bytes: u64,
    sst_read_bytes: u64,
    manifest_read_bytes: u64,
    wal_read_bytes: u64,
    open_median: std::time::Duration,
    open_min: std::time::Duration,
    open_max: std::time::Duration,
}

/// Opens the fixture at `dir` `OPEN_REPS` times through a fresh
/// `CountingEnv` each time, keeping the counts from the final rep (the
/// one whose wall-clock is also reported) since every rep reads
/// identical metadata from an unchanged, already-closed database.
fn measure(label: &'static str, dir: &Path) -> Measured {
    let files = file_count(&dir.join("sst"));
    let on_disk_bytes = dir_bytes(&dir.join("sst"));

    let counters = Arc::new(ByteCounters::default());
    let mut times = Vec::with_capacity(OPEN_REPS);
    for _ in 0..OPEN_REPS {
        let env = Arc::new(CountingEnv {
            inner: StdEnv,
            counters: Arc::clone(&counters),
        });
        counters.reset();
        let opts = bench_opts(env);
        let t = Instant::now();
        let db = Db::open(dir, opts).unwrap();
        times.push(t.elapsed());
        drop(db);
    }
    times.sort();

    Measured {
        label,
        files,
        on_disk_bytes,
        sst_read_bytes: counters.sst.load(Ordering::Relaxed),
        manifest_read_bytes: counters.manifest.load(Ordering::Relaxed),
        wal_read_bytes: counters.wal.load(Ordering::Relaxed),
        open_median: times[times.len() / 2],
        open_min: times[0],
        open_max: times[times.len() - 1],
    }
}

fn print_row(m: &Measured) {
    let pct = m.sst_read_bytes as f64 / m.on_disk_bytes as f64 * 100.0;
    println!(
        "{:<8} files={:<3} on_disk_sst_bytes={:<10} sst_read_bytes={:<8} \
         manifest_read_bytes={:<6} wal_read_bytes={:<4} read_pct={:.3}% \
         open(reported, not asserted): median={:?} min={:?} max={:?}",
        m.label,
        m.files,
        m.on_disk_bytes,
        m.sst_read_bytes,
        m.manifest_read_bytes,
        m.wal_read_bytes,
        pct,
        m.open_median,
        m.open_min,
        m.open_max,
    );
}

#[test]
fn open_reads_far_less_than_the_data_it_has_on_disk() {
    let dir_a = TempDir::new().unwrap();
    build_fixture(dir_a.path(), 64);
    let a = measure("DB-A(64B)", dir_a.path());
    print_row(&a);

    assert!(
        a.sst_read_bytes > 0,
        "the counting environment saw zero sst bytes read at open, so it is \
         measuring nothing"
    );

    // Measured on this machine: DB-A's sst/ open read about 13.7% of its
    // on-disk sst/ bytes. The footer, bloom filter, and index account for
    // only a few KiB of that; the rest is the one `block_size` data block
    // per file that `Db::load_cf_registry` seeks into at open (see the
    // module doc). That per-file cost does not shrink with more entries in
    // this small an on-disk footprint, so 25% is pinned with headroom over
    // the measured 13.7% rather than over a footer-only estimate that
    // turned out not to be the dominant term.
    let pct = a.sst_read_bytes as f64 / a.on_disk_bytes as f64;
    assert!(
        pct < 0.25,
        "DB-A: open read {} of {} on-disk sst bytes ({:.1}%), expected under \
         25%; open must read metadata and at most one data block per file, \
         not the file's data proper",
        a.sst_read_bytes,
        a.on_disk_bytes,
        pct * 100.0
    );
}

#[test]
fn open_cost_does_not_grow_with_the_data_bytes_behind_the_same_file_count() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    build_fixture(dir_a.path(), 64);
    build_fixture(dir_b.path(), 512);

    let a = measure("DB-A(64B)", dir_a.path());
    let b = measure("DB-B(512B)", dir_b.path());
    print_row(&a);
    print_row(&b);

    assert_eq!(
        a.files, b.files,
        "the fixtures must share the same file count"
    );

    let on_disk_ratio = b.on_disk_bytes as f64 / a.on_disk_bytes as f64;
    assert!(
        on_disk_ratio >= 5.0,
        "premise check failed: DB-B's on-disk sst bytes ({}) are only {:.2}x \
         DB-A's ({}), expected >= 5x from ~8x larger values; LZ4 may have \
         collapsed the values, or the fixtures diverged",
        b.on_disk_bytes,
        on_disk_ratio,
        a.on_disk_bytes
    );
    assert!(
        a.sst_read_bytes > 0 && b.sst_read_bytes > 0,
        "the counting environment saw zero sst bytes read at open for at \
         least one database, so it is measuring nothing"
    );

    let read_ratio = b.sst_read_bytes as f64 / a.sst_read_bytes as f64;
    // Measured on this machine: sst_read_bytes was 91900 for DB-A and 113699
    // for DB-B, a 1.24x ratio, against an on-disk ratio of 6.48x. Bloom bytes
    // and index bytes are identical or near-identical between A and B; the
    // one data block `Db::load_cf_registry` seeks into per file is a single
    // `block_size` chunk in both databases regardless of value size, so it
    // barely moves the ratio either. The bound below is pinned at roughly
    // 1.7x the measured 1.24, comfortably under the >= 5x on-disk ratio the
    // premise check above requires: metadata-and-one-block reads land near
    // 1.2, a data-proportional open would land near 6.5+, and this bound
    // sits far enough below that only a real regression could cross it.
    const MAX_READ_RATIO: f64 = 2.1;
    assert!(
        read_ratio < MAX_READ_RATIO,
        "open's sst bytes read grew {:.2}x from DB-A to DB-B while on-disk \
         bytes grew {:.2}x; open must scale with file and key count, not \
         with value bytes (bound is {MAX_READ_RATIO}x)",
        read_ratio,
        on_disk_ratio
    );
}
