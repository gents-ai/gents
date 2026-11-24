//! Power cuts at every step of a close that seals its last memtable in
//! place (E46), under the `LD_PRELOAD` shim: a child writes, closes, and is
//! killed at an exact syscall of the close; the directory is rebuilt the way
//! a power cut would leave it, and the database is reopened.
//!
//! The close leaves the log its writes went to unsynced and the newest
//! until the flush's manifest batch, durable, says recovery skips it; only
//! then is the log removed and the next log created and closed with one
//! sync (`proofs/tla/WalRecovery.tla`, `SealInPlace`, `FlushInPlace`,
//! `CloseNext`). The workload syncs its log once half way, so the log has a
//! durable stamp and prefix and an unsynced tail: the shape in which a torn
//! tail is forgiven only in the newest log. At every kill point of the
//! close:
//!
//! - the open never refuses (`RecoveryOpens`): no newer log exists before
//!   the flush is durable (RED `NewLogBeforeFlush`), and a torn stamp of the
//!   next log reads as a log nothing of which was synced (RED
//!   `UnstampedRefuses`);
//! - what it recovers is a gap-free prefix of the writes (`NoGap`) holding
//!   every write the sync covered, and under Immediate every acknowledged
//!   write (`AckedSurvive`);
//! - once the log the writes went to was removed, every write survives
//!   whatever the durability: the removal follows the durable flush.

// The whole file drives the `LD_PRELOAD` syscall shim, which exists only
// on Linux. Gated at the file level, like the other shim-backed suites.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;
use std::time::Duration;

use common::fault::{
    self, Acks, ChildOutcome, ChildSpec, CrashRun, CutPoint, DieKind, OpKind, OpValue, Phase,
    PowerLossOptions, Recovery, TearMode, Trigger,
};
use regolith::{Db, DurabilityMode, WriteBatch};
use tempfile::TempDir;

const CLOSE_AFTER_A_SYNC: &str = "close_after_a_sync";

/// The writes in the workload, and the one written with a sync.
const OPS: usize = 200;
const SYNCED_AT: usize = 99;

#[test]
fn crash_child() {
    fault::child_entrypoint(|spec| match &spec.phase {
        Phase::Custom(name) if name == CLOSE_AFTER_A_SYNC => close_after_a_sync(spec),
        _ => fault::builtin_workload(spec),
    });
}

/// The child: the planned writes one by one, write `SYNCED_AT` with a sync
/// and the rest as the spec's durability says, each acknowledged once it
/// returns; then a clean close.
fn close_after_a_sync(spec: &ChildSpec) {
    let db = Db::open(&spec.db_path, spec.options()).expect("child: open");
    let mut acks = Acks::open(&spec.ack_path);
    for (i, op) in spec.history().ops().iter().enumerate() {
        let mut batch = WriteBatch::new();
        match &op.value {
            OpValue::Put(value) => batch.put(&op.key, value),
            OpValue::Delete => batch.delete(&op.key),
        }
        let durability = if i == SYNCED_AT {
            DurabilityMode::Immediate
        } else {
            spec.durability
        };
        regolith::through_stalls(|| db.write_with_durability(batch.clone(), durability))
            .expect("child: write");
        acks.ack([i as u64]);
    }
    db.close().expect("child: close");
}

/// The ways a power cut leaves unsynced bytes, and whether files created
/// without a sync of their directory are lost too.
const CUTS: [(TearMode, bool); 4] = [
    (TearMode::Truncate, false),
    (TearMode::Garbage, false),
    (TearMode::Garbage, true),
    (TearMode::TornSector, false),
];

fn spec(db: &Path, durability: DurabilityMode) -> ChildSpec {
    ChildSpec::new(Phase::Custom(CLOSE_AFTER_A_SYNC.to_string()), db)
        .durability(durability)
        .ops(OPS)
        .value_len(96)
        .workers(0)
}

/// Run the workload to its end, and count the syscalls of `kind` on paths
/// holding `path` that came before the close: the kills start after them.
fn before_close(durability: DurabilityMode, kind: &DieKind, path: &str) -> u64 {
    let tmp = TempDir::new().unwrap();
    let out = CrashRun::new(spec(&tmp.path().join("db"), durability))
        .timeout(Duration::from_secs(120))
        .run();
    out.assert_clean();
    let records = &out.journal.records;
    // The close begins after the last write to the log the writes went to.
    let start = records
        .iter()
        .rposition(|r| r.kind == OpKind::Write && r.path.ends_with("wal_000001.log"))
        .expect("the writes reached the log");
    let op = match kind {
        DieKind::Write => OpKind::Write,
        DieKind::Fsync => OpKind::Sync,
        DieKind::Open => OpKind::Open,
        DieKind::Truncate => OpKind::Truncate,
    };
    records[..=start]
        .iter()
        .filter(|r| r.kind == op && r.path.to_string_lossy().contains(path))
        .count() as u64
}

