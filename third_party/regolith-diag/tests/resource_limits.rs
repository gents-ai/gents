//! Resource exhaustion and extremes.
//!
//! Everything here pushes regolith past a limit rather than through a
//! normal workload: a filesystem that runs out of space mid-write, keys
//! and values sitting exactly on and one byte over the configured
//! maxima, degenerate zero-length keys and values, six-figure operation
//! counts, megabyte keys, and an LSM tree deep enough that a read has to
//! walk past L3.
//!
//! # Two things this file is careful about
//!
//! **The boundary is asserted on both write paths.** `Db::put` checks
//! the user key directly while `WriteBatch` checks a key that already
//! carries a 4-byte column-family prefix and subtracts it again
//! (`src/lib.rs::validate_prefixed_key_size`). Those are two
//! implementations of one rule, so every limit test drives both and
//! asserts they agree. A test that only exercised `Db::put` would not
//! notice the batch path being off by four bytes.
//!
//! **ENOSPC is produced, not simulated.** The out-of-space test mounts a
//! real, small `tmpfs` inside an unprivileged user + mount namespace and
//! re-executes this test binary inside it, so the engine meets a genuine
//! `ENOSPC` from the kernel. Nothing about regolith's I/O is mocked. If the
//! namespace cannot be created the test fails loudly and says why; it
//! never passes without having filled a filesystem.
//!
//! # Memory high-water marks
//!
//! Every extreme reports its peak resident set size. The kernel's
//! peak-RSS counter is reset to the current RSS immediately before each
//! workload (`/proc/self/clear_refs`), so the number is that workload's
//! own peak rather than the whole binary's - but only when nothing else
//! runs concurrently in the process. Run the file through
//! `just test-extremes`, which pins `--test-threads 1`. When the counter
//! cannot be read or reset, the report says so instead of printing a
//! number that would not mean what it claims.
//!
//! # Runtime
//!
//! Every test here is in the default run. Everything that writes six
//! figures of operations, a 64 MiB value, or fills a filesystem states its
//! measured runtime at the test.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::fs;
use std::time::{Duration, Instant};

use regolith::{Db, Error, Options, WriteBatch};
use tempfile::TempDir;

mod common;

#[test]
fn crash_child() {
    common::fault::child_entrypoint(common::fault::builtin_workload);
}

// ---------------------------------------------------------------------
// Memory high-water-mark reporting
// ---------------------------------------------------------------------

/// Peak resident set size of this process in KiB, or `None` when the
/// kernel does not expose `VmHWM`.
fn peak_rss_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

/// Reset the kernel's peak-RSS counter to the current RSS so the next
/// measurement belongs to the workload about to run. Returns whether the
/// reset actually happened.
fn reset_peak_rss() -> bool {
    fs::write("/proc/self/clear_refs", b"5\n").is_ok()
}

/// Run `f`, then print its peak RSS. The caveats are printed with the
/// number so a reader can tell what it does and does not cover.
fn measured<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let reset = reset_peak_rss();
    let started = Instant::now();
    let out = f();
    let elapsed = started.elapsed();
    match (peak_rss_kib(), reset) {
        (Some(kib), true) => println!(
            "[resource_limits] {label}: peak RSS {kib} KiB, {elapsed:.2?} \
             (counter reset before the workload; only isolated under --test-threads=1)"
        ),
        (Some(kib), false) => println!(
            "[resource_limits] {label}: peak RSS {kib} KiB, {elapsed:.2?} \
             (process-wide since start - the peak counter could not be reset)"
        ),
        (None, _) => println!(
            "[resource_limits] {label}: peak RSS not measured \
             (this kernel does not expose /proc/self/status VmHWM), {elapsed:.2?}"
        ),
    }
    out
}

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

fn opts_with(write_buffer_size: usize) -> Options {
    Options::default().write_buffer_size(write_buffer_size)
}

