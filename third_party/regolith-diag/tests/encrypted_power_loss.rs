//! Power cuts on a database encrypted at rest.
//!
//! The rule `proofs/tla/WalRecovery.tla` checks with `Frame = "AEAD"`: a
//! record whose tag fails is an unusable record under the O < P rule, so a
//! power cut leaves a database that opens under the right key with every
//! acknowledged write (`RecoveryOpens`, `AckedSurvive`) and a gap-free
//! prefix (`NoGap`), and one that never opens under the wrong key
//! (`WrongKeyRefuses`). RED `StampNotSealed` is the reason a sealed log's
//! stamp is sealed and durable before the log exists: the cuts below land
//! on both sides of every step of a sealed log's creation (the staging
//! write, its sync, the first record after the rename).
//!
//! Every cut is reconstructed from the syscalls the child really issued, by
//! the `LD_PRELOAD` shim's journal, under each way a device can leave the
//! unsynced bytes.

// The whole file drives the `LD_PRELOAD` syscall shim, which exists only
// on Linux. Gated at the file level, like the other shim-backed suites.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;
use std::time::Duration;

use common::fault::{
    self, ChildOutcome, ChildSpec, CrashRun, CutPoint, DieKind, Phase, PowerLossOptions,
    PowerLossReport, Recovery, TearMode, Trigger,
};
use common::keys::Keys;
use regolith::{DurabilityMode, Options};
use tempfile::TempDir;

const CHILD_TIMEOUT: Duration = Duration::from_secs(180);

const TEARS: [TearMode; 4] = [
    TearMode::Truncate,
    TearMode::Zero,
    TearMode::Garbage,
    TearMode::TornSector,
];

#[test]
fn crash_child() {
    fault::child_entrypoint(fault::builtin_workload);
}

fn crash_and_cut(
    spec: ChildSpec,
    trigger: Trigger,
    tear: TearMode,
) -> (ChildOutcome, PowerLossReport) {
    let out = CrashRun::new(spec.encrypted(true))
        .trigger(trigger)
        .timeout(CHILD_TIMEOUT)
        .run();
    out.assert_killed();
    let opts = PowerLossOptions::default().tear(tear);
    let report =
        fault::simulate_power_loss_with(&out.spec.db_path, &out.journal, CutPoint::End, &opts);
    (out, report)
}

fn reopen_options(out: &ChildOutcome, keys: std::sync::Arc<Keys>) -> Options {
    Options::default()
        .write_buffer_size(out.spec.write_buffer_size)
        .durability(out.spec.durability)
        .key_provider(keys)
}

/// The wrong key first, which must refuse and write nothing, then the right
/// key, which must open on a valid prefix. Returns that prefix.
fn recover_both_ways(db: &Path, out: &ChildOutcome, context: &str) -> fault::PrefixReport {
    match fault::recover_and_validate(db, reopen_options(out, Keys::wrong(&[1])), &out.history) {
        Recovery::RefusedToOpen(_) => {}
        Recovery::Recovered(r) => panic!(
            "{context}: the database opened under the wrong key, holding {} writes",
            r.k
        ),
    }
    match fault::recover_and_validate(db, reopen_options(out, Keys::new(&[1])), &out.history) {
        Recovery::Recovered(r) => r,
        Recovery::RefusedToOpen(e) => panic!(
            "{context}: the database refuses to open under the right key after a power cut: {e}"
        ),
    }
}

/// `DurabilityMode::Immediate` on an encrypted store: a write that returned
/// `Ok` survives a power cut at any WAL write, under every tear.
#[test]
fn immediate_writes_survive_every_cut_and_tear_and_a_wrong_key_never_opens() {
    let tmp = TempDir::new().unwrap();
    for nth in [3u64, 11, 29, 67, 131] {
        for tear in TEARS {
            let db = tmp.path().join(format!("db_{nth}_{tear:?}"));
            let spec = ChildSpec::new(Phase::Custom("encrypted_wal_kill".into()), &db)
                .ops(200)
                .durability(DurabilityMode::Immediate);
            let (out, report) = crash_and_cut(spec, Trigger::wal_write(nth), tear);
            assert!(
                out.acked_count() > 0,
                "no write was acknowledged before the crash at WAL write {nth}"
            );
            let context = format!("WAL write {nth}, {tear:?}");
            let r = recover_both_ways(&db, &out, &context);
            if let Err(e) = r.covers_acked(&out.acked()) {
                panic!("{context}: {e}\n{}", report.summary());
            }
        }
    }
}

