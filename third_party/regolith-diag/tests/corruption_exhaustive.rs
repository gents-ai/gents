//! Exhaustive on-disk corruption: truncation at every offset, bit rot in
//! every region, torn record headers, and structurally impossible table
//! sets.
//!
//! `tests/corruption.rs` and `tests/open_and_corruption.rs` already poke a
//! handful of hand-picked offsets. This file differs from them in two
//! ways that matter.
//!
//! **Coverage.** Every byte offset of the WAL, of an SSTable and of the
//! MANIFEST is truncated, and every bit of every byte is flipped, by the
//! exhaustive sweeps. The default run takes a seeded, evenly spread
//! sample of the same offsets so `cargo test` stays fast. The sample is
//! deterministic, so an offset that fails once fails every time.
//!
//! **Correctness, not merely liveness.** "did not panic" is a weak bar:
//! an engine that silently serves half a table clears it. Every trial
//! here reads the whole database back and requires one of exactly two
//! outcomes:
//!
//! * a loud refusal or a loud read error, or
//! * data that matches what was actually written.
//!
//! Wrong values, invented keys, a forward scan that disagrees with a
//! point lookup, a reverse scan that disagrees with the forward one, and
//! a scan that stops early while `status()` reports success are all
//! failures, not passes. Those are the ways an LSM engine returns
//! silently wrong data, and none of them are visible to a test that only
//! checks for a panic.
//!
//! A panic raised on a *background* thread would be invisible to the test
//! harness, so `harness::watch_engine_panics` installs a hook that records
//! any panic originating outside `tests/`, and every sweep asserts that none
//! were recorded. A hang would be equally invisible, so every test body
//! runs on a worker thread whose progress counter is watched from the
//! test thread with a deadline plus bounded backoff.
#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::panic;
use std::sync::atomic::{AtomicU64, Ordering};

use regolith::Db;
use tempfile::TempDir;

mod common;
// The crate root of an integration test sits in `tests/`, so the usual
// `file.rs` + `file/` nesting needs an explicit path.
#[path = "corruption_exhaustive/harness.rs"]
mod harness;

use common::fault::{
    builtin_workload, child_entrypoint, flip_bit, garbage, overwrite_range, truncate_at,
};
// Used only by the WAL sweeps, which are linux-only.
#[cfg(target_os = "linux")]
use common::fault::validate_prefix_of_state;
use harness::{
    BLOOM, DATA, FOOTER, INDEX, Recovered, SAMPLE, SEED, TAG_ADD_FILE, Tally,
    assert_engine_never_panicked, every, exactly, manifest_frames, never_invents, read_state,
    region, sample, sst_regions, table_fixture, trial, watch,
};
// The WAL fixtures are built by killing a child process, so they exist
// only where "was it killed" has an answer. The SSTable and MANIFEST
// sweeps below use `table_fixture`, which spawns nothing, and run
// everywhere.
#[cfg(target_os = "linux")]
use harness::{batch_fixture, valid_prefix, wal_fixture, wal_frames};