fn expect_invalid_argument(result: regolith::Result<()>, what: &str) {
    match result {
        Err(Error::InvalidArgument(message)) => {
            assert!(
                message.contains(what),
                "the rejection must name the limit it enforced, got {message:?}"
            );
        }
        Err(other) => panic!("expected InvalidArgument for {what}, got {other:?}"),
        Ok(()) => panic!("{what} must be rejected, but the write was accepted"),
    }
}

/// Deterministic, poorly-compressible bytes. A seeded xorshift keeps the
/// payload identical on every run while denying LZ4 an easy win, so a
/// "64 MiB value" really costs 64 MiB on disk.
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

/// Walk the whole default column family with the streaming iterator and
/// return it as a map, asserting strict ascending key order on the way.
/// Streaming keeps the walk's memory proportional to one entry plus the
/// collected result rather than to an intermediate copy of the database.
fn scan_in_order(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut previous: Option<Vec<u8>> = None;
    let mut iter = db.iter();
    iter.seek_to_first();
    while iter.valid() {
        let key = iter.key().expect("a valid iterator has a key").to_vec();
        let value = iter.value().expect("a valid iterator has a value").to_vec();
        if let Some(prev) = &previous {
            assert!(
                prev.as_slice() < key.as_slice(),
                "scan produced keys out of order: {:?} then {:?}",
                &prev[..prev.len().min(16)],
                &key[..key.len().min(16)]
            );
        }
        previous = Some(key.clone());
        out.insert(key, value);
        iter.next();
    }
    iter.status().expect("the scan must not end in an error");
    out
}

/// Poll `check` until it holds or `deadline` passes, backing off from
/// 1 ms up to 20 ms. A fast machine returns on the first poll; a slow one
/// still gets the full budget. Returns whether the condition was met.
fn wait_until(deadline: Duration, mut check: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    let mut backoff = Duration::from_millis(1);
    loop {
        if check() {
            return true;
        }
        if started.elapsed() >= deadline {
            return false;
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(20));
    }
}

fn deepest_populated_level(db: &Db) -> Option<usize> {
    (0..7).rev().find(|level| {
        db.get_int_property(&format!("regolith.num-files-at-level{level}"))
            .unwrap_or(0)
            > 0
    })
}

// ---------------------------------------------------------------------
// Degenerate inputs (fast)
// ---------------------------------------------------------------------

/// Proves a zero-length key and a zero-length value are ordinary data:
/// the pair `("", "")` survives a memtable flush, a full compaction and a
/// reopen, stays distinct from "absent", and sorts ahead of every other
/// key. Catches a length-prefix or comparator bug that treats an empty
/// byte string as a missing field, and a compaction that drops the
/// lowest key in a file.
#[test]
fn a_zero_length_key_and_a_zero_length_value_survive_flush_and_compaction() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts_with(4 * 1024)).unwrap();

    regolith::through_stalls(|| db.put(b"", b"")).unwrap();
    regolith::through_stalls(|| db.put(b"a", b"")).unwrap();
    regolith::through_stalls(|| db.put(b"b", b"nonempty")).unwrap();
    assert_eq!(db.get(b"").unwrap(), Some(Vec::new()));
    assert_eq!(db.get(b"absent").unwrap(), None);

    db.compact_range(None, None).wait().unwrap();
    assert_eq!(db.get(b"").unwrap(), Some(Vec::new()));
    assert_eq!(db.get(b"a").unwrap(), Some(Vec::new()));

    let scanned = scan_in_order(&db);
    assert_eq!(
        scanned.keys().next().map(|k| k.as_slice()),
        Some(&b""[..]),
        "the empty key must sort first"
    );
    assert_eq!(scanned.len(), 3);
    db.close().unwrap();
    drop(db);

    let db = Db::open(dir.path(), opts_with(4 * 1024)).unwrap();
    assert_eq!(db.get(b"").unwrap(), Some(Vec::new()));
    assert_eq!(db.get(b"b").unwrap(), Some(b"nonempty".to_vec()));

    regolith::through_stalls(|| db.delete(b"")).unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(
        db.get(b"").unwrap(),
        None,
        "deleting the empty key must actually hide it"
    );
    assert_eq!(scan_in_order(&db).len(), 2);
}

