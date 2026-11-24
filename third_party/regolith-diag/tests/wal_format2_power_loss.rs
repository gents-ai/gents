//! Power-cut proofs of WAL format 2 replay (plan 4.2), under the
//! `LD_PRELOAD` shim: a child is killed at an exact syscall, the directory
//! is rebuilt the way a power cut would leave it, and the database is
//! reopened.
//!
//! Each test pins one promise of `proofs/tla/WalRecovery.tla`:
//!
//! - Immediate: an acknowledged commit survives any crash the device's
//!   flush honours (`AckedSurvive`), and the open never refuses such a
//!   crash (`RecoveryOpens`).
//! - Eventual: recovery is a gap-free prefix of commit order (`NoGap`).
//! - Residual: damage inside the last synced group reads as a droppable
//!   tail and is reported, never silent (`LossIsReported`).
//! - A length written before its data: a whole final record that fails
//!   its check, with non-zero bytes after it, is a tail, not a refusal.
//! - The truncation of a dropped tail is durable before the next log
//!   exists (RED `NoTruncate`).
//!
//! The tear modes are the ones the reconstruction models: the unsynced
//! bytes dropped (`Truncate`), zeroed (`Zero`), garbled (`Garbage`), or
//! dropped past a sector that keeps half its bytes and garbles the rest
//! (`TornSector`).

// The whole file drives the `LD_PRELOAD` syscall shim, which exists only
// on Linux. Gated at the file level, like the other shim-backed suites.
#![cfg(target_os = "linux")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::fault::{
    self, ChildOutcome, ChildSpec, CrashRun, CutPoint, DieKind, OpKind, Phase, PowerLossOptions,
    Recovery, TearMode, Trigger,
};
use common::wal_format::{self, TailReports};
use regolith::{Db, DurabilityMode, EventListener, Options, Statistics, Ticker};
use tempfile::TempDir;

const OPEN_ONLY: &str = "open_only";

#[test]
fn crash_child() {
    fault::child_entrypoint(|spec| match &spec.phase {
        Phase::Custom(name) if name == OPEN_ONLY => {
            Db::open(&spec.db_path, spec.options())
                .expect("child: open")
                .close()
                .expect("child: close");
        }
        _ => fault::builtin_workload(spec),
    });
}

const TEARS: [TearMode; 4] = [
    TearMode::Truncate,
    TearMode::Zero,
    TearMode::Garbage,
    TearMode::TornSector,
];

/// Kill points: the nth write to a log, spread over the whole run.
const CUTS: [u64; 13] = [1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233, 377];

/// Options that record what an open discards.
fn reporting(opts: Options) -> (Options, Arc<TailReports>, Arc<Statistics>) {
    let reports = TailReports::new();
    let stats = Arc::new(Statistics::new());
    let opts = opts
        .listeners(vec![reports.clone() as Arc<dyn EventListener>])
        .statistics(Some(Arc::clone(&stats)));
    (opts, reports, stats)
}

/// Kill a child running `spec` at `trigger` and reconstruct the power cut.
fn crash(spec: ChildSpec, trigger: Trigger, tear: TearMode) -> ChildOutcome {
    let out = CrashRun::new(spec)
        .trigger(trigger)
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    let popts = PowerLossOptions::default().tear(tear).sector_bytes(512);
    fault::simulate_power_loss_with(&out.spec.db_path, &out.journal, CutPoint::End, &popts);
    out
}