// ─── WAL: truncation ────────────────────────────────────────────────────

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
/// Truncating the WAL must leave the database holding the state after
/// some whole number of the intended writes. A cut leaves every record
/// before it byte-for-byte as the process wrote it and only the record it
/// lands in incomplete, which is the shape of every crash, so the exact
/// boundary is asserted rather than merely the absence of a panic: every
/// cut must open and serve exactly the records that survived whole.
/// Catches a replay that swallows a torn tail as if it were data, one
/// that drops an intact earlier record, and one that refuses the whole
/// log because of the incomplete record at the end of it.
fn wal_truncation_sweep(cuts: &[u64], what: &str, progress: &AtomicU64) {
    let fixture = wal_fixture();
    let rel = fixture.only(".log");
    let frames = wal_frames(fixture.bytes(&rel));
    let boundaries: Vec<u64> = std::iter::once(0)
        .chain(frames.iter().map(|f| f.end))
        .collect();
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut tally = Tally::default();
    assert_eq!(
        frames.len(),
        fixture.history.len(),
        "this sweep maps one WAL record to one intended write; the fixture no longer does",
    );

    for &cut in cuts {
        progress.fetch_add(1, Ordering::Relaxed);
        let label = format!("WAL truncated to {cut} of {}", fixture.bytes(&rel).len());
        let outcome = trial(fixture, &db, |db| truncate_at(&db.join(&rel), cut));
        let records = boundaries.iter().filter(|b| **b <= cut).count() - 1;
        match &outcome {
            Recovered::Opened(state) => match validate_prefix_of_state(state, &fixture.history) {
                Ok(report) if report.valid_ks.contains(&records) => tally.matched += 1,
                Ok(report) => tally.violation(format!(
                    "{label}: {records} whole record(s) survived the cut but the state \
                     matches prefix lengths {:?}",
                    report.valid_ks
                )),
                Err(e) => tally.violation(format!("{label}: {e}")),
            },
            other => tally.violation(format!(
                "{label}: {records} whole record(s) survived the cut and every one of them is \
                 intact, so open must serve exactly those, but the database {}",
                other.why()
            )),
        }
    }
    tally.finish(what);
}

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
#[test]
fn a_wal_truncated_at_a_sampled_offset_replays_whole_records_or_refuses() {
    watch("wal truncation sample", |progress| {
        let fixture = wal_fixture();
        let len = fixture.bytes(&fixture.only(".log")).len() as u64;
        wal_truncation_sweep(
            &sample(0, len, SAMPLE, SEED),
            "wal truncation sample",
            progress,
        );
    });
}

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
/// The exhaustive twin of the sampled sweep: every byte offset of the
/// WAL, with no sampling to hide behind.
#[test]
fn a_wal_truncated_at_every_offset_replays_whole_records_or_refuses() {
    watch("wal truncation exhaustive", |progress| {
        let fixture = wal_fixture();
        let len = fixture.bytes(&fixture.only(".log")).len() as u64;
        wal_truncation_sweep(&every(0, len), "wal truncation exhaustive", progress);
    });
}

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
/// A `WriteBatch` is atomic, so a WAL cut inside the record that carries
/// one must leave none of it applied, in every durability mode. Cutting
/// at every offset inside the final batch record catches a replay that
/// applies the operations it managed to parse before hitting the damage.
#[test]
fn a_write_batch_record_cut_in_half_is_never_half_applied() {
    watch("wal batch atomicity", |progress| {
        let fixture = batch_fixture();
        let rel = fixture.only(".log");
        let frames = wal_frames(fixture.bytes(&rel));
        let last = *frames.last().expect("at least one record");
        assert!(
            frames.len() >= 2,
            "the batch fixture must hold more than one record",
        );
        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        let mut tally = Tally::default();
        let expect = valid_prefix(&fixture.history);
        for cut in last.start + 1..last.end {
            progress.fetch_add(1, Ordering::Relaxed);
            let outcome = trial(fixture, &db, |db| truncate_at(&db.join(&rel), cut));
            tally.record(&format!("batch record cut at {cut}"), outcome, &expect);
        }
        tally.finish("wal batch atomicity");
    });
}

