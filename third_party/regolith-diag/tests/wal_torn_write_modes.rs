//! Adversarial probe for the torn-tail rule under the tear modes a power cut can leave
//! inside a WAL record.
//!
//! `Wal::replay` treats an *incomplete* trailing record as the end of
//! the log, which is the fix. A record whose bytes are all present but
//! wrong is treated as corruption and refuses the open. Truncation is
//! not the only thing a power cut leaves behind: a device that tears at
//! sector granularity, or a filesystem that allocated blocks it never
//! wrote, leaves the record's full length with garbage or zeros in it.
//! If that shape can reach the last WAL record, an ordinary crash
//! refuses the open and every earlier acknowledged write goes with it.

// The whole file drives the `LD_PRELOAD` syscall shim, which exists only
// on Linux: macOS blocks the loader equivalent under system integrity
// protection and Windows has none. Gated at the file level, like the
// other shim-backed suites, so the tests are absent where the substrate
// cannot run rather than failing there.
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

fn opts() -> Options {
    Options::default().write_buffer_size(1 << 20)
}

fn cut_inside_a_wal_write(db: &std::path::Path, nth: u64, tear: TearMode) -> ChildOutcome {
    let spec = ChildSpec::new(Phase::AfterNPuts, db)
        .durability(DurabilityMode::Immediate)
        .ops(400)
        .value_len(96);
    let out = CrashRun::new(spec)
        .trigger(Trigger::wal_write(nth))
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    let popts = PowerLossOptions::default().tear(tear).sector_bytes(512);
    fault::simulate_power_loss_with(&out.spec.db_path, &out.journal, CutPoint::End, &popts);
    out
}

fn probe(tear: TearMode, nth: u64) -> Result<usize, String> {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let out = cut_inside_a_wal_write(&db, nth, tear);
    if out.acked_count() == 0 {
        return Ok(0);
    }
    match fault::recover_and_validate(&db, opts(), &out.history) {
        Recovery::Recovered(r) => {
            fault::assert_acked_survived(&r, &out.acked());
            Ok(r.k)
        }
        Recovery::RefusedToOpen(e) => Err(format!(
            "{tear:?} at WAL write {nth}: {} acknowledged Immediate-durability writes lost, \
             the database refuses to open: {e}",
            out.acked_count(),
        )),
    }
}

fn sweep(tear: TearMode) {
    let mut bad = Vec::new();
    let mut opened = 0usize;
    let mut trials = 0usize;
    for nth in [1u64, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233] {
        trials += 1;
        match probe(tear, nth) {
            Ok(_) => opened += 1,
            Err(m) => bad.push(m),
        }
    }
    println!(
        "{tear:?}: {trials} cut points, {opened} opened, {} refused",
        bad.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

#[test]
fn a_truncating_cut_inside_a_wal_record_keeps_every_earlier_write() {
    sweep(TearMode::Truncate);
}

#[test]
fn a_zeroing_cut_inside_a_wal_record_keeps_every_earlier_write() {
    sweep(TearMode::Zero);
}

#[test]
fn a_sector_tearing_cut_inside_a_wal_record_keeps_every_earlier_write() {
    sweep(TearMode::TornSector);
}

#[test]
fn a_garbling_cut_inside_a_wal_record_keeps_every_earlier_write() {
    sweep(TearMode::Garbage);
}

/// The blast radius under the default `Eventual` durability. Nothing is
/// acknowledged there, so no write is owed, but the *database* is: a
/// power cut on a filesystem that zero-fills allocated-but-unwritten
/// blocks leaves the whole WAL as zeros, and a zero-filled WAL frames
/// as a whole record with a bad checksum rather than as an incomplete
/// one. If that refuses, an ordinary power cut takes the database
/// offline entirely rather than costing it its unsynced tail.
#[test]
fn a_zeroing_power_cut_under_eventual_durability_still_opens_the_database() {
    let mut refused = Vec::new();
    let mut opened = 0usize;
    for nth in [1u64, 4, 16, 64] {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("db");
        let spec = ChildSpec::new(Phase::AfterNPuts, &db)
            .durability(DurabilityMode::Eventual)
            .ops(400)
            .value_len(96);
        let out = CrashRun::new(spec)
            .trigger(Trigger::wal_write(nth))
            .timeout(Duration::from_secs(180))
            .run();
        out.assert_killed();
        let popts = PowerLossOptions::default()
            .tear(TearMode::Zero)
            .sector_bytes(512);
        fault::simulate_power_loss_with(&out.spec.db_path, &out.journal, CutPoint::End, &popts);

        match fault::recover_and_validate(&db, opts(), &out.history) {
            Recovery::Recovered(_) => opened += 1,
            Recovery::RefusedToOpen(e) => refused.push(format!(
                "cut at WAL write {nth}: the database refuses to open after an ordinary power \
                 cut: {e}"
            )),
        }
    }
    println!(
        "Eventual + Zero: 4 cut points, {opened} opened, {} refused",
        refused.len()
    );
    assert!(refused.is_empty(), "{}", refused.join("\n  "));
}