/// The `Immediate` promise at every cut and every tear: the open never
/// refuses, every acknowledged write survives, and whatever the cut left
/// of the newest log past the last sync is dropped and reported exactly.
#[test]
fn immediate_every_tear_at_every_cut_keeps_every_acknowledged_write() {
    let mut failures = Vec::new();
    let mut reported = 0usize;
    for tear in TEARS {
        for nth in CUTS {
            let tmp = TempDir::new().unwrap();
            let db = tmp.path().join("db");
            let spec = ChildSpec::new(Phase::AfterNPuts, &db)
                .durability(DurabilityMode::Immediate)
                .ops(400)
                .value_len(96);
            let out = crash(spec, Trigger::wal_write(nth), tear);
            let wal = fault::newest_wal(&db);
            let on_disk = std::fs::read(&wal).unwrap();
            let (opts, reports, stats) = reporting(out.spec.options());
            match fault::recover_and_validate(&db, opts, &out.history) {
                Recovery::Recovered(r) => fault::assert_acked_survived(&r, &out.acked()),
                Recovery::RefusedToOpen(e) => {
                    failures.push(format!("{tear:?} at write {nth}: refused: {e}"));
                    continue;
                }
            }
            // The kill landed after a write and before its sync, so the
            // synced prefix ends on a record boundary, and every byte past
            // it the file still holds is a tail the open must report.
            let whole = whole_prefix(&on_disk);
            let taken = reports.taken();
            if whole < on_disk.len() {
                reported += 1;
                let expected = (on_disk.len() - whole) as u64;
                if taken.len() != 1 || taken[0].offset != whole as u64 {
                    failures.push(format!(
                        "{tear:?} at write {nth}: {expected} bytes past offset {whole} \
                         dropped, reported {taken:?}"
                    ));
                } else if stats.get_ticker(Ticker::WalTailDiscardedBytes) != expected {
                    failures.push(format!("{tear:?} at write {nth}: ticker disagrees"));
                }
            } else if !taken.is_empty() {
                failures.push(format!(
                    "{tear:?} at write {nth}: nothing dropped, reported {taken:?}"
                ));
            }
        }
    }
    println!(
        "Immediate: {} cuts, {reported} reported tails, {} failures",
        TEARS.len() * CUTS.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n  "));
}

/// Where the whole records of a format 2 log end: at the end of the last
/// record whose header and payload fit, read by the format's own layout.
/// A torn or garbled tail ends the walk; the test log's records are
/// whole groups, so this is the boundary the synced prefix ends on.
fn whole_prefix(bytes: &[u8]) -> usize {
    if !wal_format::is_format_2(bytes) {
        return 0;
    }
    let nonce = wal_format::nonce_of(bytes);
    let mut at = wal_format::STAMP_LEN;
    while at + wal_format::HEADER_LEN <= bytes.len() {
        let header = &bytes[at..at + wal_format::HEADER_LEN];
        let stored = u32::from_le_bytes(header[17..21].try_into().unwrap());
        if stored != wal_format::header_check(nonce, at as u64, header) {
            break;
        }
        let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let end = at + wal_format::HEADER_LEN + len;
        if end > bytes.len() {
            break;
        }
        at = end;
    }
    at
}

/// The `Eventual` promise at every cut and every tear, with memtable
/// rotations in the run, so sealed logs synced by a rotation sit beside a
/// newest log nothing synced: the open never refuses and serves a
/// gap-free prefix of the writes.
#[test]
fn eventual_every_tear_at_every_cut_opens_on_a_gap_free_prefix() {
    let mut failures = Vec::new();
    let mut kept = Vec::new();
    for tear in TEARS {
        for nth in CUTS {
            let tmp = TempDir::new().unwrap();
            let db = tmp.path().join("db");
            let spec = ChildSpec::new(Phase::AfterNPuts, &db)
                .durability(DurabilityMode::Eventual)
                .ops(400)
                .value_len(96)
                .write_buffer_size(16 * 1024);
            let out = crash(spec, Trigger::wal_write(nth), tear);
            let (opts, _, _) = reporting(out.spec.options());
            match fault::recover_and_validate(&db, opts, &out.history) {
                Recovery::Recovered(r) => kept.push(r.k),
                Recovery::RefusedToOpen(e) => {
                    failures.push(format!("{tear:?} at write {nth}: refused: {e}"))
                }
            }
        }
    }
    println!(
        "Eventual: {} cuts, prefixes kept {kept:?}, {} failures",
        TEARS.len() * CUTS.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n  "));
}

/// A length written before its data: the cut leaves the unsynced record's
/// header on disk, its length intact, while its payload never arrived, and
/// non-zero bytes after it:
/// what OPFS's slot header, FAT, ext4 `data=writeback` and a volatile
/// device cache leave when a length reaches the device before its data.
/// Format 1 refused this state; format 2 drops it as a tail above P and
/// keeps every acknowledged write.
#[test]
fn a_whole_final_record_with_garbage_after_it_opens_as_a_tail() {
    for nth in [2u64, 5, 13, 34, 89] {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("db");
        let spec = ChildSpec::new(Phase::AfterNPuts, &db)
            .durability(DurabilityMode::Immediate)
            .ops(400)
            .value_len(96);
        let out = CrashRun::new(spec)
            .trigger(Trigger::wal_write(nth))
            .timeout(Duration::from_secs(180))
            .run();
        out.assert_killed();
        let wal = fault::newest_wal(&db);
        let written = std::fs::read(&wal).unwrap();
        let popts = PowerLossOptions::default().tear(TearMode::Truncate);
        fault::simulate_power_loss_with(&db, &out.journal, CutPoint::End, &popts);
        // Before the first sync the stamp is unsynced too; it shares the
        // record's first sector, so it reached the device with the length.
        let synced = std::fs::read(&wal)
            .unwrap()
            .len()
            .max(wal_format::STAMP_LEN);
        assert!(
            synced < written.len(),
            "write {nth} must leave an unsynced record"
        );

        let mut state = written[..synced + wal_format::HEADER_LEN].to_vec();
        state.extend_from_slice(&fault::garbage(nth, written.len() - state.len()));
        state.extend_from_slice(&fault::garbage(!nth, 4096));
        std::fs::write(&wal, &state).unwrap();

        let (opts, reports, _) = reporting(out.spec.options());
        match fault::recover_and_validate(&db, opts, &out.history) {
            Recovery::Recovered(r) => fault::assert_acked_survived(&r, &out.acked()),
            Recovery::RefusedToOpen(e) => panic!("at write {nth}: refused: {e}"),
        }
        let taken = reports.taken();
        assert_eq!(taken.len(), 1, "write {nth}");
        assert_eq!(taken[0].offset, synced as u64, "write {nth}");
        assert_eq!(
            taken[0].discarded_bytes,
            (state.len() - synced) as u64,
            "write {nth}"
        );
    }
}

/// Residual: after a power cut, rot inside the last synced group. Nothing
/// after it proves it synced, so the open drops it and reports the drop:
/// the loss of an acknowledged write there is never silent. The same rot
/// one group earlier is proved synced by the group after it, and refuses.
#[test]
fn residual_damage_in_the_last_synced_group_is_reported_never_silent() {
    for nth in [5u64, 21, 144] {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("db");
        let spec = ChildSpec::new(Phase::AfterNPuts, &db)
            .durability(DurabilityMode::Immediate)
            .delete_every(0)
            .ops(400)
            .value_len(96);
        let out = crash(spec, Trigger::wal_write(nth), TearMode::Truncate);
        let wal = fault::newest_wal(&db);
        let pristine = std::fs::read(&wal).unwrap();
        let manifest = db.join("MANIFEST");
        let pristine_manifest = std::fs::read(&manifest).unwrap();
        let bounds = wal_format::record_bounds(&pristine);
        assert!(bounds.len() >= 3, "write {nth}: needs two whole groups");
        let last = bounds[bounds.len() - 2];
        let before = bounds[bounds.len() - 3];

        let mut rotted = pristine.clone();
        rotted[last + wal_format::HEADER_LEN + 2] ^= 0x10;
        std::fs::write(&wal, &rotted).unwrap();
        let (opts, reports, stats) = reporting(out.spec.options());
        let k = match fault::recover_and_validate(&db, opts, &out.history) {
            Recovery::Recovered(r) => r.k,
            Recovery::RefusedToOpen(e) => panic!("write {nth}: refused: {e}"),
        };
        assert_eq!(
            k + 1,
            out.acked_count(),
            "write {nth}: only the rotted group is lost"
        );
        let taken = reports.taken();
        assert_eq!(taken.len(), 1, "write {nth}: the loss must be reported");
        assert_eq!(taken[0].offset, last as u64);
        assert_eq!(stats.get_ticker(Ticker::WalTailDiscarded), 1);

        // Reopening dropped and truncated it, and recorded the log as
        // retired in the manifest; plant the rot one group earlier in the
        // pristine log, beside the pristine manifest, instead.
        let mut rotted = pristine.clone();
        rotted[before + wal_format::HEADER_LEN + 2] ^= 0x10;
        std::fs::write(&wal, &rotted).unwrap();
        std::fs::write(&manifest, &pristine_manifest).unwrap();
        for extra in fault::find_wals(&db) {
            if extra != wal {
                std::fs::remove_file(extra).unwrap();
            }
        }
        match fault::recover_and_validate(&db, out.spec.options(), &out.history) {
            Recovery::RefusedToOpen(e) => {
                assert!(e.contains(&format!("damaged at offset {before}")), "{e}")
            }
            Recovery::Recovered(r) => panic!(
                "write {nth}: rot a later group proves synced opened on prefix {}",
                r.k
            ),
        }
    }
}

/// RED `NoTruncate`: recovery drops the newest log's damaged tail, and the
/// power goes after the next log is created but before the old logs are
/// removed. The old log is then an earlier log, which must be complete,
/// so the truncation has to be durable before the next log exists. The
/// journal shows the order, and the reopen shows why it matters.
#[test]
fn red_no_truncate_the_dropped_tail_is_truncated_durably_before_the_next_log() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::AfterNPuts, &db)
        .durability(DurabilityMode::Immediate)
        .ops(200)
        .value_len(96);
    let first = crash(spec, Trigger::wal_write(55), TearMode::Garbage);
    let damaged = fault::newest_wal(&db);
    let synced = whole_prefix(&std::fs::read(&damaged).unwrap());
    assert!(
        (synced as u64) < fault::file_len(&damaged),
        "the first cut must leave a tail to drop"
    );

    // Recovery: truncate and sync the old log (the first log fsync), then
    // create the next log and sync its rewrite (the second). The power
    // goes just before that second sync.
    let mut spec = first.spec.clone();
    spec.phase = Phase::Custom(OPEN_ONLY.to_string());
    let recovering = CrashRun::new(spec)
        .trigger(Trigger::Syscall {
            kind: DieKind::Fsync,
            path_contains: "/wal/".to_string(),
            nth: 2,
            before: true,
        })
        .timeout(Duration::from_secs(180))
        .run();
    recovering.assert_killed();

    let records = &recovering.journal.records;
    let seq_of = |kind: OpKind, path: &std::path::Path| {
        records
            .iter()
            .find(|r| r.kind == kind && r.path == path && r.succeeded())
            .map(|r| r.seq)
    };
    let truncated = seq_of(OpKind::Truncate, &damaged).expect("the tail is truncated");
    let synced_at = records
        .iter()
        .find(|r| r.kind == OpKind::Sync && r.path == damaged && r.seq > truncated)
        .map(|r| r.seq)
        .expect("the truncation is synced");
    let created = records
        .iter()
        .find(|r| {
            r.kind == OpKind::Open
                && r.opened_creating()
                && r.path != damaged
                && r.path.to_string_lossy().contains("/wal/")
        })
        .map(|r| r.seq)
        .expect("the next log is created");
    assert!(
        truncated < synced_at && synced_at < created,
        "truncate at {truncated}, sync at {synced_at}, next log at {created}"
    );

    fault::simulate_power_loss_with(
        &db,
        &recovering.journal,
        CutPoint::End,
        &PowerLossOptions::default().tear(TearMode::Garbage),
    );
    assert!(
        fault::find_wals(&db).len() >= 2,
        "both logs survive the cut"
    );
    match fault::recover_and_validate(&db, first.spec.options(), &first.history) {
        Recovery::Recovered(r) => fault::assert_acked_survived(&r, &first.acked()),
        Recovery::RefusedToOpen(e) => {
            panic!("the truncated log must replay as an earlier log: {e}")
        }
    }
}

/// A clean close syncs every record and then appends CLOSE, so under
/// `Eventual` a power cut after `close` returns keeps every write, and
/// the log replays without a tail.
#[test]
fn a_clean_close_makes_every_eventual_write_durable() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::CleanExit, &db)
        .durability(DurabilityMode::Eventual)
        .ops(300)
        .value_len(96);
    let out = CrashRun::new(spec)
        .trigger(Trigger::None)
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_clean();
    fault::simulate_power_loss_with(
        &db,
        &out.journal,
        CutPoint::End,
        &PowerLossOptions::default().tear(TearMode::Garbage),
    );
    let wal = std::fs::read(fault::newest_wal(&db)).unwrap();
    let bounds = wal_format::record_bounds(&wal);
    assert_eq!(
        wal_format::kind_at(&wal, bounds[bounds.len() - 2]),
        wal_format::KIND_CLOSE,
        "CLOSE ends a cleanly closed log"
    );
    let (opts, reports, _) = reporting(out.spec.options());
    match fault::recover_and_validate(&db, opts, &out.history) {
        Recovery::Recovered(r) => assert_eq!(r.k, out.history.len()),
        Recovery::RefusedToOpen(e) => panic!("refused: {e}"),
    }
    assert!(reports.taken().is_empty());
}
