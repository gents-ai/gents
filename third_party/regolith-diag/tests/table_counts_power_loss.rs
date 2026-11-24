//! A power cut around the manifest record that carries a table's deletion
//! counts.
//!
//! The record is written in two places. A new table's counts go in the same
//! batch as the table, so a cut leaves both or neither: a table is never
//! listed without the counts it was written with. A table an earlier
//! version left is counted in the background, and its counts go in a batch
//! of their own that is not synced: a cut may lose it, the table is then
//! uncounted again, and it is counted again. Either way the database opens
//! and holds what it held.
//!
//! The second half runs on `tests/fixtures/v0_1_9`, a database regolith
//! 0.1.9 wrote (two tables and a format 1 log; see
//! `upgrade_from_0_1_9.rs`), whose tables carry no counts.
//!
//! # Linux only
//!
//! `LD_PRELOAD` interposition is a glibc mechanism; the file is compiled out
//! elsewhere. See `power_loss.rs` for what the shim does and does not prove.
#![cfg(target_os = "linux")]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::fault::{
    self, ChildSpec, CrashRun, CutPoint, DieKind, Phase, PowerLossOptions, TearMode, Trigger,
};
use regolith::{CompactionOutcome, Db, DurabilityMode, MergeOperator, Options};
use tempfile::TempDir;

const UNCOUNTED: &str = "regolith.num-uncounted-tables";

#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

const COUNT_THEN_DIE: &str = "count_then_die";

fn dispatch(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == COUNT_THEN_DIE => count_then_die(spec),
        _ => fault::builtin_workload(spec),
    }
}

const TEARS: [TearMode; 4] = [
    TearMode::Truncate,
    TearMode::Zero,
    TearMode::Garbage,
    TearMode::TornSector,
];

fn syscall(kind: DieKind, nth: u64, before: bool) -> Trigger {
    Trigger::Syscall {
        kind,
        path_contains: "MANIFEST".to_string(),
        nth,
        before,
    }
}

fn uncounted(db: &Db) -> u64 {
    db.get_int_property(UNCOUNTED).expect("the property exists")
}

/// The child ran under the shim and reached the manifest: a run that
/// recorded nothing would pass every check below without a cut.
fn assert_recorded(journal: &fault::Journal, trigger: &Trigger) {
    assert!(
        journal
            .records
            .iter()
            .any(|record| record.path.to_string_lossy().contains("MANIFEST")),
        "{trigger:?}: the child's manifest writes were not recorded\n{journal}"
    );
}

// ---------------------------------------------------------------------
// A new table and its counts: one batch.

/// No worker, so nothing counts a table between the open and the check.
fn flush_opts() -> Options {
    Options::default()
        .write_buffer_size(8 * 1024)
        .max_background_compactions(0)
}

/// Cut a run of flushes at `trigger`, tear the unsynced bytes with `tear`,
/// reopen, and return how many tables the reopened database lists.
fn cut_a_flush(trigger: &Trigger, tear: TearMode) -> u64 {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::DuringFlush, &db)
        .durability(DurabilityMode::Immediate)
        .workers(0);
    let out = CrashRun::new(spec)
        .trigger(trigger.clone())
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    assert_recorded(&out.journal, trigger);
    let report = fault::simulate_power_loss_with(
        &db,
        &out.journal,
        CutPoint::End,
        &PowerLossOptions::default().tear(tear),
    );
    let db = Db::open(&db, flush_opts()).unwrap_or_else(|e| {
        panic!(
            "{trigger:?}, {tear:?}: the database refuses to open: {e}\n{}",
            report.summary()
        )
    });
    // Read before anything else runs: every table the manifest lists was
    // written by this version, so each is listed with its counts or not at
    // all.
    assert_eq!(
        uncounted(&db),
        0,
        "{trigger:?}, {tear:?}: a table is listed without its counts\n{}",
        report.summary()
    );
    let tables = db
        .get_int_property("regolith.num-files-at-level0")
        .expect("the property exists");
    let recovered = fault::assert_valid_prefix(&db, &out.history);
    fault::assert_acked_survived(&recovered, &out.acked());
    tables
}

/// The manifest's second sync is the first flush's batch: the first made
/// its header durable when the database was created.
const FIRST_FLUSH_BATCH: u64 = 2;

#[test]
fn a_cut_before_a_tables_batch_is_synced_leaves_no_table_without_counts() {
    for tear in TEARS {
        cut_a_flush(&syscall(DieKind::Fsync, FIRST_FLUSH_BATCH, true), tear);
    }
}

#[test]
fn a_cut_once_a_tables_batch_is_synced_keeps_the_table_and_its_counts() {
    for tear in TEARS {
        let tables = cut_a_flush(&syscall(DieKind::Fsync, FIRST_FLUSH_BATCH, false), tear);
        assert!(tables >= 1, "{tear:?}: the synced batch's table is gone");
    }
}

#[test]
fn a_cut_at_any_manifest_write_leaves_no_table_without_counts() {
    for nth in 1..=6 {
        for before in [true, false] {
            cut_a_flush(&syscall(DieKind::Write, nth, before), TearMode::TornSector);
            cut_a_flush(&syscall(DieKind::Write, nth, before), TearMode::Truncate);
        }
    }
}

// ---------------------------------------------------------------------
// Counts recorded for a table an earlier version left: a batch of their
// own, not synced.

struct Concat;

impl MergeOperator for Concat {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for op in operands {
            out.extend_from_slice(op);
        }
        Some(out)
    }

    fn name(&self) -> &'static str {
        "concat"
    }
}