/// Proves an empty `WriteBatch` is a successful no-op rather than an
/// error or a phantom write: `Db::write` returns `Ok`, the visible state
/// is byte-identical afterwards, the memtable does not grow by a single
/// byte, and nothing reappears after a reopen. Catches an engine that
/// appends a zero-operation record to the WAL or inserts a placeholder
/// entry for a batch with nothing in it.
#[test]
fn a_write_batch_with_zero_operations_is_a_successful_no_op() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts_with(4 * 1024)).unwrap();
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();

    let before = scan_in_order(&db);
    let memtable_before = db.get_int_property("regolith.cur-size-all-mem-tables");
    let snapshot = db.snapshot();

    let empty = WriteBatch::new();
    assert_eq!(empty.len(), 0);
    assert!(empty.is_empty());
    regolith::through_stalls(|| db.write(empty.clone())).unwrap();

    assert_eq!(scan_in_order(&db), before, "an empty batch changed state");
    assert_eq!(
        db.get_int_property("regolith.cur-size-all-mem-tables"),
        memtable_before,
        "an empty batch put bytes into the memtable"
    );
    assert_eq!(snapshot.get(b"k").unwrap(), Some(b"v".to_vec()));

    db.close().unwrap();
    // A live `Snapshot` keeps its own `Arc` on the engine, and the engine
    // is what owns the cross-process LOCK file, so both handles have to go
    // before the directory can be reopened.
    drop(snapshot);
    drop(db);
    let db = Db::open(dir.path(), opts_with(4 * 1024)).unwrap();
    assert_eq!(
        scan_in_order(&db),
        before,
        "an empty batch left something behind in the WAL"
    );
}

/// Proves `delete_range` is exact at the two ends of its cardinality
/// range and in the middle: a range covering zero keys removes nothing, a
/// range covering exactly one key removes exactly that key, and a range
/// covering every key empties the database. All three are re-checked
/// after a full compaction, where a range tombstone stops being a
/// standalone record and has to be applied during the merge. Catches an
/// off-by-one on the half-open bound and a tombstone that widens or
/// narrows when compaction rewrites it.
#[test]
fn delete_range_covers_exactly_zero_one_and_all_keys() {
    let keys: Vec<&[u8]> = vec![b"a", b"b", b"c", b"d", b"e"];

    let fresh = |dir: &TempDir| {
        let db = Db::open(dir.path(), opts_with(4 * 1024)).unwrap();
        for key in &keys {
            regolith::through_stalls(|| db.put(key, b"v")).unwrap();
        }
        db
    };

    // Zero keys: a range that lies entirely past the last key.
    let dir = TempDir::new().unwrap();
    let db = fresh(&dir);
    regolith::through_stalls(|| db.delete_range(b"m", b"z")).unwrap();
    assert_eq!(scan_in_order(&db).len(), 5);
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(scan_in_order(&db).len(), 5, "an empty range deleted data");

    // One key: the half-open range [c, d) holds exactly "c".
    let dir = TempDir::new().unwrap();
    let db = fresh(&dir);
    regolith::through_stalls(|| db.delete_range(b"c", b"d")).unwrap();
    let after = scan_in_order(&db);
    assert_eq!(after.len(), 4);
    assert!(!after.contains_key(b"c".as_slice()));
    assert!(
        after.contains_key(b"d".as_slice()),
        "the exclusive upper bound must survive"
    );
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(scan_in_order(&db), after);

    // All keys: an unbounded-below range up past the last key.
    let dir = TempDir::new().unwrap();
    let db = fresh(&dir);
    regolith::through_stalls(|| db.delete_range(b"", b"\xff")).unwrap();
    assert!(scan_in_order(&db).is_empty());
    db.compact_range(None, None).wait().unwrap();
    assert!(scan_in_order(&db).is_empty());
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), opts_with(4 * 1024)).unwrap();
    assert!(
        scan_in_order(&db).is_empty(),
        "a full-range delete did not survive reopen"
    );
}

