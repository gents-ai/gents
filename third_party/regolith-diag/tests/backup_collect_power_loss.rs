//! Power cuts while a purge collects the shared tables no backup lists.
//!
//! The child takes three backups of one database: backup 1 of the first
//! half of its writes, then, after the rest are written and compacted into
//! new tables, backups 2 and 3 of the whole (3 shares every table of 2).
//! It then purges all but the newest, which deletes backup 1, whose tables
//! no other backup lists, and backup 2, whose tables backup 3 still lists.
//! The purge removes both backups' metadata and syncs `meta/` once, then
//! sweeps `shared/` a batch of tables at a time: for each batch it walks
//! `meta/` one entry at a time, reading every remaining listing and
//! striking what it names, and only then removes what is left and syncs
//! `shared/`. The child runs under the `LD_PRELOAD` shim, which kills it at
//! a chosen call: as the metadata goes, while the sweep reads the listing,
//! or as the removals are synced. The directory is then rebuilt as the
//! filesystem would have left it, every byte never synced discarded.
//! Whatever the cut:
//!
//! - every backup still listed restores to exactly the writes before it,
//!   so the collection removed no table a listed backup names
//!   (`proofs/tla/BackupSeal.tla`, `ListedRestores`);
//! - the purge runs again over what the cut left and finishes, leaving
//!   backup 3, which restores to every write.
//!
//! The cut points are counted in calls read off one uncut run of the same
//! workload, so a change in how many tables a backup copies moves them
//! with it.
//!
//! # Linux only
//!
//! `LD_PRELOAD` interposition is a glibc mechanism; the file is compiled out
//! elsewhere. See `power_loss.rs` for what the shim does and does not prove.
#![cfg(target_os = "linux")]

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::fault::{
    self, ChildSpec, CrashRun, CutPoint, DieKind, History, Journal, OpKind, OpValue, Phase,
    PowerLossOptions, TearMode, Trigger, WriteOp,
};
use regolith::{BackupEngine, BackupId, Db, Options};
use tempfile::TempDir;

const PURGE: &str = "backup_collect_cut";
const TEARS: [TearMode; 2] = [TearMode::Truncate, TearMode::TornSector];

#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

fn dispatch(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == PURGE => workload(spec),
        _ => fault::builtin_workload(spec),
    }
}

fn db_dir(root: &Path) -> PathBuf {
    root.join("db")
}

fn backup_dir(root: &Path) -> PathBuf {
    root.join("backups")
}

/// Beside the root, so the shim records none of them and a power cut
/// never touches the record of what returned.
fn marker(root: &Path, what: &str) -> PathBuf {
    root.with_extension(what)
}

fn mark(root: &Path, what: &str) {
    let path = marker(root, what);
    std::fs::write(&path, b"1").expect("child: marker");
    std::fs::File::open(&path)
        .and_then(|f| f.sync_all())
        .expect("child: sync marker");
}

/// Small tables and no background compaction, so every run issues the same
/// calls in the same order and a cut counted in calls lands in the same
/// place.
fn db_options(spec: &ChildSpec) -> Options {
    Options::default()
        .write_buffer_size(spec.write_buffer_size)
        .durability(spec.durability)
        .max_background_compactions(0)
}

fn apply(db: &Db, op: &WriteOp) {
    match &op.value {
        OpValue::Put(v) => regolith::through_stalls(|| db.put(&op.key, v)).expect("child: put"),
        OpValue::Delete => regolith::through_stalls(|| db.delete(&op.key)).expect("child: delete"),
    }
}