// ─── WAL: bit rot ───────────────────────────────────────────────────────

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
/// Every byte of a WAL record is covered by a check: the header check
/// covers the header, and the payload check, which the header check
/// covers, covers the payload. So no flipped record may ever be replayed
/// as data. Every record but the last is followed by one that proves it
/// synced (the fixture writes at `Immediate` durability), so a flip there
/// must refuse.
///
/// The *last* record is the one exception, and it is inherent to the
/// format rather than a hole: nothing after it proves it was synced, so
/// damage to it cannot be told from the record a crash tore, which replay
/// is required to drop. Either a refusal or an open serving exactly the
/// records before that last one is correct there, and the drop is
/// reported. Nothing else is: the blast radius stays that one record.
fn wal_flip_sweep(positions: &[u64], what: &str, progress: &AtomicU64) {
    let fixture = wal_fixture();
    let rel = fixture.only(".log");
    let frames = wal_frames(fixture.bytes(&rel));
    let last = *frames.last().expect("at least one record");
    let ambiguous = last.start..last.end;
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut tally = Tally::default();
    assert_eq!(
        frames.len(),
        fixture.history.len(),
        "this sweep maps one WAL record to one intended write; the fixture no longer does",
    );

    for &offset in positions {
        for bit in 0..8u8 {
            progress.fetch_add(1, Ordering::Relaxed);
            let label = format!("WAL byte {offset} bit {bit}");
            let outcome = trial(fixture, &db, |db| flip_bit(&db.join(&rel), offset, bit));
            match (&outcome, ambiguous.contains(&offset)) {
                (Recovered::Refused(_), _) => tally.refused += 1,
                (Recovered::Opened(state), true) => {
                    let before_last = frames.len() - 1;
                    match validate_prefix_of_state(state, &fixture.history) {
                        Ok(report) if report.valid_ks.contains(&before_last) => tally.matched += 1,
                        Ok(report) => tally.violation(format!(
                            "{label}: the flip is in the last record, so at most that record \
                             may be lost, but the state matches prefix lengths {:?} rather than \
                             {before_last}",
                            report.valid_ks
                        )),
                        Err(e) => tally.violation(format!("{label}: {e}")),
                    }
                }
                (other, _) => tally.violation(format!(
                    "{label}: the record checksum did not catch the flip, the database {}",
                    other.why()
                )),
            }
        }
    }
    tally.finish(what);
}

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
#[test]
fn a_sampled_bit_flip_anywhere_in_the_wal_is_caught_by_the_record_checksum() {
    watch("wal bit flips sample", |progress| {
        let fixture = wal_fixture();
        let len = fixture.bytes(&fixture.only(".log")).len() as u64;
        wal_flip_sweep(
            &sample(0, len, SAMPLE, SEED),
            "wal bit flips sample",
            progress,
        );
    });
}

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
/// The exhaustive twin: every bit of every byte of the WAL.
#[test]
fn every_bit_flip_in_the_wal_is_caught_by_the_record_checksum() {
    watch("wal bit flips exhaustive", |progress| {
        let fixture = wal_fixture();
        let len = fixture.bytes(&fixture.only(".log")).len() as u64;
        wal_flip_sweep(&every(0, len), "wal bit flips exhaustive", progress);
    });
}