// ---------------------------------------------------------------------
// Size limits (fast)
// ---------------------------------------------------------------------

/// Proves `max_value_size` is enforced at exactly the configured byte and
/// identically on the point path and the batch path. A value of exactly
/// `max_value_size` is accepted and reads back byte-for-byte; one byte
/// more is rejected with `InvalidArgument` naming the limit, on `put`,
/// `put_opt` and `WriteBatch`. Catches a `<` written where `<=` belongs,
/// and the batch path drifting from the point path.
#[test]
fn the_value_size_limit_is_exact_on_the_point_and_batch_paths() {
    const LIMIT: usize = 4096;
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .max_value_size(LIMIT)
            .write_buffer_size(64 * 1024),
    )
    .unwrap();

    let at_limit = seeded_bytes(0x5123_A11E, LIMIT);
    let over_limit = seeded_bytes(0x5123_A11E, LIMIT + 1);

    regolith::through_stalls(|| db.put(b"exact", &at_limit)).unwrap();
    assert_eq!(db.get(b"exact").unwrap(), Some(at_limit.clone()));

    expect_invalid_argument(db.put(b"over", &over_limit), "max_value_size");
    expect_invalid_argument(
        db.put_opt(&regolith::WriteOptions::sync(), b"over", &over_limit),
        "max_value_size",
    );

    let mut batch = WriteBatch::new();
    batch.put(b"batch_exact", &at_limit);
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();
    assert_eq!(db.get(b"batch_exact").unwrap(), Some(at_limit));

    let mut batch = WriteBatch::new();
    batch.put(b"batch_over", &over_limit);
    expect_invalid_argument(db.write(batch), "max_value_size");
    assert_eq!(
        db.get(b"batch_over").unwrap(),
        None,
        "a rejected batch must not apply any of its operations"
    );
}

/// Proves `max_key_size` is enforced at exactly the configured byte on
/// every API that takes a key: `put`, `delete`, `delete_range` (both
/// bounds), `compact_range`, and the `WriteBatch` equivalents, which
/// measure a key that already carries a 4-byte column-family prefix.
/// Catches the batch path forgetting to subtract that prefix, which would
/// silently make the effective limit four bytes smaller there than on
/// `Db::put`.
#[test]
fn the_key_size_limit_is_exact_on_the_point_and_batch_paths() {
    const LIMIT: usize = 64;
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .max_key_size(LIMIT)
            .write_buffer_size(64 * 1024),
    )
    .unwrap();

    let at_limit = vec![b'k'; LIMIT];
    let over_limit = vec![b'k'; LIMIT + 1];

    regolith::through_stalls(|| db.put(&at_limit, b"v")).unwrap();
    assert_eq!(db.get(&at_limit).unwrap(), Some(b"v".to_vec()));
    regolith::through_stalls(|| db.delete(&at_limit)).unwrap();
    regolith::through_stalls(|| db.put(&at_limit, b"v")).unwrap();
    db.compact_range(Some(&at_limit), None).wait().unwrap();

    expect_invalid_argument(db.put(&over_limit, b"v"), "max_key_size");
    expect_invalid_argument(db.delete(&over_limit), "max_key_size");
    expect_invalid_argument(db.delete_range(&over_limit, b"\xff"), "max_key_size");
    expect_invalid_argument(db.delete_range(b"", &over_limit), "max_key_size");
    expect_invalid_argument(
        db.compact_range(Some(&over_limit), None).wait(),
        "max_key_size",
    );

    let mut batch = WriteBatch::new();
    batch.put(&at_limit, b"batched");
    batch.delete_range(&at_limit, b"\xff");
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();

    let mut batch = WriteBatch::new();
    batch.put(&over_limit, b"v");
    expect_invalid_argument(db.write(batch), "max_key_size");

    let mut batch = WriteBatch::new();
    batch.delete(&over_limit);
    expect_invalid_argument(db.write(batch), "max_key_size");

    let mut batch = WriteBatch::new();
    batch.delete_range(&over_limit, b"\xff");
    expect_invalid_argument(db.write(batch), "max_key_size");
    assert_eq!(
        db.get(&at_limit).unwrap(),
        None,
        "the accepted batch's own range delete should have removed the key"
    );
}

