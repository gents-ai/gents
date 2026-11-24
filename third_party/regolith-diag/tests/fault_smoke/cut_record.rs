//! A journal record is never cut.
//!
//! A child dies by `SIGKILL` while its other threads go on working, and a
//! thread inside a record's write at that instant could leave the front of
//! the record behind: the kernel copies a write a page at a time and gives up
//! between two pages once the kill is pending, so a record lying across a
//! page boundary of the journal ended at that boundary. Such a journal was
//! refused when what was left of the record could not be read, which failed
//! about one run in 150 of every suite built on the shim, and replayed wrong
//! when it could: a write of 100 bytes as a write of 1.
//!
//! So every record is one length that divides a page, no record lies across
//! two pages, and a journal is whole records or it is refused. These tests
//! pin both halves.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::common::fault::journal::RECORD_BYTES;
use crate::common::fault::{
    self, ChildSpec, CrashRun, CutPoint, Journal, OpKind, Phase, PowerLossOptions, TearMode,
    Trigger, journal_path_for,
};
use tempfile::TempDir;

const PAGE: usize = 4096;
/// The descriptor the recorded child had its data file open on.
const FD: i64 = 5;
/// `O_WRONLY | O_CREAT`.
const CREATING: i64 = 0o101;

/// One record as the shim writes it.
fn record(seq: u64, kind: char, fd: i64, a: i64, b: i64, ret: i64, path: &str) -> String {
    let mut line = format!("{seq}\t7\t{kind}\t{fd}\t{a}\t{b}\t{ret}\t{path}");
    line.extend(std::iter::repeat_n('\0', RECORD_BYTES - 1 - line.len()));
    line.push('\n');
    line
}

/// The record of a third write of 100 bytes, never synced.
fn last() -> String {
    record(5, 'W', FD, 200, 100, 100, "")
}

/// A database directory holding one file of 300 bytes, and the journal of
/// the child that wrote it up to its third write: 100 bytes written and
/// synced, then 100 written and never synced.
struct Scene {
    _tmp: TempDir,
    db: PathBuf,
    data: PathBuf,
    whole: String,
}

impl Scene {
    fn new() -> Scene {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("db");
        std::fs::create_dir(&db).unwrap();
        let data = db.join("data");
        std::fs::write(&data, [b'x'; 300]).unwrap();
        let whole = [
            record(1, 'O', FD, 0, CREATING, 0, data.to_str().unwrap()),
            record(2, 'W', FD, 0, 100, 100, ""),
            record(3, 'S', FD, 1, 0, 0, ""),
            record(4, 'W', FD, 100, 100, 100, ""),
        ]
        .concat();
        Scene {
            _tmp: tmp,
            db,
            data,
            whole,
        }
    }

    /// Write the scene's records and `rest` as the journal and read it back.
    fn journal(&self, rest: &str) -> Journal {
        let path = journal_path_for(&self.db);
        std::fs::write(&path, format!("{}{rest}", self.whole)).unwrap();
        Journal::read(&path).unwrap()
    }
}

/// Reconstruct with the unsynced bytes zeroed in place.
fn zeroing() -> PowerLossOptions {
    PowerLossOptions::default().tear(TearMode::Zero)
}

/// Why the reconstruction refused `journal`, if it did.
fn refusal(db: &Path, journal: &Journal) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fault::simulate_power_loss_with(db, journal, CutPoint::End, &zeroing())
    }))
    .err()
    .map(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "a panic with no message".to_string())
    })
}

/// The scene's journal with its last record whole: nothing is refused, and
/// the third write is replayed as what it was.
#[test]
fn a_journal_of_whole_records_is_replayed() {
    let scene = Scene::new();
    let journal = scene.journal(&last());
    assert_eq!(journal.malformed, 0, "{journal}");
    assert_eq!(
        journal
            .records
            .iter()
            .filter(|r| r.kind == OpKind::Write)
            .map(|r| (r.a, r.ret))
            .collect::<Vec<_>>(),
        [(0, 100), (100, 100), (200, 100)]
    );
    fault::simulate_power_loss_with(&scene.db, &journal, CutPoint::End, &zeroing());
    // The synced bytes stand and the unsynced ones stay, as zeros.
    let mut left = vec![b'x'; 100];
    left.extend([0u8; 200]);
    assert_eq!(std::fs::read(&scene.data).unwrap(), left);
}

