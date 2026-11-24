//! A power cut during a database's first flush (E29).
//!
//! A flush writes and syncs its table, makes it part of the database with
//! one synced manifest batch, and only then removes the log that held its
//! writes. The batches the manifest takes before that one only reserve file
//! ids and are never synced on their own. A cut inside the first flush
//! therefore tears an unsynced manifest tail while the table, whole or
//! torn, sits in the table directory and no durable batch names it. A crash
//! produces that state, and every write is still in the log, so the
//! database opens. Whatever the cut and whatever the tear:
//!
//! - the database opens;
//! - every acknowledged write survives;
//! - once the flush's batch was synced, its table is part of the database.
//!
//! # Linux only
//!
//! `LD_PRELOAD` interposition is a glibc mechanism; the file is compiled out
//! elsewhere. See `power_loss.rs` for what the shim does and does not prove.
#![cfg(target_os = "linux")]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::fault::{
    self, ChildSpec, CrashRun, CutPoint, DieKind, OpValue, Phase, PowerLossOptions, TearMode,
    Trigger,
};
use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

const FLUSH_THEN_DIE: &str = "flush_then_die";

fn dispatch(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == FLUSH_THEN_DIE => flush_then_die(spec),
        _ => fault::builtin_workload(spec),
    }
}

/// Beside the database directory, so the shim records none of it.
fn flushed_marker(db: &Path) -> PathBuf {
    db.with_extension("flushed")
}

/// Write the first half of the history, flush it into the first table,
/// record that the flush returned, and die.
fn flush_then_die(spec: &ChildSpec) {
    let db = Db::open(&spec.db_path, spec.options()).expect("child: open");
    let history = spec.history();
    let ops = history.ops();
    for op in &ops[..ops.len() / 2] {
        match &op.value {
            OpValue::Put(v) => regolith::through_stalls(|| db.put(&op.key, v)).expect("child: put"),
            OpValue::Delete => {
                regolith::through_stalls(|| db.delete(&op.key)).expect("child: delete")
            }
        }
    }
    db.flush().expect("child: flush");
    let marker = flushed_marker(&spec.db_path);
    std::fs::write(&marker, b"1").expect("child: marker");
    std::fs::File::open(&marker)
        .and_then(|f| f.sync_all())
        .expect("child: sync marker");
    fault::kill_self();
}

const TEARS: [TearMode; 4] = [
    TearMode::Truncate,
    TearMode::Zero,
    TearMode::Garbage,
    TearMode::TornSector,
];

fn opts() -> Options {
    Options::default().write_buffer_size(8 * 1024)
}

fn syscall(kind: DieKind, path: &str, nth: u64, before: bool) -> Trigger {
    Trigger::Syscall {
        kind,
        path_contains: path.to_string(),
        nth,
        before,
    }
}

/// The manifest's second sync: the first made its header durable when the
/// database was created, the second is the first flush's batch.
const FIRST_FLUSH_BATCH: u64 = 2;

/// Cut the first flush at `trigger`, tear the unsynced bytes with `tear`,
/// reopen, and return how many tables the reopened database holds at L0.
fn cut(trigger: &Trigger, tear: TearMode) -> u64 {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::DuringFlush, &db).durability(DurabilityMode::Immediate);
    let out = CrashRun::new(spec)
        .trigger(trigger.clone())
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    if let Trigger::Syscall { path_contains, .. } = trigger {
        // The compaction worker flushes while the writer goes on writing the
        // log (E9), so the writer's calls can follow the fatal one in the
        // journal. The thread that died ends on the file it was cut in.
        let mut last_per_thread = std::collections::HashMap::new();
        for record in &out.journal.records {
            last_per_thread.insert(record.tid, &record.path);
        }
        assert!(
            last_per_thread
                .values()
                .any(|path| path.to_string_lossy().contains(path_contains.as_str())),
            "no thread's last call was on {path_contains}\n{}",
            out.journal
        );
    }
    assert!(out.acked_count() > 0, "no write was acknowledged");
    let report = fault::simulate_power_loss_with(
        &db,
        &out.journal,
        CutPoint::End,
        &PowerLossOptions::default().tear(tear),
    );
    let db = Db::open(&db, opts()).unwrap_or_else(|e| {
        panic!(
            "{trigger:?}, {tear:?}: the database refuses to open after a power cut in its first \
             flush: {e}\n{}",
            report.summary()
        )
    });
    // Counted before anything else runs: a close flushes a full memtable,
    // which would add a table of its own.
    let tables = db
        .get_int_property("regolith.num-files-at-level0")
        .expect("the property exists");
    let recovered = fault::assert_valid_prefix(&db, &out.history);
    fault::assert_acked_survived(&recovered, &out.acked());
    tables
}

#[test]
fn a_cut_while_the_first_table_is_written_opens_with_every_acknowledged_write() {
    for tear in TEARS {
        cut(&Trigger::sst_write(1), tear);
    }
}

#[test]
fn a_cut_once_the_first_table_is_synced_opens_with_every_acknowledged_write() {
    for tear in TEARS {
        cut(&syscall(DieKind::Fsync, ".sst", 1, false), tear);
    }
}

#[test]
fn a_cut_before_the_first_flush_batch_is_synced_opens_with_every_acknowledged_write() {
    for tear in TEARS {
        cut(
            &syscall(DieKind::Fsync, "MANIFEST", FIRST_FLUSH_BATCH, true),
            tear,
        );
    }
}

#[test]
fn a_cut_once_the_first_flush_batch_is_synced_keeps_its_table() {
    for tear in TEARS {
        let tables = cut(
            &syscall(DieKind::Fsync, "MANIFEST", FIRST_FLUSH_BATCH, false),
            tear,
        );
        assert!(
            tables >= 1,
            "{tear:?}: the first flush's batch was synced, yet the database holds no table"
        );
    }
}

/// Once the first flush returned, its table is the only copy of the writes
/// it holds: the log that held them is gone. A power cut then must keep the
/// table and every one of those writes, which takes the flush's batch being
/// synced before the log is removed.
#[test]
fn a_cut_after_the_first_flush_returned_keeps_its_table_and_its_writes() {
    for tear in TEARS {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("db");
        let spec = ChildSpec::new(Phase::Custom(FLUSH_THEN_DIE.to_string()), &db)
            .durability(DurabilityMode::Immediate)
            .delete_every(0);
        let history = spec.history();
        let out = CrashRun::new(spec)
            .trigger(Trigger::Workload)
            .timeout(Duration::from_secs(180))
            .run();
        out.assert_killed();
        assert!(flushed_marker(&db).exists(), "the flush returned");
        let report = fault::simulate_power_loss_with(
            &db,
            &out.journal,
            CutPoint::End,
            &PowerLossOptions::default().tear(tear),
        );
        let db = Db::open(&db, opts()).unwrap_or_else(|e| {
            panic!(
                "{tear:?}: refused after the first flush returned: {e}\n{}",
                report.summary()
            )
        });
        let tables = db
            .get_int_property("regolith.num-files-at-level0")
            .expect("the property exists");
        assert!(tables >= 1, "{tear:?}: the returned flush's table is gone");
        let recovered = fault::assert_valid_prefix(&db, &history);
        assert!(
            recovered.k >= history.ops().len() / 2,
            "{tear:?}: {} of the {} writes the flush returned with survived",
            recovered.k,
            history.ops().len() / 2
        );
    }
}