// ─── WAL: torn and trailing bytes ───────────────────────────────────────

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
/// A record header that promises more payload than the file holds is the
/// signature of a write torn by a crash, and what replay owes depends on
/// where it sits.
///
/// In the middle of the log it is not a torn write at all: whole records
/// follow it, so the length field itself was damaged and discarding the
/// rest would lose records that are intact. Replay must refuse and leave
/// the WAL on disk for repair.
///
/// On the last record it is the ordinary end of a crashed log, and
/// refusing there would throw away every acknowledged write before it.
/// Replay must serve exactly the records ahead of it instead.
///
/// Catches a replay that reads past the record, one that quietly treats
/// the middle of the log as absent, and one that turns the cheapest
/// failure there is into total data loss.
#[test]
fn a_wal_record_header_promising_more_bytes_than_exist_is_refused_unless_it_is_the_tail() {
    watch("wal torn header", |progress| {
        let fixture = wal_fixture();
        let rel = fixture.only(".log");
        let bytes = fixture.bytes(&rel).to_vec();
        let frames = wal_frames(&bytes);
        let last = *frames.last().expect("non-empty");
        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        let mut tally = Tally::default();
        assert!(
            frames.len() >= 2,
            "the two placements this test contrasts need more than one record",
        );
        assert_eq!(
            frames.len(),
            fixture.history.len(),
            "this test maps one WAL record to one intended write; the fixture no longer does",
        );

        for frame in [frames[0], last] {
            let is_tail = frame.start == last.start;
            let real = (frame.end - frame.start) as u32 - common::wal_format::HEADER_LEN as u32;
            for extra in [1u32, 7, 64, 1 << 20, u32::MAX - real] {
                progress.fetch_add(1, Ordering::Relaxed);
                let claimed = real + extra;
                let label = format!(
                    "record at {} claiming {claimed} bytes instead of {real}",
                    frame.start
                );
                let outcome = trial(fixture, &db, |db| {
                    overwrite_range(&db.join(&rel), frame.start, &claimed.to_le_bytes())
                });
                let kept = fs::metadata(db.join(&rel)).map(|m| m.len()).unwrap_or(0);
                match (&outcome, is_tail) {
                    (Recovered::Refused(_), _) => {
                        assert_eq!(
                            kept,
                            bytes.len() as u64,
                            "a refused open must leave the WAL untouched for repair",
                        );
                        tally.refused += 1;
                    }
                    (Recovered::Opened(state), true) => {
                        let before_last = frames.len() - 1;
                        match validate_prefix_of_state(state, &fixture.history) {
                            Ok(report) if report.valid_ks.contains(&before_last) => {
                                tally.matched += 1
                            }
                            Ok(report) => tally.violation(format!(
                                "{label}: the damaged record is the last one, so the \
                                 {before_last} before it are intact, but the state matches \
                                 prefix lengths {:?}",
                                report.valid_ks
                            )),
                            Err(e) => tally.violation(format!("{label}: {e}")),
                        }
                    }
                    (other, _) => tally.violation(format!("{label}: {}", other.why())),
                }
            }
        }
        tally.finish("wal torn header");
    });
}

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
/// Garbage appended after the last valid record is what a crash mid-write
/// plus a filesystem that pads with whatever was in the block leaves
/// behind. It must never be replayed as data. Catches a replay loop that
/// trusts a length field it has not checksummed.
#[test]
fn garbage_appended_after_a_valid_wal_is_never_replayed_as_data() {
    watch("wal trailing garbage", |progress| {
        let fixture = wal_fixture();
        let rel = fixture.only(".log");
        let len = fixture.bytes(&rel).len() as u64;
        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        let mut tally = Tally::default();
        let expect = valid_prefix(&fixture.history);
        for n in [1usize, 2, 4, 5, 8, 9, 16, 33, 64, 257] {
            progress.fetch_add(1, Ordering::Relaxed);
            let junk = garbage(SEED ^ n as u64, n);
            let outcome = trial(fixture, &db, |db| {
                overwrite_range(&db.join(&rel), len, &junk)
            });
            tally.record(&format!("{n} garbage bytes appended"), outcome, &expect);
        }
        tally.finish("wal trailing garbage");
    });
}

// The fixture this uses is built by killing a child process, and
// "was it killed" is a POSIX signal question that Windows cannot
// answer, so the assertion would be vacuous there.
#[cfg(target_os = "linux")]
/// A WAL truncated to nothing is a legal state: the writes it held are
/// lost, but the database must open, serve whatever the tables hold and
/// keep working. Catches an open path that treats a zero-length WAL as
/// corruption, and one that leaves the reopened database unable to write.
#[test]
fn a_zero_length_wal_opens_and_the_database_stays_usable() {
    watch("zero-length wal", |_progress| {
        let fixture = wal_fixture();
        let rel = fixture.only(".log");
        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        fixture.plant(&db);
        truncate_at(&db.join(&rel), 0);

        let handle = Db::open(&db, fixture.opts()).expect("a zero-length WAL must still open");
        assert!(
            read_state(&handle).map(|s| s.is_empty()).unwrap_or(false),
            "every write lived in the WAL, so an emptied WAL must leave no data",
        );
        handle
            .put(b"after", b"recovery")
            .expect("put after recovery");
        assert_eq!(
            handle.get(b"after").expect("get after recovery"),
            Some(b"recovery".to_vec()),
        );
        handle.close().expect("close");
        assert_engine_never_panicked("zero-length wal");
    });
}

// ─── SSTable: truncation ────────────────────────────────────────────────

