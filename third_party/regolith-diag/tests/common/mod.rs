//! Shared helpers for regolith integration tests.
//!
//! Each file under `tests/` is compiled as its own integration-test
//! crate, so Rust has no inherent way to share code between them. The
//! idiomatic workaround is a `common/mod.rs` subdirectory: it is
//! *not* treated as a standalone test binary, but any test file can
//! pull it in with `mod common;`.
//!
//! Keep this module small and opinionated - helpers here get
//! recompiled once per test binary, so bloat costs compile time.

#![allow(dead_code)]

use std::fs;
use std::path::Path;

use regolith::{Db, Options, TransactionError, TxResult};
use tempfile::TempDir;

/// Options with a small write buffer (4 KiB) so memtable flushes
/// trigger quickly - useful for exercising flush and compaction paths
/// in tests without having to write megabytes of data.
pub fn small_opts() -> Options {
    Options::default().write_buffer_size(4 * 1024)
}

/// Open a fresh regolith DB in `dir` using [`small_opts`].
pub fn open(dir: &TempDir) -> Db {
    Db::open(dir.path(), small_opts()).unwrap()
}

/// Put `count` sequential keys `key_000000..key_{count-1:06}` with
/// value `val_{i:06}`. Predictable filler for write-path tests; a write a
/// stall stops waits for the stall to clear, as a blocking caller does.
pub fn fill_sequential(db: &Db, count: usize) {
    for i in 0..count {
        let k = format!("key_{:06}", i);
        let v = format!("val_{:06}", i);
        regolith::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
    }
}

/// Verify the `count` keys produced by [`fill_sequential`] are all
/// present with their expected values.
pub fn verify_sequential_keys(db: &Db, count: usize) {
    for i in 0..count {
        let k = format!("key_{:06}", i);
        let v = format!("val_{:06}", i);
        assert_eq!(
            db.get(k.as_bytes()).unwrap(),
            Some(v.as_bytes().to_vec()),
            "key {:?} missing or wrong value",
            k,
        );
    }
}

/// Count `*.sst` files under `<db_dir>/sst/`.
pub fn count_sst_files(db_dir: &Path) -> usize {
    count_with_extension(&db_dir.join("sst"), "sst")
}

/// Count `*.log` files under `<db_dir>/wal/`.
pub fn count_wal_files(db_dir: &Path) -> usize {
    count_with_extension(&db_dir.join("wal"), "log")
}

fn count_with_extension(dir: &Path, ext: &str) -> usize {
    if !dir.is_dir() {
        return 0;
    }
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some(ext))
        .count()
}

/// Force a full flush + compaction by calling `compact_range(None, None)`.
/// Blocks until the compaction scheduler reports completion.
pub fn force_compaction(db: &Db) {
    db.compact_range(None, None).wait().unwrap();
}

/// How long a call that must not wait, or must end, is given before a test
/// calls it stuck. Far past anything a loaded host needs; a test only ever
/// sits this long when it fails.
pub const STUCK: std::time::Duration = std::time::Duration::from_secs(60);

/// Run `call` on its own thread and hand back its result, failing the test
/// when it has not returned by [`STUCK`]: how a test checks that a call does
/// not wait, or does end, without hanging when it does not.
pub fn returns<T: Send + 'static>(what: &str, call: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(call());
    });
    result
        .recv_timeout(STUCK)
        .unwrap_or_else(|_| panic!("{what} did not return in {STUCK:?}"))
}

/// Wait until `reached` holds, by [`STUCK`], with a bounded backoff and no
/// fixed sleep; fails the test, naming `what`, when it never does.
pub fn eventually(what: &str, mut reached: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + STUCK;
    let mut backoff = std::time::Duration::from_micros(50);
    while !reached() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: not in {STUCK:?}"
        );
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(std::time::Duration::from_millis(5));
    }
}

/// Whether a commit landed: `true` when it committed, `false` when it lost
/// to a conflict. Any other failure fails the test with the error, so a
/// commit that failed for another reason is never read as a conflict.
#[track_caller]
pub fn committed<T>(outcome: &TxResult<T>) -> bool {
    match outcome {
        Ok(_) => true,
        Err(TransactionError::Conflict(_)) => false,
        Err(other) => panic!("a commit failed for a reason other than a conflict: {other}"),
    }
}

/// Fault injection: subprocess crash harness, power-loss simulation,
/// the valid-prefix validator, and byte-level mutators. See
/// [`fault`] for the full contract, including why a `kill -9` is not a
/// power cut.
pub mod fault;

/// A key provider for encryption at rest, with keys by id and a movable
/// current key.
pub mod keys;

/// A merge operator over values of three counters, with `touches`.
pub mod parted;

/// The format 2 write-ahead log layout, for tests that edit log bytes, and
/// a listener that records the tails an open discards.
pub mod wal_format;

/// The database 0.1.9 left with an overlapping bottom level, and what it
/// holds.
pub mod overlap_fixture;

/// The database 0.1.9 left with a range delete below older keys it covers,
/// and what it holds.
pub mod buried_fixture;

/// The first open of a database an earlier release wrote, across a power
/// cut at each of its I/O calls.
pub mod open_sweep;

/// Every warning a test thread logs, through one subscriber for the binary.
pub mod log_capture;

/// An `Env` that counts SSTable opens and reads, and can hold reads at the
/// device: what the non-blocking read tests pin.
pub mod device_env;

/// An `Env` that refuses chosen log removals and, while armed, table
/// creations: what the flush retirement tests pin.
pub mod faulty_env;

/// An `Env` that records the threads a database starts and every write and
/// sync of its log, and can hold a sync at the device: what the thread
/// budget and the allocation tests pin.
pub mod watched_env;