/// Proves the documented defaults are the limits that actually ship:
/// 64 MiB + 1 for a value and 8 MiB + 1 for a key are rejected by a
/// database opened with `Options::default()`. This costs one allocation
/// and no I/O, and guards the constants in `src/options.rs` against
/// drifting away from the doc comment.
#[test]
fn one_byte_over_the_default_limits_is_rejected() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();

    let over_value = vec![0u8; regolith::DEFAULT_MAX_VALUE_SIZE + 1];
    expect_invalid_argument(db.put(b"k", &over_value), "max_value_size");
    drop(over_value);

    let over_key = vec![b'k'; regolith::DEFAULT_MAX_KEY_SIZE + 1];
    expect_invalid_argument(db.put(&over_key, b"v"), "max_key_size");
}

// ---------------------------------------------------------------------
// Extremes (`just test-extremes` runs them one at a time)
// ---------------------------------------------------------------------

/// Proves the default maxima are usable, not just declared: a value of
/// exactly `DEFAULT_MAX_VALUE_SIZE` (64 MiB) and a key of exactly
/// `DEFAULT_MAX_KEY_SIZE` (8 MiB) round-trip through the memtable, a
/// flush, a compaction and a reopen with every byte intact. Catches a
/// block, index or WAL frame whose own length field is narrower than the
/// limit the API advertises. Measured runtime: 12.7 s, peak RSS 497 MiB.
#[test]
fn the_default_size_maxima_round_trip_at_exactly_the_limit() {
    measured("64 MiB value + 8 MiB key at the limit", || {
        let dir = TempDir::new().unwrap();
        let value = seeded_bytes(0x0BAD_F00D, regolith::DEFAULT_MAX_VALUE_SIZE);
        let key = {
            let mut k = seeded_bytes(0x00C0_FFEE, regolith::DEFAULT_MAX_KEY_SIZE);
            k[0] = b'z';
            k
        };

        let db = Db::open(dir.path(), Options::default()).unwrap();
        regolith::through_stalls(|| db.put(b"biggest", &value)).unwrap();
        regolith::through_stalls(|| db.put(&key, b"long key")).unwrap();
        assert_eq!(db.get(b"biggest").unwrap().as_ref(), Some(&value));
        assert_eq!(db.get(&key).unwrap(), Some(b"long key".to_vec()));

        db.compact_range(None, None).wait().unwrap();
        assert_eq!(db.get(b"biggest").unwrap().as_ref(), Some(&value));
        assert_eq!(db.get(&key).unwrap(), Some(b"long key".to_vec()));
        db.close().unwrap();
        drop(db);

        let db = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(
            db.get(b"biggest").unwrap().as_ref(),
            Some(&value),
            "a value at exactly max_value_size did not survive reopen"
        );
        assert_eq!(db.get(&key).unwrap(), Some(b"long key".to_vec()));
    });
}