/// An SSTable carries its footer at the end of the file, so any
/// truncation destroys the only map of the file. The engine must refuse
/// to open, name the file, and leave every table on disk: a truncated
/// table is repairable, and an open that silently drops it is not.
/// Catches an open path that treats an unreadable table as an empty one.
fn sst_truncation_sweep(cuts: &[u64], what: &str, progress: &AtomicU64) {
    let fixture = table_fixture();
    let rel = fixture.only(".sst");
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut tally = Tally::default();
    for &cut in cuts {
        progress.fetch_add(1, Ordering::Relaxed);
        let outcome = trial(fixture, &db, |db| truncate_at(&db.join(&rel), cut));
        match outcome {
            Recovered::Refused(e) => {
                assert!(
                    db.join(&rel).exists(),
                    "cut at {cut}: a refused open must leave the table on disk",
                );
                assert!(
                    !e.trim().is_empty(),
                    "cut at {cut}: the refusal must say something actionable",
                );
                tally.refused += 1;
            }
            other => tally.violation(format!(
                "table truncated to {cut} of {}: {}",
                fixture.bytes(&rel).len(),
                other.why()
            )),
        }
    }
    tally.finish(what);
}

#[test]
fn an_sstable_truncated_at_a_sampled_offset_refuses_to_open_and_keeps_the_file() {
    watch("sst truncation sample", |progress| {
        let fixture = table_fixture();
        let len = fixture.bytes(&fixture.only(".sst")).len() as u64;
        sst_truncation_sweep(
            &sample(0, len, SAMPLE, SEED),
            "sst truncation sample",
            progress,
        );
    });
}

/// The exhaustive twin: every byte offset of the table.
#[test]
fn an_sstable_truncated_at_every_offset_refuses_to_open_and_keeps_the_file() {
    watch("sst truncation exhaustive", |progress| {
        let fixture = table_fixture();
        let len = fixture.bytes(&fixture.only(".sst")).len() as u64;
        sst_truncation_sweep(&every(0, len), "sst truncation exhaustive", progress);
    });
}

/// A table file that a crash left at zero bytes still has a MANIFEST
/// entry pointing at it. The open must fail loudly and name the path, so
/// an operator knows which file to restore. Catches an open that reports
/// a bare "invalid data" with no way to act on it.
#[test]
fn a_zero_length_sstable_refuses_to_open_and_names_the_file() {
    watch("zero-length sst", |_progress| {
        let fixture = table_fixture();
        let rel = fixture.only(".sst");
        let leaf = rel.rsplit('/').next().expect("a file name").to_string();
        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        fixture.plant(&db);
        truncate_at(&db.join(&rel), 0);
        match Db::open(&db, fixture.opts()) {
            Ok(_) => panic!("a zero-length table must not open"),
            Err(e) => assert!(
                e.to_string().contains(&leaf),
                "the error must name {leaf}, got: {e}",
            ),
        }
        assert_engine_never_panicked("zero-length sst");
    });
}

// ─── SSTable: bit rot, region by region ─────────────────────────────────

/// Flip every bit of the sampled positions in one region and require each
/// trial to be caught or to be provably harmless. "Harmless" means the
/// database still reads back byte-for-byte what was written; anything
/// else is silently wrong data.
fn sst_region_sweep(name: &'static str, positions: &[u64], what: &str, progress: &AtomicU64) {
    let fixture = table_fixture();
    let rel = fixture.only(".sst");
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut tally = Tally::default();
    let expect = exactly(&fixture.state);
    for &offset in positions {
        for bit in 0..8u8 {
            progress.fetch_add(1, Ordering::Relaxed);
            let outcome = trial(fixture, &db, |db| flip_bit(&db.join(&rel), offset, bit));
            tally.record(
                &format!("{name}: byte {offset} bit {bit} flipped"),
                outcome,
                &expect,
            );
        }
    }
    tally.finish(what);
}

fn sst_region_positions(name: &'static str, count: usize) -> Vec<u64> {
    let fixture = table_fixture();
    let r = region(fixture.bytes(&fixture.only(".sst")), name);
    sample(r.start, r.end, count, SEED)
}

