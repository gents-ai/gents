//! A rotation must leave the log it seals durable.
//!
//! Recovery tolerates a torn tail only in the newest write-ahead log: an
//! earlier one is taken to have been complete when the rotation that created
//! its successor happened. Under the default `Eventual` durability nothing
//! syncs a log on its own, so unless the rotation does, a power cut between
//! the rotation and the flush that retires the sealed log can tear its tail
//! while a newer log exists, and the database then refuses to open.

// The whole file drives the `LD_PRELOAD` syscall shim, which exists only
// on Linux. Gated at the file level, like the other shim-backed suites.
#![cfg(target_os = "linux")]

mod common;

use std::time::Duration;

use common::fault::{
    self, ChildSpec, CrashRun, CutPoint, DieKind, OpValue, Phase, PowerLossOptions, Recovery,
    TearMode, Trigger,
};
use regolith::{Db, Options, WriteOptions};
use tempfile::TempDir;

/// The one write the child syncs. Everything after it in the first log is
/// unsynced, so a power cut with no rotation sync tears the log just past it.
const SYNCED_OP: usize = 5;

#[test]
fn crash_child() {
    fault::child_entrypoint(workload);
}

/// Puts under the default `Eventual` durability, except [`SYNCED_OP`].
fn workload(spec: &ChildSpec) {
    let db = Db::open(&spec.db_path, spec.options()).expect("child: open db");
    for (i, op) in spec.history().ops().iter().enumerate() {
        let opts = WriteOptions {
            sync: i == SYNCED_OP,
            ..WriteOptions::default()
        };
        match &op.value {
            OpValue::Put(value) => {
                regolith::through_stalls(|| db.put_opt(&opts, &op.key, value)).expect("child: put")
            }
            OpValue::Delete => unreachable!("the spec disables deletes"),
        }
    }
    db.close().expect("child: close");
}

/// Cut the power where `trigger` fires, then reopen. Returns how many writes
/// the reopened database holds.
fn cut_at(trigger: Trigger, tear: TearMode) -> usize {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::DuringFlush, &db).delete_every(0);
    let write_buffer_size = spec.write_buffer_size;
    let out = CrashRun::new(spec)
        .trigger(trigger)
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    let popts = PowerLossOptions::default().tear(tear);
    let report =
        fault::simulate_power_loss_with(&out.spec.db_path, &out.journal, CutPoint::End, &popts);

    let opts = Options::default().write_buffer_size(write_buffer_size);
    match fault::recover_and_validate(&db, opts, &out.history) {
        Recovery::Recovered(r) => r.k,
        Recovery::RefusedToOpen(e) => panic!(
            "{tear:?}: the database refuses to open after a power cut around a rotation: \
             {e}\n{}",
            report.summary(),
        ),
    }
}

/// The first write to an SSTable: the memtable has been rotated out, its log
/// sealed and a newer one created, and the flush that would retire the sealed
/// log has not finished.
fn inside_the_first_flush() -> Trigger {
    Trigger::sst_write(1)
}

/// Just before the first write to the second log, whose id the rotation takes
/// from the file counter: the new file exists and is empty. Recovery takes the
/// newest log by name, so this is already enough to make the sealed one
/// unreadable if it was not synced first.
fn right_after_the_new_log_is_created() -> Trigger {
    Trigger::Syscall {
        kind: DieKind::Write,
        path_contains: "wal_000002.log".to_string(),
        nth: 1,
        before: true,
    }
}

fn assert_sealed_log_survived(k: usize) {
    assert!(
        k > SYNCED_OP + 1,
        "only {k} writes survived, so the sealed log lost its unsynced tail"
    );
}

#[test]
fn a_sector_tearing_cut_inside_the_flush_after_a_rotation_leaves_a_database_that_opens() {
    assert_sealed_log_survived(cut_at(inside_the_first_flush(), TearMode::TornSector));
}

#[test]
fn a_truncating_cut_inside_the_flush_after_a_rotation_keeps_the_whole_sealed_log() {
    assert_sealed_log_survived(cut_at(inside_the_first_flush(), TearMode::Truncate));
}

#[test]
fn a_sector_tearing_cut_right_after_the_new_log_is_created_leaves_a_database_that_opens() {
    assert_sealed_log_survived(cut_at(
        right_after_the_new_log_is_created(),
        TearMode::TornSector,
    ));
}

#[test]
fn a_truncating_cut_right_after_the_new_log_is_created_keeps_the_whole_sealed_log() {
    assert_sealed_log_survived(cut_at(
        right_after_the_new_log_is_created(),
        TearMode::Truncate,
    ));
}
