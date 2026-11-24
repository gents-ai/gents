//! A power cut inside a database's first flush, under every tear mode a
//! real filesystem produces (E29).
//!
//! The cut leaves a table no durable manifest batch names, whole, zeroed or
//! torn, beside a manifest whose unsynced tail is torn. ext4 with the
//! blocks already allocated leaves zeros at full length, and a device that
//! tears at sector granularity leaves a prefix followed by unrelated bytes.
//!
//! The open used to refuse whenever such a table could hold data and the
//! manifest named no table, because it could not tell this crash from a
//! manifest whose first durable batch was later damaged. It now judges the
//! manifest's end by the rule a write-ahead log's tail follows: no later
//! batch proves the torn bytes were synced, so a crash left them, the table
//! is no part of the database, and every acknowledged write is in the
//! fsynced WAL. So every tear opens with every acknowledged write, and the
//! orphan is removed by the next open that replays the manifest whole.
//!
//! The refusal remains for what no crash produces: damage below a batch a
//! later sync proves (`src/engine/manifest/tail_tests.rs`), and a manifest
//! with no header beside a table that may hold data
//! (`open_and_corruption.rs`, `adversarial_open_guard.rs`).

// The whole file drives the `LD_PRELOAD` syscall shim, which exists
// only on Linux: on macOS the loader equivalent is blocked by system
// integrity protection, and on Windows there is none. Gated at the file
// level, like the other shim-backed suites, so the tests are absent
// rather than failing where the substrate cannot run.
#![cfg(target_os = "linux")]

mod common;

use common::fault::{
    self, ChildOutcome, ChildSpec, CrashRun, CutPoint, Phase, PowerLossOptions, Recovery, TearMode,
    Trigger,
};
use regolith::{DurabilityMode, Options};
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn crash_child() {
    fault::child_entrypoint(fault::builtin_workload);
}

fn opts(write_buffer_size: usize) -> Options {
    Options::default().write_buffer_size(write_buffer_size)
}

fn cut_first_flush(db: &std::path::Path, tear: TearMode) -> ChildOutcome {
    let spec = ChildSpec::new(Phase::DuringFlush, db).durability(DurabilityMode::Immediate);
    let out = CrashRun::new(spec)
        .trigger(Trigger::sst_write(1))
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    let popts = PowerLossOptions::default().tear(tear);
    fault::simulate_power_loss_with(&out.spec.db_path, &out.journal, CutPoint::End, &popts);
    out
}

/// A tear that leaves nothing readable behind must still open, with
/// every acknowledged write intact.
fn probe_opens(tear: TearMode) {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let out = cut_first_flush(&db, tear);
    let ssts = fault::find_ssts(&db);
    let lens: Vec<u64> = ssts.iter().map(|p| fault::file_len(p)).collect();
    println!(
        "{tear:?}: orphan SSTables {ssts:?} lengths {lens:?}, {} acked",
        out.acked_count()
    );
    assert!(out.acked_count() > 0, "no write was acknowledged");

    match fault::recover_and_validate(&db, opts(8 * 1024), &out.history) {
        Recovery::Recovered(r) => {
            fault::assert_acked_survived(&r, &out.acked());
            println!("{tear:?}: opened, {} writes recovered", r.k);
        }
        Recovery::RefusedToOpen(e) => panic!("{tear:?} must not block the open: {e}"),
    }
}

/// A tear that leaves an orphan whose bytes prove nothing still opens with
/// every acknowledged write, leaves the orphan where it is, and the next
/// open, which replays the manifest whole, removes it.
fn probe_opens_and_sweeps(tear: TearMode) {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let out = cut_first_flush(&db, tear);
    let orphans = fault::find_ssts(&db);
    assert!(
        !orphans.is_empty() && orphans.iter().all(|p| fault::file_len(p) > 0),
        "{tear:?} must leave a non-empty orphan for this probe, got {orphans:?}",
    );
    assert!(out.acked_count() > 0, "no write was acknowledged");

    match fault::recover_and_validate(&db, opts(8 * 1024), &out.history) {
        Recovery::Recovered(r) => fault::assert_acked_survived(&r, &out.acked()),
        Recovery::RefusedToOpen(e) => {
            panic!("{tear:?}: a first-flush cut is a crash's doing and must open: {e}")
        }
    }

    let d = regolith::Db::open(&db, opts(8 * 1024))
        .unwrap_or_else(|e| panic!("{tear:?}: the second open refused: {e}"));
    drop(d);
    for orphan in &orphans {
        assert!(
            !orphan.exists(),
            "{tear:?}: the open that replays the manifest whole removes {orphan:?}",
        );
    }
}

#[test]
fn a_first_flush_cut_that_tears_a_sector_keeps_every_acknowledged_write() {
    probe_opens(TearMode::TornSector);
}

/// `TearMode::Zero` is the ext4 delayed-allocation shape and
/// `TearMode::Garbage` the harness's harshest synthetic one. Both leave an
/// orphan whose bytes prove nothing about what it held; the manifest's end
/// proves it is no part of the database.
#[test]
fn a_first_flush_cut_that_leaves_an_unprovable_orphan_opens_with_every_acknowledged_write() {
    probe_opens_and_sweeps(TearMode::Zero);
    probe_opens_and_sweeps(TearMode::Garbage);
}

/// Convergence: after a zero-length orphan lets the open through, the
/// database must reach a steady state. Reopening repeatedly, writing
/// through, and closing must keep every write and must never start
/// refusing again.
#[test]
fn an_open_that_dismissed_a_zero_length_orphan_converges_over_repeated_reopens() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let out = cut_first_flush(&db, TearMode::Truncate);
    let orphans = fault::find_ssts(&db);
    assert!(
        orphans.iter().all(|p| fault::file_len(p) == 0),
        "this probe needs the zero-length shape, got {orphans:?}",
    );

    // The pre-crash acknowledged writes must come back before anything
    // else touches the directory; the reopen cycles below then add keys
    // of their own, which the prefix validator would reject.
    let baseline = tmp.path().join("baseline");
    fault::copy_tree(&db, &baseline);
    match fault::recover_and_validate(&baseline, opts(8 * 1024), &out.history) {
        Recovery::Recovered(r) => fault::assert_acked_survived(&r, &out.acked()),
        Recovery::RefusedToOpen(e) => panic!("the dismissed orphan still refused the open: {e}"),
    }

    let mut extra: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for cycle in 0..6u32 {
        let d = regolith::Db::open(&db, opts(8 * 1024)).unwrap_or_else(|e| {
            panic!("cycle {cycle}: reopen refused after the orphan was dismissed: {e}")
        });
        for (k, v) in &extra {
            assert_eq!(
                d.get(k).expect("get"),
                Some(v.clone()),
                "cycle {cycle}: a write from an earlier cycle is gone",
            );
        }
        let k = format!("cycle_{cycle:03}").into_bytes();
        let v = format!("value_{cycle:03}").into_bytes();
        regolith::through_stalls(|| d.put(&k, &v)).expect("put");
        extra.push((k, v));
        d.close().expect("close");
        drop(d);
    }

    let survivors = fault::find_ssts(&db);
    println!(
        "convergence: 6 reopen cycles, {} sst file(s) left, orphan(s) {:?}",
        survivors.len(),
        survivors
            .iter()
            .map(|p| (
                p.file_name().map(|n| n.to_string_lossy().into_owned()),
                fault::file_len(p)
            ))
            .collect::<Vec<_>>(),
    );
}