/// Run the workload killed at `trigger`, or `None` when the trigger never
/// fired: the run is past its last matching syscall.
fn killed(db: &Path, durability: DurabilityMode, trigger: Trigger) -> Option<ChildOutcome> {
    let out = CrashRun::new(spec(db, durability))
        .trigger(trigger)
        .timeout(Duration::from_secs(120))
        .run();
    if out.was_killed() {
        Some(out)
    } else {
        out.assert_clean();
        None
    }
}

/// The checks every kill point gets. Returns a failure, if any.
fn check(out: &ChildOutcome, db: &Path, tear: TearMode, drop_creates: bool) -> Option<String> {
    let mut popts = PowerLossOptions::default().tear(tear).sector_bytes(512);
    popts.drop_unsynced_creates = drop_creates;
    fault::simulate_power_loss_with(db, &out.journal, CutPoint::End, &popts);
    // The log the writes went to was removed before the kill: the flush
    // that made it redundant was durable by then.
    let removed =
        out.journal.records.iter().any(|r| {
            r.kind == OpKind::Unlink && r.succeeded() && r.path.ends_with("wal_000001.log")
        });
    match fault::recover_and_validate(db, out.spec.options(), &out.history) {
        Recovery::RefusedToOpen(e) => Some(format!("refused: {e}")),
        Recovery::Recovered(report) => {
            let synced: Vec<usize> = (0..=SYNCED_AT).collect();
            if let Err(e) = report.covers_acked(&synced) {
                return Some(format!("lost a synced write: {e}"));
            }
            if out.spec.durability == DurabilityMode::Immediate
                && let Err(e) = report.covers_acked(&out.acked())
            {
                return Some(format!("lost an acknowledged write: {e}"));
            }
            if removed {
                let every: Vec<usize> = (0..out.history.len()).collect();
                if let Err(e) = report.covers_acked(&every) {
                    return Some(format!("lost a write after the log was removed: {e}"));
                }
            }
            None
        }
    }
}

/// Kill the close at every syscall of `kind` on a path holding `path`,
/// before and after the call, under every way a cut can leave the bytes.
/// Returns how many kill points the close had, and the failures.
fn every_kill(kind: DieKind, path: &str, durability: DurabilityMode) -> (usize, Vec<String>) {
    let from = before_close(durability, &kind, path) + 1;
    let mut failures = Vec::new();
    let mut points = 0;
    for before in [false, true] {
        for nth in from.. {
            let mut fired = false;
            for (tear, drop_creates) in CUTS {
                let tmp = TempDir::new().unwrap();
                let db = tmp.path().join("db");
                let trigger = Trigger::Syscall {
                    kind: kind.clone(),
                    path_contains: path.to_string(),
                    nth,
                    before,
                };
                let Some(out) = killed(&db, durability, trigger) else {
                    break;
                };
                fired = true;
                if let Some(failure) = check(&out, &db, tear, drop_creates) {
                    failures.push(format!(
                        "{durability:?} {kind:?} {path} #{nth} before={before} {tear:?} \
                         drop_creates={drop_creates}: {failure}"
                    ));
                }
            }
            if !fired {
                break;
            }
            points += 1;
        }
    }
    (points, failures)
}

/// Every kill point of the close under `durability`: each sync, each file
/// opened, and each write to the manifest, a table or a log.
fn every_step_of_the_close(durability: DurabilityMode) {
    let mut failures = Vec::new();
    let mut points = 0;
    for (kind, path) in [
        (DieKind::Fsync, "/db"),
        (DieKind::Open, "/db"),
        (DieKind::Write, "MANIFEST"),
        (DieKind::Write, ".sst"),
        (DieKind::Write, "/wal/"),
    ] {
        let (n, f) = every_kill(kind, path, durability);
        points += n;
        failures.extend(f);
    }
    println!("{durability:?}: {points} kill points in the close");
    assert!(
        points >= 20,
        "the close was reached at only {points} kill points"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn eventual_every_step_of_the_close_opens_with_a_prefix() {
    every_step_of_the_close(DurabilityMode::Eventual);
}

#[test]
fn immediate_every_step_of_the_close_keeps_every_acknowledged_write() {
    every_step_of_the_close(DurabilityMode::Immediate);
}