/// What is left of a record that was cut is never read as a record, wherever
/// the cut fell and whether or not other records follow it: the journal is
/// refused and the directory left as it was.
///
/// Catches: a parser that reads the front of a cut record as a record (a
/// write of 100 bytes cut inside its last number replays as a write of 1),
/// one that drops what it cannot read and reconstructs from the rest, and
/// one that reads a cut record run into the next as a single record.
#[test]
fn a_journal_with_a_cut_record_is_refused_wherever_the_cut_fell() {
    let last = last();
    let numbers = last.find("\t200\t100\t100\t").unwrap();
    let cuts = [
        ("inside its first number", 1),
        ("inside its numbers", numbers + "\t200\t1".len()),
        ("inside its last number", numbers + "\t200\t100\t1".len()),
        (
            "inside its padding",
            numbers + "\t200\t100\t100\t".len() + 9,
        ),
        ("before its line end", RECORD_BYTES - 1),
    ];
    let later = record(6, 'S', FD, 1, 0, 0, "");
    for (what, keep) in cuts {
        let cut = &last[..keep];
        let journals = [
            ("at the end", cut.to_string(), 1),
            ("ended and followed", format!("{cut}\n{later}"), 1),
            ("run into the next", format!("{cut}{later}"), 1),
            (
                "run into the next, with another at the end",
                format!("{cut}{later}{cut}"),
                2,
            ),
        ];
        for (shape, rest, unreadable) in journals {
            // All of a record but its line end, and then a line end, is the
            // record.
            if rest.starts_with(&last) {
                continue;
            }
            let what = format!("a record cut {what}, {shape}");
            let scene = Scene::new();
            let journal = scene.journal(&rest);
            assert_eq!(journal.malformed, unreadable, "{what}: {journal}");
            assert!(
                journal.records.iter().all(|r| r.seq != 5),
                "{what}: what is left of the cut record was read as a record"
            );
            let refused = refusal(&scene.db, &journal);
            assert!(
                refused.as_deref().is_some_and(
                    |why| why.contains(&format!("has {unreadable} unparseable lines"))
                ),
                "{what}: {refused:?}"
            );
            assert_eq!(
                std::fs::read(&scene.data).unwrap(),
                [b'x'; 300],
                "{what}: a refused journal rewrites nothing"
            );
        }
    }
}

/// A line of the right length that is not the eight fields is refused too.
#[test]
fn a_line_that_is_not_eight_fields_is_refused() {
    for (what, fields) in [
        ("seven fields", "5\t7\tW\t5\t200\t100\t100"),
        ("nine fields", "5\t7\tW\t5\t200\t100\t100\t\t"),
    ] {
        let mut line = fields.to_string();
        line.extend(std::iter::repeat_n('\0', RECORD_BYTES - 1 - line.len()));
        line.push('\n');
        let scene = Scene::new();
        let journal = scene.journal(&line);
        assert_eq!(journal.malformed, 1, "{what}: {journal}");
        assert!(refusal(&scene.db, &journal).is_some(), "{what}");
    }
}

const STORM: &str = "record-storm";
/// Threads appending records while the kill lands.
const STORM_THREADS: usize = 4;
/// Children the storm test kills.
const STORM_KILLS: u64 = 200;