/// Data blocks are framed `[compression: u8][payload][crc: u32]` and the
/// checksum covers the compression byte as well as the payload, so every
/// bit in the region is protected. A flip must surface as an error, at
/// open or at the read that touches the block; it must never come back
/// as a value the caller believes.
#[test]
fn a_bit_flip_in_an_sstable_data_block_is_caught_by_the_block_checksum() {
    watch("sst data block flips", |progress| {
        sst_region_sweep(
            DATA,
            &sst_region_positions(DATA, SAMPLE),
            "sst data block flips",
            progress,
        );
    });
}

/// The index block carries a checksum trailer counted inside the size the
/// footer records for it, so a flip anywhere in the region is caught at
/// open rather than indirectly. Without it a damaged separator key sends
/// a lookup to the wrong block and the read silently answers "not found"
/// for a key that is on disk, which is what this test used to record.
#[test]
fn a_bit_flip_in_the_sstable_index_block_is_caught_or_harmless() {
    watch("sst index flips", |progress| {
        sst_region_sweep(
            INDEX,
            &sst_region_positions(INDEX, SAMPLE),
            "sst index flips",
            progress,
        );
    });
}

/// A point lookup trusts the bloom: `may_contain` returning false
/// short-circuits the read, so clearing one set bit hides a key that is
/// physically present. The region now carries a checksum trailer, so the
/// table is refused at open instead of quietly answering "not found".
#[test]
fn a_bit_flip_in_the_sstable_bloom_region_is_caught_or_harmless() {
    watch("sst bloom flips", |progress| {
        sst_region_sweep(
            BLOOM,
            &sst_region_positions(BLOOM, SAMPLE),
            "sst bloom flips",
            progress,
        );
    });
}

/// The footer is the map of the file: seven offsets and sizes plus the
/// magic. A version-3 or version-4 footer covers those seven fields and
/// its magic with a 64-bit checksum, so a flip in any of its 72 bytes is
/// refused at open. Catches a footer field that is trusted without being
/// checked.
#[test]
fn a_bit_flip_in_the_sstable_footer_is_caught_or_harmless() {
    watch("sst footer flips", |progress| {
        sst_region_sweep(
            FOOTER,
            &sst_region_positions(FOOTER, SAMPLE),
            "sst footer flips",
            progress,
        );
    });
}

/// The exhaustive twin: every bit of every byte of the table, tallied by
/// region so an unprotected region is named rather than merely counted.
#[test]
fn every_bit_flip_in_an_sstable_is_caught_or_harmless() {
    watch("sst flips exhaustive", |progress| {
        let fixture = table_fixture();
        let regions = sst_regions(fixture.bytes(&fixture.only(".sst")));
        let mut failed = Vec::new();
        for r in regions {
            let result = panic::catch_unwind(|| {
                sst_region_sweep(
                    r.name,
                    &every(r.start, r.end),
                    &format!("sst {} exhaustive", r.name),
                    progress,
                );
            });
            if result.is_err() {
                failed.push(r.name);
            }
        }
        assert!(
            failed.is_empty(),
            "unprotected SSTable region(s): {failed:?} (details above)",
        );
    });
}

// ─── SSTable: files that are not there ──────────────────────────────────

/// A MANIFEST that references a table which is not on disk is what an
/// out-of-band delete, a partial restore or a half-finished copy leaves.
/// The open must fail and name the missing path; serving the database
/// without the table would silently drop every key it held. Catches an
/// open path that skips a table it cannot find.
#[test]
fn a_manifest_referencing_a_missing_sstable_refuses_to_open_and_names_it() {
    watch("missing sst", |_progress| {
        let fixture = table_fixture();
        let rel = fixture.only(".sst");
        let leaf = rel.rsplit('/').next().expect("a file name").to_string();
        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        fixture.plant(&db);
        fs::remove_file(db.join(&rel)).expect("remove the table");
        match Db::open(&db, fixture.opts()) {
            Ok(handle) => {
                let served = read_state(&handle).map(|s| s.len()).unwrap_or(0);
                panic!(
                    "opening without {leaf} served {served} of {} keys instead of failing",
                    fixture.state.len()
                );
            }
            Err(e) => assert!(
                e.to_string().contains(&leaf),
                "the error must name the missing file {leaf}, got: {e}",
            ),
        }
        assert_engine_never_panicked("missing sst");
    });
}