/// Proves version accumulation is bounded by compaction, not by the
/// number of writes: after 100 000 overwrites of one key, a full
/// compaction leaves exactly one live entry, the latest value, and the
/// on-disk footprint collapses to a single small SSTable rather than
/// holding 100 000 shadowed versions. Catches a merge that keeps every
/// version because no snapshot pins them, which would turn a hot counter
/// key into unbounded disk growth. Measured runtime: 0.8 s, peak RSS 27 MiB.
#[test]
fn one_key_overwritten_100_000_times_collapses_to_a_single_version() {
    measured("100 000 overwrites of one key", || {
        const WRITES: usize = 100_000;
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts_with(64 * 1024)).unwrap();

        for i in 0..WRITES {
            regolith::through_stalls(|| db.put(b"hot", format!("v{i:07}").as_bytes())).unwrap();
        }
        let last = format!("v{:07}", WRITES - 1).into_bytes();
        assert_eq!(db.get(b"hot").unwrap(), Some(last.clone()));

        db.compact_range(None, None).wait().unwrap();

        let live = scan_in_order(&db);
        assert_eq!(live.len(), 1, "compaction kept more than the live version");
        assert_eq!(live.get(b"hot".as_slice()), Some(&last));

        let on_disk = db
            .get_int_property("regolith.total-sst-files-size")
            .expect("regolith.total-sst-files-size is a supported property");
        assert!(
            on_disk < 64 * 1024,
            "one live 11-byte value occupies {on_disk} bytes on disk; \
             shadowed versions were not collapsed"
        );

        db.close().unwrap();
        drop(db);
        let db = Db::open(dir.path(), opts_with(64 * 1024)).unwrap();
        assert_eq!(db.get(b"hot").unwrap(), Some(last));
        assert_eq!(scan_in_order(&db).len(), 1);
    });
}

/// Proves a six-figure key space stays fully ordered and fully readable:
/// 100 000 distinct keys spread across dozens of L0 flushes and several
/// compactions, then one streaming forward scan that must yield every key
/// exactly once in strictly ascending order with the right value.
/// Catches a merge iterator that drops or duplicates an entry at a file
/// boundary, which a small-N scan test would never reach. Measured
/// runtime: 3.8 s, peak RSS 27 MiB.
#[test]
fn one_hundred_thousand_distinct_keys_scan_in_order() {
    measured("100 000 distinct keys, full ordered scan", || {
        const KEYS: usize = 100_000;
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts_with(256 * 1024)).unwrap();

        for i in 0..KEYS {
            regolith::through_stalls(|| {
                db.put(
                    format!("key_{i:07}").as_bytes(),
                    format!("value_{i:07}").as_bytes(),
                )
            })
            .unwrap();
        }

        let mut seen = 0usize;
        let mut previous: Option<Vec<u8>> = None;
        let mut iter = db.iter();
        iter.seek_to_first();
        while iter.valid() {
            let key = iter.key().unwrap().to_vec();
            let value = iter.value().unwrap().to_vec();
            assert_eq!(
                value,
                format!("value_{seen:07}").into_bytes(),
                "entry {seen} carries the wrong value"
            );
            assert_eq!(key, format!("key_{seen:07}").into_bytes());
            if let Some(prev) = &previous {
                assert!(prev < &key, "scan went backwards at entry {seen}");
            }
            previous = Some(key);
            seen += 1;
            iter.next();
        }
        iter.status().unwrap();
        assert_eq!(seen, KEYS, "the scan lost or invented keys");

        db.compact_range(None, None).wait().unwrap();
        assert_eq!(scan_in_order(&db).len(), KEYS);
    });
}