fn workload(spec: &ChildSpec) {
    let root = &spec.db_path;
    let db = Db::open(db_dir(root), db_options(spec)).expect("child: open");
    let history = spec.history();
    let ops = history.ops();
    let half = ops.len() / 2;
    ops[..half].iter().for_each(|op| apply(&db, op));
    db.flush().expect("child: flush");
    let mut engine = BackupEngine::open(backup_dir(root)).expect("child: backup engine");
    engine.create_backup(&db).expect("child: backup 1");
    ops[half..].iter().for_each(|op| apply(&db, op));
    // New tables for backups 2 and 3, none of which backup 1 lists.
    db.compact_range(None, None).wait().expect("child: compact");
    engine.create_backup(&db).expect("child: backup 2");
    engine.create_backup(&db).expect("child: backup 3");
    mark(root, "backups");
    engine.purge_old_backups(1).expect("child: purge");
    mark(root, "purged");
    fault::kill_self();
}

fn spec(root: &Path) -> ChildSpec {
    ChildSpec::new(Phase::Custom(PURGE.to_string()), root)
        .ops(240)
        .value_len(64)
        .write_buffer_size(4 * 1024)
}

/// The calls one run issues when nothing cuts it short but the workload's
/// own end.
fn probe() -> Journal {
    let tmp = TempDir::new().unwrap();
    let out = CrashRun::new(spec(&tmp.path().join("root")))
        .trigger(Trigger::Workload)
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    assert!(
        marker(&out.spec.db_path, "purged").exists(),
        "the uncut run did not finish"
    );
    out.journal
}

/// Where to cut: the `nth` call of `kind` whose path holds `needle`.
fn at_call(kind: DieKind, needle: &str, nth: u64, before: bool) -> Trigger {
    Trigger::Syscall {
        kind,
        path_contains: needle.to_string(),
        nth,
        before,
    }
}

/// How many of the probe's calls of `kind` to `needle` come before the
/// call with sequence number `seq`.
fn calls_before(journal: &Journal, kind: OpKind, needle: &str, seq: u64) -> u64 {
    journal
        .records
        .iter()
        .filter(|r| {
            r.kind == kind
                && r.succeeded()
                && r.seq < seq
                && r.path.to_string_lossy().contains(needle)
        })
        .count() as u64
}

/// The sequence number of the probe's first `kind` call to `needle`.
fn first_seq(journal: &Journal, kind: OpKind, needle: &str) -> u64 {
    journal
        .records
        .iter()
        .find(|r| r.kind == kind && r.succeeded() && r.path.to_string_lossy().contains(needle))
        .unwrap_or_else(|| panic!("the uncut run made no {kind:?} call to {needle}"))
        .seq
}

/// Run the workload cut at `trigger`, then cut the power under `tear`.
fn cut(tmp: &TempDir, label: &str, trigger: Trigger, tear: TearMode) -> (PathBuf, History) {
    let root = tmp.path().join(label);
    let spec = spec(&root);
    let history = spec.history();
    let out = CrashRun::new(spec)
        .trigger(trigger.clone())
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    if let Trigger::Syscall { path_contains, .. } = &trigger {
        let last = out
            .journal
            .records
            .last()
            .expect("the journal holds the fatal call");
        assert!(
            last.path.to_string_lossy().contains(path_contains.as_str()),
            "{label}: the cut landed on {:?}, not on {path_contains}",
            last.path
        );
    }
    fault::simulate_power_loss_with(
        &root,
        &out.journal,
        CutPoint::End,
        &PowerLossOptions::default().tear(tear),
    );
    (root, history)
}

fn sorted(state: HashMap<Vec<u8>, Vec<u8>>) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut state: Vec<_> = state.into_iter().collect();
    state.sort();
    state
}

/// Backup `id` restores to exactly `expected`.
fn restores_to(engine: &BackupEngine, id: u64, expected: &[(Vec<u8>, Vec<u8>)], context: &str) {
    let target = TempDir::new().unwrap();
    engine
        .restore(BackupId(id), target.path(), None)
        .unwrap_or_else(|e| panic!("{context}: backup {id} does not restore: {e}"));
    let db = Db::open(target.path(), Options::default())
        .unwrap_or_else(|e| panic!("{context}: backup {id} does not open: {e}"));
    let held = fault::recovered_state(&db).unwrap_or_else(|e| panic!("{context}: {e}"));
    assert!(held == expected, "{context}: backup {id} holds other data");
}