/// A cut inside a memtable flush, with the table being written sealed and
/// the manifest's batches sealed, keeps every acknowledged write.
#[test]
fn a_cut_inside_a_flush_keeps_every_acknowledged_write() {
    let tmp = TempDir::new().unwrap();
    for tear in TEARS {
        let db = tmp.path().join(format!("db_{tear:?}"));
        let spec = ChildSpec::new(Phase::DuringFlush, &db).durability(DurabilityMode::Immediate);
        let (out, report) = crash_and_cut(spec, Trigger::sst_write(3), tear);
        assert!(out.acked_count() > 0);
        let context = format!("third flush, {tear:?}");
        let r = recover_both_ways(&db, &out, &context);
        if let Err(e) = r.covers_acked(&out.acked()) {
            panic!("{context}: {e}\n{}", report.summary());
        }
    }
}

/// RED `StampNotSealed`, on the real write path. A rotation creates the next
/// sealed log: its stamp is written to a staging file, synced, renamed into
/// place and the directory synced, and only then does the log take a record.
/// A cut on either side of each step leaves no log, or a log whose sealed
/// stamp is whole and durable: the right key opens on a valid prefix that
/// keeps the log the rotation sealed, and the wrong key refuses rather than
/// reading the newest log as a torn tail and dropping it.
#[test]
fn a_cut_around_a_sealed_logs_creation_opens_under_the_right_key_only() {
    // The open creates log 1 and the first rotation log 2, the flush after it
    // takes table id 3, so the second rotation creates log 4. Named in full:
    // the temporary directory's own name contains `.tmp`.
    let at = |kind: DieKind, file: &str, nth: u64, before: bool| Trigger::Syscall {
        kind,
        path_contains: file.to_string(),
        nth,
        before,
    };
    let triggers = [
        (
            "before the staging write",
            at(DieKind::Write, "wal_000002.tmp", 1, true),
        ),
        (
            "after the staging write",
            at(DieKind::Write, "wal_000002.tmp", 1, false),
        ),
        (
            "before the staging sync",
            at(DieKind::Fsync, "wal_000002.tmp", 1, true),
        ),
        (
            "after the staging sync",
            at(DieKind::Fsync, "wal_000002.tmp", 1, false),
        ),
        (
            "before the first record of the new log",
            at(DieKind::Write, "wal_000002.log", 1, true),
        ),
        (
            "after the first record of the new log",
            at(DieKind::Write, "wal_000002.log", 1, false),
        ),
        (
            "before the next rotation's staging sync",
            at(DieKind::Fsync, "wal_000004.tmp", 1, true),
        ),
        (
            "before the first record of the log after it",
            at(DieKind::Write, "wal_000004.log", 1, true),
        ),
    ];
    let tmp = TempDir::new().unwrap();
    for (i, (name, trigger)) in triggers.into_iter().enumerate() {
        for tear in [TearMode::Truncate, TearMode::Garbage, TearMode::TornSector] {
            let db = tmp.path().join(format!("db_{i}_{tear:?}"));
            let spec = ChildSpec::new(Phase::DuringFlush, &db).delete_every(0);
            let (out, _) = crash_and_cut(spec, trigger.clone(), tear);
            let context = format!("{name}, {tear:?}");
            // Every cut here follows the first rotation's sync of log 1.
            let r = recover_both_ways(&db, &out, &context);
            assert!(r.k > 0, "{context}: the log the rotation sealed was lost");
            let staged: Vec<_> = std::fs::read_dir(db.join("wal"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
                .collect();
            assert!(
                staged.is_empty(),
                "{context}: the open left staging files behind: {staged:?}"
            );
        }
    }
}

/// Cuts inside compactions and manifest writes, with every table and batch
/// sealed, leave a database that opens on a valid prefix under the right key
/// and never under the wrong one.
#[test]
fn a_cut_inside_a_compaction_or_a_manifest_write_recovers_a_valid_prefix() {
    let tmp = TempDir::new().unwrap();
    let cases = [
        (
            "compaction table write",
            Phase::DuringCompaction,
            Trigger::sst_write(12),
        ),
        (
            "manifest write",
            Phase::DuringManifestWrite,
            Trigger::manifest_write(6),
        ),
    ];
    for (i, (name, phase, trigger)) in cases.into_iter().enumerate() {
        for tear in [TearMode::Truncate, TearMode::TornSector] {
            let db = tmp.path().join(format!("db_{i}_{tear:?}"));
            let spec = ChildSpec::new(phase.clone(), &db);
            let (out, _) = crash_and_cut(spec, trigger.clone(), tear);
            recover_both_ways(&db, &out, &format!("{name}, {tear:?}"));
        }
    }
}