/// Proves a 100 000-operation `WriteBatch` is applied atomically and in
/// full: nothing is visible before the write returns, everything is
/// visible after, the interleaved deletes inside the batch win over the
/// earlier puts on the same key, and the whole thing survives a reopen.
/// Catches a batch path that chunks internally and loses atomicity past
/// some size, and a WAL record framing that cannot carry a batch this
/// large. Measured runtime: 1.9 s, peak RSS 52 MiB.
#[test]
fn a_write_batch_of_100_000_operations_applies_atomically() {
    measured("100 000-operation WriteBatch", || {
        const OPS: usize = 100_000;
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts_with(256 * 1024)).unwrap();
        regolith::through_stalls(|| db.put(b"sentinel", b"before")).unwrap();

        let before = db.snapshot();

        let mut batch = WriteBatch::new();
        for i in 0..OPS {
            batch.put(format!("b_{i:07}").as_bytes(), b"batched");
        }
        // Every tenth key is deleted inside the same batch: the later
        // operation on a key must win.
        for i in (0..OPS).step_by(10) {
            batch.delete(format!("b_{i:07}").as_bytes());
        }
        assert_eq!(batch.len(), OPS + OPS.div_ceil(10));
        regolith::through_stalls(|| db.write(batch.clone())).unwrap();

        assert_eq!(
            before.get(b"b_0000001").unwrap(),
            None,
            "a snapshot taken before the batch observed part of it"
        );

        let expected_live = OPS - OPS.div_ceil(10);
        let live = scan_in_order(&db);
        assert_eq!(
            live.len(),
            expected_live + 1,
            "sentinel plus surviving keys"
        );
        assert_eq!(live.get(b"b_0000000".as_slice()), None);
        assert_eq!(
            live.get(b"b_0000001".as_slice()),
            Some(&b"batched".to_vec())
        );

        db.close().unwrap();
        drop(before);
        drop(db);
        let db = Db::open(dir.path(), opts_with(256 * 1024)).unwrap();
        assert_eq!(
            scan_in_order(&db).len(),
            expected_live + 1,
            "the batch did not survive reopen intact"
        );
    });
}

/// Proves the block format's prefix compression and the internal-key
/// comparator hold up when key length varies by six orders of magnitude
/// in one file: megabyte keys sharing a megabyte-long common prefix,
/// interleaved in sort order with keys a dozen bytes long. Every key must
/// still be found by point lookup, appear exactly once in the ordered
/// scan, and be reachable by `seek` and `seek_for_prev`, before and after
/// a compaction that rewrites every block, and after a reopen. Catches a
/// shared-prefix length that overflows its varint, a restart-point
/// interval that assumes short keys, and a comparator that stops at a
/// fixed prefix. Measured runtime: 1.8 s, peak RSS 99 MiB.
#[test]
fn megabyte_keys_mixed_with_short_keys_survive_prefix_compression() {
    measured("1 MiB keys mixed with short keys", || {
        const MIB: usize = 1024 * 1024;
        let filler = vec![b'x'; MIB];
        let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();

        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts_with(4 * MIB)).unwrap();

        // Alternating long and short keys: "pNN_short" sorts just ahead
        // of "pNN_xxxx..." so the block builder repeatedly follows a
        // 12-byte key with a megabyte one and back again.
        for i in 0..6u32 {
            let mut long = format!("p{i:02}_").into_bytes();
            long.extend_from_slice(&filler);
            let short = format!("p{i:02}_short").into_bytes();
            expected.insert(long.clone(), format!("long{i}").into_bytes());
            expected.insert(short.clone(), format!("short{i}").into_bytes());
            regolith::through_stalls(|| db.put(&long, format!("long{i}").as_bytes())).unwrap();
            regolith::through_stalls(|| db.put(&short, format!("short{i}").as_bytes())).unwrap();
        }

        // Four keys that share a full megabyte of prefix and differ only
        // in their last four bytes.
        for j in 0..4u32 {
            let mut key = b"q_".to_vec();
            key.extend_from_slice(&filler);
            key.extend_from_slice(format!("{j:04}").as_bytes());
            expected.insert(key.clone(), format!("shared{j}").into_bytes());
            regolith::through_stalls(|| db.put(&key, format!("shared{j}").as_bytes())).unwrap();
        }

        let verify = |db: &Db, stage: &str| {
            for (key, value) in &expected {
                assert_eq!(
                    db.get(key).unwrap().as_ref(),
                    Some(value),
                    "{stage}: a {}-byte key lost its value",
                    key.len()
                );
            }
            assert_eq!(scan_in_order(db), expected, "{stage}: scan mismatch");

            let target = expected.keys().last().unwrap();
            let mut iter = db.iter();
            iter.seek(target);
            assert_eq!(iter.key(), Some(target.as_slice()), "{stage}: seek missed");
            iter.seek_for_prev(target);
            assert_eq!(
                iter.key(),
                Some(target.as_slice()),
                "{stage}: seek_for_prev missed"
            );
        };

        verify(&db, "in memory");
        db.compact_range(None, None).wait().unwrap();
        verify(&db, "after compaction");
        db.close().unwrap();
        drop(db);

        let db = Db::open(dir.path(), opts_with(4 * MIB)).unwrap();
        verify(&db, "after reopen");
    });
}