// ─── MANIFEST ───────────────────────────────────────────────────────────

/// A MANIFEST truncated by a crash loses its tail, which legitimately
/// costs whole tables. What it must never do is serve a key that was
/// never written or a value that was never stored under its key, and it
/// must never serve an inconsistent view. Catches a replay that resumes
/// after damaged bytes and reads a later record as if the record before
/// it had been intact.
fn manifest_truncation_sweep(cuts: &[u64], what: &str, progress: &AtomicU64) {
    let fixture = table_fixture();
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut tally = Tally::default();
    let expect = never_invents(&fixture.state);
    for &cut in cuts {
        progress.fetch_add(1, Ordering::Relaxed);
        let outcome = trial(fixture, &db, |db| truncate_at(&db.join("MANIFEST"), cut));
        tally.record(&format!("MANIFEST truncated to {cut}"), outcome, &expect);
    }
    tally.finish(what);
}

#[test]
fn a_manifest_truncated_at_a_sampled_offset_never_serves_invented_data() {
    watch("manifest truncation sample", |progress| {
        let fixture = table_fixture();
        let len = fixture.bytes("MANIFEST").len() as u64;
        manifest_truncation_sweep(
            &sample(0, len, SAMPLE, SEED),
            "manifest truncation sample",
            progress,
        );
    });
}

/// The exhaustive twin: every byte offset of the MANIFEST.
#[test]
fn a_manifest_truncated_at_every_offset_never_serves_invented_data() {
    watch("manifest truncation exhaustive", |progress| {
        let fixture = table_fixture();
        let len = fixture.bytes("MANIFEST").len() as u64;
        manifest_truncation_sweep(&every(0, len), "manifest truncation exhaustive", progress);
    });
}

/// A MANIFEST record checksum covers its length header and its payload,
/// so a flip anywhere in a record must stop replay at that record. The
/// records before it stay valid, so the database may still open with less
/// data; what it may not do is act on the damaged record or on anything
/// after it. Catches a replay that skips a bad record and keeps going.
fn manifest_flip_sweep(positions: &[u64], what: &str, progress: &AtomicU64) {
    let fixture = table_fixture();
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let mut tally = Tally::default();
    let expect = never_invents(&fixture.state);
    for &offset in positions {
        for bit in 0..8u8 {
            progress.fetch_add(1, Ordering::Relaxed);
            let outcome = trial(fixture, &db, |db| {
                flip_bit(&db.join("MANIFEST"), offset, bit)
            });
            tally.record(
                &format!("MANIFEST byte {offset} bit {bit} flipped"),
                outcome,
                &expect,
            );
        }
    }
    tally.finish(what);
}

#[test]
fn a_sampled_bit_flip_in_the_manifest_stops_replay_without_inventing_data() {
    watch("manifest flips sample", |progress| {
        let fixture = table_fixture();
        let len = fixture.bytes("MANIFEST").len() as u64;
        manifest_flip_sweep(
            &sample(0, len, SAMPLE, SEED),
            "manifest flips sample",
            progress,
        );
    });
}

/// The exhaustive twin: every bit of every byte of the MANIFEST.
#[test]
fn every_bit_flip_in_the_manifest_stops_replay_without_inventing_data() {
    watch("manifest flips exhaustive", |progress| {
        let fixture = table_fixture();
        let len = fixture.bytes("MANIFEST").len() as u64;
        manifest_flip_sweep(&every(0, len), "manifest flips exhaustive", progress);
    });
}