/// Threads that do nothing but make the shim append records, so that the
/// kill often lands while one of them is inside a record's write.
fn storm(spec: &ChildSpec) {
    // Removing a file that is not there fails at once and still leaves a
    // record. The long name makes its fields several hundred bytes, so that
    // written at their own length about one record in nine would lie across
    // a page boundary of the journal.
    let missing = spec.db_path.join("m".repeat(200)).join("m".repeat(200));
    // One name longer than a record has room for.
    let _ = std::fs::remove_file(missing.join("m".repeat(200)));
    let appending = Arc::new(AtomicUsize::new(0));
    for t in 0..STORM_THREADS {
        let missing = missing.join(t.to_string());
        let appending = Arc::clone(&appending);
        std::thread::spawn(move || {
            let _ = std::fs::remove_file(&missing);
            appending.fetch_add(1, Ordering::Release);
            loop {
                let _ = std::fs::remove_file(&missing);
            }
        });
    }
    // Every thread is appending before the countdown to the kill starts.
    while appending.load(Ordering::Acquire) < STORM_THREADS {
        std::thread::yield_now();
    }
    // The seed spreads the kill over a page of the journal.
    for _ in 0..8 + spec.seed % 16 {
        let _ = std::fs::remove_file(&missing);
    }
    fault::kill_self();
}

#[test]
fn storm_child() {
    fault::child_entrypoint(storm);
}

fn storm_run(tmp: &TempDir, seed: u64) -> fault::ChildOutcome {
    let db = tmp.path().join(format!("db-{seed}"));
    let out = CrashRun::new(ChildSpec::new(Phase::Custom(STORM.into()), &db).seed(seed))
        .entry_test("cut_record::storm_child")
        .trigger(Trigger::Workload)
        .run();
    out.assert_killed();
    out
}

/// The bytes of a journal, checked to be whole records of one length, none
/// of which lies across a page.
fn whole_records(journal: &Journal) -> Vec<u8> {
    let bytes = std::fs::read(&journal.source).unwrap();
    assert_eq!(PAGE % RECORD_BYTES, 0);
    assert_eq!(bytes.len() % RECORD_BYTES, 0, "{journal}");
    for record in bytes.chunks(RECORD_BYTES) {
        assert_eq!(
            record.iter().position(|b| *b == b'\n'),
            Some(RECORD_BYTES - 1),
            "a record's only line end is its last byte: {journal}"
        );
    }
    assert_eq!(journal.malformed, 0, "{journal}");
    assert_eq!(
        journal.records.len(),
        bytes.len() / RECORD_BYTES,
        "{journal}"
    );
    bytes
}

/// The shim writes every record at one length that divides a page, whatever
/// the record holds: the engine's own I/O with its short records, and a path
/// longer than a record has room for.
///
/// Catches: a record written at its own length, which puts some record
/// across a page boundary of the journal, where a kill can cut it.
#[test]
fn the_shim_writes_every_record_at_one_length_that_divides_a_page() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let out = CrashRun::new(ChildSpec::new(Phase::CleanExit, &db).ops(200)).run();
    out.assert_clean();
    let engine = whole_records(&out.journal);
    assert!(engine.len() > PAGE, "the run filled more than a page");

    let stormed = whole_records(&storm_run(&tmp, 0).journal);
    assert!(
        stormed
            .chunks(RECORD_BYTES)
            .any(|record| record[RECORD_BYTES - 2] != 0),
        "the name longer than a record has room for fills its record"
    );
}

/// The real shim under the real kill: children whose threads append records
/// as fast as they can are killed, and every journal they leave is whole
/// records.
///
/// With records written at their own length, 4 to 7 of every 200 such kills
/// left a record cut at a page boundary of the journal, and 2 of about 1500
/// left a whole record behind the cut one (tmpfs, Linux 6.18).
///
/// Catches: any way of writing a record that a kill can cut.
#[test]
fn a_kill_while_threads_append_records_cuts_none_of_them() {
    let tmp = TempDir::new().unwrap();
    for seed in 0..STORM_KILLS {
        let out = storm_run(&tmp, seed);
        whole_records(&out.journal);
        fault::simulate_power_loss(&out.spec.db_path, CutPoint::End);
    }
}