/// Proves reads still find every key once the tree is genuinely deep, and
/// that they find it across several populated levels at once rather than
/// in one flat run.
///
/// The level targets are shrunk to 1, 2, 4, 8 and 16 KiB for L1..L5, so
/// their combined capacity (31 KiB) cannot hold the roughly 80 KiB the
/// workload compresses to. The overflow has nowhere to go but L6, and it
/// can only get there by being rewritten through L1, L2, L3, L4 and L5 in
/// turn, so waiting for a file at L6 is a proof of depth rather than a
/// guess. The full key set is verified while compaction is still in
/// flight and again once the tree settles. Catches a level-aware read
/// path that stops searching above the bottommost level and a compaction
/// that drops entries as it promotes them. Measured runtime: 0.3 s, peak
/// RSS 8 MiB.
#[test]
fn a_deep_level_structure_still_answers_every_read() {
    measured("cascade to L6", || {
        const KEYS: usize = 6_000;
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(8 * 1024)
            .block_size(1024)
            .target_file_size(8 * 1024)
            .level_base_bytes(1024)
            .level_size_multiplier(2)
            .l0_compaction_trigger(2);
        let db = Db::open(dir.path(), opts.clone()).unwrap();

        let mut expected = BTreeMap::new();
        for i in 0..KEYS {
            let key = format!("k{i:06}").into_bytes();
            let value = format!("value_for_{i:06}").into_bytes();
            // Through stalls: a busy host's compaction worker falls behind.
            regolith::through_stalls(|| db.put(&key, &value)).unwrap();
            expected.insert(key, value);
        }

        // Reads must stay correct while the tree is still being rewritten
        // underneath them.
        assert_eq!(
            scan_in_order(&db),
            expected,
            "a scan during background compaction lost data"
        );

        let deep = wait_until(Duration::from_secs(120), || {
            deepest_populated_level(&db).is_some_and(|level| level >= 6)
        });
        assert!(
            deep,
            "the tree never reached L6 within 120 s; levels now:\n{}",
            db.get_property("regolith.levelstats").unwrap_or_default()
        );

        let levels: Vec<(usize, u64)> = (0..7)
            .map(|l| {
                (
                    l,
                    db.get_int_property(&format!("regolith.num-files-at-level{l}"))
                        .unwrap_or(0),
                )
            })
            .filter(|(_, n)| *n > 0)
            .collect();
        println!("[resource_limits] populated levels (level, files): {levels:?}");
        assert!(
            deepest_populated_level(&db).is_some_and(|level| level >= 3),
            "the deepest populated level regressed above L3"
        );
        assert!(
            levels.iter().filter(|(l, _)| *l >= 1).count() >= 3,
            "the reads never had to cross a nested tree: only {levels:?} are populated"
        );

        for (key, value) in &expected {
            assert_eq!(db.get(key).unwrap().as_ref(), Some(value));
        }
        assert_eq!(scan_in_order(&db), expected);

        db.close().unwrap();
        drop(db);
        let db = Db::open(dir.path(), opts).unwrap();
        assert_eq!(
            scan_in_order(&db),
            expected,
            "the deep tree did not survive reopen"
        );
    });
}

/// The out-of-space half of this suite, kept in its own file because it
/// carries a subprocess harness the rest of the tests do not need. The
/// `#[path]` attribute is what keeps `tests/resource_limits/` out of
/// cargo's test-target autodiscovery, which only claims `tests/*.rs` and
/// `tests/*/main.rs`.
#[path = "resource_limits/enospc.rs"]
mod enospc;