/// After a cut during the purge: every listed backup restores to the writes
/// before it, and the purge runs again and leaves backup 3, whole.
fn check(root: &Path, history: &History, context: &str) {
    assert!(
        marker(root, "backups").exists(),
        "{context}: the cut came before the purge"
    );
    let half = sorted(history.state_after(history.len() / 2));
    let all = sorted(history.state_after(history.len()));
    let mut engine = BackupEngine::open(backup_dir(root)).unwrap();
    let listed: Vec<u64> = engine
        .list_backups()
        .map(|b| {
            b.unwrap_or_else(|e| panic!("{context}: {e}"))
                .unwrap_or_else(|bad| panic!("{context}: {bad}"))
                .id
                .0
        })
        .collect();
    assert!(
        listed.contains(&3),
        "{context}: backup 3 is gone: {listed:?}"
    );
    for &id in &listed {
        let expected = if id == 1 { &half } else { &all };
        restores_to(&engine, id, expected, context);
    }
    engine
        .purge_old_backups(1)
        .unwrap_or_else(|e| panic!("{context}: the purge does not run again: {e}"));
    let listed: Vec<u64> = engine
        .list_backups()
        .map(|b| b.unwrap().unwrap().id.0)
        .collect();
    assert_eq!(listed, vec![3], "{context}: after the purge ran again");
    restores_to(&engine, 3, &all, &format!("{context}, purged again"));
}

const META: &str = "/backups/meta/";
const META_DIR: &str = "/backups/meta";
const META_1: &str = "/backups/meta/000001.backup";
const SHARED: &str = "/backups/shared";

#[test]
fn a_cut_while_the_purge_collects_removes_no_table_a_listed_backup_names() {
    let tmp = TempDir::new().unwrap();
    // First, so a purge that removed a listed table fails here, on what the
    // uncut run did, before the probe below counts its calls.
    let (root, history) = cut(&tmp, "returned", Trigger::Workload, TearMode::TornSector);
    check(&root, &history, "after the purge returned");

    let journal = probe();
    // The purge starts by removing backup 1's metadata, then backup 2's,
    // before one sync of `meta/`; everything counted below comes after the
    // first removal.
    let start = first_seq(&journal, OpKind::Unlink, META_1);
    let opens = calls_before(&journal, OpKind::Open, META, start);
    let syncs = calls_before(&journal, OpKind::Sync, SHARED, start);
    let meta_syncs = calls_before(&journal, OpKind::Sync, META_DIR, start);
    let removed_after = journal
        .records
        .iter()
        .filter(|r| r.kind == OpKind::Unlink && r.seq > start)
        .filter(|r| r.path.to_string_lossy().contains(SHARED))
        .count() as u64;
    assert!(removed_after > 0, "the purge removed no shared table");
    let cuts = [
        (
            "backups 1 and 2's metadata gone, before the directory sync",
            at_call(DieKind::Fsync, META_DIR, meta_syncs + 1, true),
        ),
        (
            "the metadata removals synced, before the sweep",
            at_call(DieKind::Fsync, META_DIR, meta_syncs + 1, false),
        ),
        (
            "the sweep reads backup 3's listing",
            at_call(DieKind::Open, META, opens + 1, false),
        ),
        (
            "the unlisted tables removed, before their sync",
            at_call(DieKind::Fsync, SHARED, syncs + 1, true),
        ),
        (
            "the unlisted tables removed and synced",
            at_call(DieKind::Fsync, SHARED, syncs + 1, false),
        ),
    ];
    for (i, (name, trigger)) in cuts.into_iter().enumerate() {
        for tear in TEARS {
            let label = format!("purge-{i}-{tear:?}");
            let (root, history) = cut(&tmp, &label, trigger.clone(), tear);
            check(&root, &history, &format!("{name}, {tear:?}"));
        }
    }
}