/// The MANIFEST is appended to without an fsync for most edits, so a
/// crash can leave a record header promising more bytes than were ever
/// written. Replay must stop there against the file size rather than
/// trusting the length. Catches a replay that indexes past the end of the
/// buffer or invents a record out of whatever follows.
#[test]
fn a_manifest_record_header_promising_more_bytes_than_exist_stops_replay() {
    watch("manifest torn header", |progress| {
        let fixture = table_fixture();
        let bytes = fixture.bytes("MANIFEST").to_vec();
        let frames = manifest_frames(&bytes);
        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        let mut tally = Tally::default();
        let expect = never_invents(&fixture.state);
        for frame in [frames[0], *frames.last().expect("non-empty")] {
            let real = (frame.end - frame.start - 8) as u32;
            for extra in [1u32, 9, 4096, 1 << 20, u32::MAX - real] {
                progress.fetch_add(1, Ordering::Relaxed);
                let claimed = real + extra;
                let outcome = trial(fixture, &db, |db| {
                    overwrite_range(&db.join("MANIFEST"), frame.start, &claimed.to_le_bytes())
                });
                tally.record(
                    &format!("record at {} claiming {claimed} bytes", frame.start),
                    outcome,
                    &expect,
                );
            }
        }
        tally.finish("manifest torn header");
    });
}

/// Garbage after the last committed record must cost the tail and nothing
/// else: every table committed before it is still described by intact
/// records, so all of that data must still be there. Catches a replay
/// that abandons the whole file when it meets a bad record, which would
/// turn a scribble on the tail into total data loss.
#[test]
fn garbage_appended_after_a_valid_manifest_keeps_every_committed_table() {
    watch("manifest trailing garbage", |progress| {
        let fixture = table_fixture();
        let len = fixture.bytes("MANIFEST").len() as u64;
        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        let mut tally = Tally::default();
        let expect = exactly(&fixture.state);
        for n in [1usize, 3, 4, 5, 8, 16, 64, 129, 512] {
            progress.fetch_add(1, Ordering::Relaxed);
            let junk = garbage(SEED ^ (n as u64) << 8, n);
            let outcome = trial(fixture, &db, |db| {
                overwrite_range(&db.join("MANIFEST"), len, &junk)
            });
            tally.record(&format!("{n} garbage bytes appended"), outcome, &expect);
        }
        tally.finish("manifest trailing garbage");
    });
}

/// Appending a copy of an existing record leaves a syntactically perfect
/// MANIFEST that says something impossible: for an `AddFile` record, that
/// one file id is live twice at one level. Levels above L0 are assumed
/// sorted and non-overlapping and are binary searched, so a duplicate is
/// a chance to return a key twice, to break iteration order, or to make a
/// point lookup and a scan disagree. Whatever the engine does with it, it
/// must not do that.
#[test]
fn a_manifest_that_names_the_same_table_twice_never_serves_an_inconsistent_view() {
    watch("duplicate manifest record", |progress| {
        let fixture = table_fixture();
        let bytes = fixture.bytes("MANIFEST").to_vec();
        let frames = manifest_frames(&bytes);
        let add_files = frames.iter().filter(|f| f.kind == TAG_ADD_FILE).count();
        assert!(
            add_files > 0,
            "the fixture MANIFEST must contain an AddFile record to duplicate",
        );
        let root = TempDir::new().expect("tempdir");
        let db = root.path().join("db");
        let mut tally = Tally::default();
        let expect = never_invents(&fixture.state);
        for frame in frames {
            progress.fetch_add(1, Ordering::Relaxed);
            let copy = bytes[frame.start as usize..frame.end as usize].to_vec();
            let outcome = trial(fixture, &db, |db| {
                overwrite_range(&db.join("MANIFEST"), bytes.len() as u64, &copy)
            });
            tally.record(
                &format!("record tag {} at {} duplicated", frame.kind, frame.start),
                outcome,
                &expect,
            );
        }
        tally.finish("duplicate manifest record");
    });
}

// ─── child entry point ──────────────────────────────────────────────────

#[test]
fn crash_child() {
    child_entrypoint(builtin_workload);
}