/// The fixture's options, with no worker: a table is counted only when a
/// test, or the child, asks for a pass.
fn fixture_opts() -> Options {
    Options::default()
        .write_buffer_size(64 * 1024 * 1024)
        .merge_operator(Some(Arc::new(Concat)))
        .max_background_compactions(0)
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0_1_9")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn read_all(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.scan(None, None).unwrap().into_iter().collect()
}

/// Run passes until every table is counted. Each idle pass counts one.
fn count_every_table(db: &Db) -> u64 {
    let mut passes = 0;
    while uncounted(db) > 0 {
        assert!(passes < 64, "the tables are never all counted");
        db.compact_step().expect("a pass");
        passes += 1;
    }
    passes
}

/// Beside the database directory, so the shim records none of it.
fn counted_marker(db: &Path) -> PathBuf {
    db.with_extension("counted")
}

/// Open the fixture, count its tables, record that, and die.
fn count_then_die(spec: &ChildSpec) {
    let db = Db::open(&spec.db_path, fixture_opts()).expect("child: open");
    count_every_table(&db);
    let marker = counted_marker(&spec.db_path);
    std::fs::write(&marker, b"1").expect("child: marker");
    std::fs::File::open(&marker)
        .and_then(|f| f.sync_all())
        .expect("child: sync marker");
    fault::kill_self();
}

/// What the fixture holds, read from a copy this version opened cleanly.
fn fixture_contents() -> BTreeMap<Vec<u8>, Vec<u8>> {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("db");
    copy_tree(&fixture_dir(), &dir);
    read_all(&Db::open(&dir, fixture_opts()).unwrap())
}

#[test]
fn the_tables_an_earlier_version_left_are_counted_and_stay_counted() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("db");
    copy_tree(&fixture_dir(), &dir);
    let before = {
        let db = Db::open(&dir, fixture_opts()).unwrap();
        assert_eq!(
            uncounted(&db),
            2,
            "the fixture's two tables carry no counts"
        );
        let before = read_all(&db);
        assert_eq!(db.compact_step().unwrap(), CompactionOutcome::Idle);
        assert_eq!(uncounted(&db), 1, "one table for one idle pass");
        assert_eq!(db.compact_step().unwrap(), CompactionOutcome::Idle);
        assert_eq!(uncounted(&db), 0);
        assert_eq!(read_all(&db), before, "counting changes no read");
        db.close().unwrap();
        before
    };
    let db = Db::open(&dir, fixture_opts()).unwrap();
    assert_eq!(uncounted(&db), 0, "the counts were recorded");
    assert_eq!(read_all(&db), before);
}

/// Cut the child that counts the fixture's tables at `trigger`, tear with
/// `tear`, and check the database opens, holds what it held, and ends up
/// with every table counted. Returns how many tables the cut left
/// uncounted.
fn cut_the_count(trigger: Trigger, tear: TearMode, contents: &BTreeMap<Vec<u8>, Vec<u8>>) -> u64 {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("db");
    copy_tree(&fixture_dir(), &dir);
    let spec = ChildSpec::new(Phase::Custom(COUNT_THEN_DIE.to_string()), &dir);
    let out = CrashRun::new(spec)
        .trigger(trigger.clone())
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    assert_recorded(&out.journal, &trigger);
    if trigger == Trigger::Workload {
        assert!(
            counted_marker(&dir).exists(),
            "the child counted both tables"
        );
    }
    let report = fault::simulate_power_loss_with(
        &dir,
        &out.journal,
        CutPoint::End,
        &PowerLossOptions::default().tear(tear),
    );
    let db = Db::open(&dir, fixture_opts()).unwrap_or_else(|e| {
        panic!(
            "{trigger:?}, {tear:?}: the database refuses to open after a cut while its tables \
             were counted: {e}\n{}",
            report.summary()
        )
    });
    assert_eq!(
        &read_all(&db),
        contents,
        "{trigger:?}, {tear:?}: the database does not hold what it held"
    );
    // A count the cut lost is taken again.
    let lost = uncounted(&db);
    assert!(lost <= 2, "{trigger:?}, {tear:?}");
    count_every_table(&db);
    assert_eq!(&read_all(&db), contents, "{trigger:?}, {tear:?}");
    db.close().unwrap();
    drop(db);
    let db = Db::open(&dir, fixture_opts()).unwrap();
    assert_eq!(uncounted(&db), 0, "{trigger:?}, {tear:?}");
    assert_eq!(&read_all(&db), contents, "{trigger:?}, {tear:?}");
    lost
}

/// The counts are appended and never synced by the count itself, so a cut
/// once both tables are counted can lose either batch, whole or torn.
#[test]
fn a_cut_after_the_tables_are_counted_opens_and_counts_what_it_lost_again() {
    let contents = fixture_contents();
    let lost: Vec<u64> = TEARS
        .iter()
        .map(|&tear| cut_the_count(Trigger::Workload, tear, &contents))
        .collect();
    // The cut is real: at least one tear lost a count the child had taken.
    assert!(
        lost.iter().any(|&tables| tables > 0),
        "no cut lost a count, so counting again was never exercised: {lost:?}"
    );
}

#[test]
fn a_cut_at_any_manifest_write_of_the_count_opens_and_counts_again() {
    let contents = fixture_contents();
    for nth in 1..=8 {
        for before in [true, false] {
            for tear in [TearMode::Truncate, TearMode::TornSector] {
                cut_the_count(syscall(DieKind::Write, nth, before), tear, &contents);
            }
        }
    }
}
