//! Power cuts during a backup, a restore and a purge of a database
//! encrypted at rest (D57).
//!
//! The child opens an encrypted database, writes half its workload, takes
//! backup 1, writes the rest, takes backup 2 and, in the restore phase,
//! restores backup 2 under the database's key; in the purge phase it
//! compacts before backup 2, so backup 1 holds tables backup 2 does not,
//! and purges every backup but backup 2. It runs under the
//! `LD_PRELOAD` shim, which kills it at a chosen call; the directory is then
//! rebuilt as the filesystem would have left it, every byte never synced
//! discarded. Whatever the cut:
//!
//! - a backup is listed exactly when its metadata reached the disk whole,
//!   which is after every table it lists did, and the listing needs no key;
//! - every listed backup restores, under the right key only, to exactly
//!   the writes before it, and leaves no key range in plaintext;
//! - a target the restore did not finish has no MANIFEST, so it is not a
//!   database; the restore runs again over what the cut left and finishes;
//! - a target the restore finished opens under the right key only;
//! - the repository still takes and restores a new backup of the reopened
//!   source;
//! - a purge cut short leaves backup 2 whole, and the next purge leaves the
//!   pool holding exactly the tables the backup it keeps lists.
//!
//! The cut points are counted in calls read off one uncut run of the same
//! workload, so a change in how many tables a backup copies moves them
//! with it. The rules are `proofs/tla/BackupSeal.tla`'s `ListedRestores`
//! and `FaithfulRestore`.
//!
//! # Linux only
//!
//! `LD_PRELOAD` interposition is a glibc mechanism; the file is compiled out
//! elsewhere. See `power_loss.rs` for what the shim does and does not prove.
#![cfg(target_os = "linux")]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::fault::{
    self, ChildSpec, CrashRun, CutPoint, DieKind, History, Journal, OpKind, OpValue, Phase,
    PowerLossOptions, TearMode, Trigger, WriteOp,
};
use common::keys::Keys;
use regolith::{BackupEngine, BackupId, Db, Error, KeyProvider, Options};
use tempfile::TempDir;

const BACKUP: &str = "encrypted_backup_cut";
const RESTORE: &str = "encrypted_restore_cut";
const PURGE: &str = "encrypted_purge_cut";
const TEARS: [TearMode; 2] = [TearMode::Truncate, TearMode::TornSector];

#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

fn dispatch(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == BACKUP || name == RESTORE || name == PURGE => {
            workload(spec, name)
        }
        _ => fault::builtin_workload(spec),
    }
}

fn db_dir(root: &Path) -> PathBuf {
    root.join("db")
}

fn backup_dir(root: &Path) -> PathBuf {
    root.join("backups")
}

fn target_dir(root: &Path) -> PathBuf {
    root.join("restored")
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

fn keys() -> Option<Arc<dyn KeyProvider>> {
    Some(Keys::new(&[1]))
}

/// Encrypted under key 1, small tables, and no background compaction, so
/// every run issues the same calls in the same order and a cut counted in
/// calls lands in the same place.
fn db_options(spec: &ChildSpec) -> Options {
    Options::default()
        .write_buffer_size(spec.write_buffer_size)
        .durability(spec.durability)
        .max_background_compactions(0)
        .key_provider(Keys::new(&[1]))
}

fn apply(db: &Db, op: &WriteOp) {
    match &op.value {
        OpValue::Put(v) => regolith::through_stalls(|| db.put(&op.key, v)).expect("child: put"),
        OpValue::Delete => regolith::through_stalls(|| db.delete(&op.key)).expect("child: delete"),
    }
}

/// The two backups, then what phase `then` does after them.
fn workload(spec: &ChildSpec, then: &str) {
    let root = &spec.db_path;
    let db = Db::open(db_dir(root), db_options(spec)).expect("child: open");
    let history = spec.history();
    let ops = history.ops();
    let half = ops.len() / 2;
    ops[..half].iter().for_each(|op| apply(&db, op));
    let mut engine = BackupEngine::open(backup_dir(root)).expect("child: backup engine");
    engine.create_backup(&db).expect("child: first backup");
    mark(root, "backup1");
    ops[half..].iter().for_each(|op| apply(&db, op));
    if then == PURGE {
        // Every table rewritten, so the purge has tables of backup 1 to
        // remove.
        db.compact_range(None, None).wait().expect("child: compact");
    }
    engine.create_backup(&db).expect("child: second backup");
    mark(root, "backup2");
    if then == RESTORE {
        engine
            .restore(BackupId(2), target_dir(root), keys())
            .expect("child: restore");
        mark(root, "restored");
    }
    if then == PURGE {
        mark(root, "purging");
        engine.purge_old_backups(1).expect("child: purge");
    }
    fault::kill_self();
}

fn spec(name: &str, root: &Path) -> ChildSpec {
    ChildSpec::new(Phase::Custom(name.to_string()), root)
        .ops(240)
        .value_len(64)
        .write_buffer_size(4 * 1024)
}

/// The calls one run of `name` issues when nothing cuts it short but the
/// workload's own end.
fn probe(name: &str) -> Journal {
    let tmp = TempDir::new().unwrap();
    let out = CrashRun::new(spec(name, &tmp.path().join("root")))
        .trigger(Trigger::Workload)
        .timeout(Duration::from_secs(180))
        .run();
    out.assert_killed();
    assert!(
        marker(&out.spec.db_path, "backup2").exists(),
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

/// The first call of `kind` to `needle` in the probe, by sequence number.
fn first_seq(journal: &Journal, kind: OpKind, needle: &str) -> u64 {
    journal
        .records
        .iter()
        .find(|r| r.kind == kind && r.succeeded() && r.path.to_string_lossy().contains(needle))
        .unwrap_or_else(|| panic!("the uncut run made no {kind:?} call to {needle}"))
        .seq
}

/// Run `name` cut at `trigger`, then cut the power under `tear`. Returns
/// the root the child wrote under.
fn cut(
    tmp: &TempDir,
    label: &str,
    name: &str,
    trigger: Trigger,
    tear: TearMode,
) -> (PathBuf, History) {
    let root = tmp.path().join(label);
    let spec = spec(name, &root);
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

fn sorted(state: std::collections::HashMap<Vec<u8>, Vec<u8>>) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut state: Vec<_> = state.into_iter().collect();
    state.sort();
    state
}

fn keyed() -> Options {
    Options::default().key_provider(Keys::new(&[1]))
}

/// Every file under `dir` that holds a workload key in plaintext.
fn plaintext_keys(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if std::fs::read(&path)
                .unwrap()
                .windows(4)
                .any(|w| w == b"key_")
            {
                out.push(path);
            }
        }
    }
    out
}

/// The database at `dir` opens under key 1 only, and holds `expected`.
fn opens_under_its_key_holding(dir: &Path, expected: &[(Vec<u8>, Vec<u8>)], context: &str) {
    assert!(
        matches!(
            Db::open(dir, Options::default()),
            Err(Error::KeyProviderRequired)
        ),
        "{context}: opened without a key"
    );
    assert!(
        Db::open(dir, Options::default().key_provider(Keys::wrong(&[1]))).is_err(),
        "{context}: opened under the wrong key"
    );
    let db = Db::open(dir, keyed()).unwrap_or_else(|e| panic!("{context}: {e}"));
    let held = fault::recovered_state(&db).unwrap_or_else(|e| panic!("{context}: {e}"));
    assert!(held == expected, "{context}: the restore holds other data");
    drop(db);
    assert!(
        plaintext_keys(dir).is_empty(),
        "{context}: a key in plaintext: {:?}",
        plaintext_keys(dir)
    );
}

/// After a cut during a backup or a purge: exactly `listed` backups are
/// listed with no key, each restores under the right key only to the writes
/// before it, and the repository takes and restores a new backup of the
/// reopened source, whose id this returns.
fn check_backups(root: &Path, history: &History, listed: &[u64], context: &str) -> BackupId {
    let engine = BackupEngine::open(backup_dir(root)).unwrap();
    let found: Vec<u64> = engine
        .list_backups()
        .map(|entry| {
            entry
                .unwrap()
                .unwrap_or_else(|bad| panic!("{context}: a cut left a backup unreadable: {bad}"))
                .id
                .0
        })
        .collect();
    assert_eq!(found, listed, "{context}: the backups listed");
    // A backup that returned stays, unless a purge was asked to delete it.
    let purging = marker(root, "purging").exists();
    for (id, what) in [(1, "backup1"), (2, "backup2")] {
        if marker(root, what).exists() && !(purging && id == 1) {
            assert!(
                found.contains(&id),
                "{context}: backup {id} returned and is lost"
            );
        }
    }
    let half = history.len() / 2;
    let checks = TempDir::new().unwrap();
    for &id in &found {
        let upto = if id == 1 { half } else { history.len() };
        let expected = sorted(history.state_after(upto));
        let target = checks.path().join(format!("backup-{id}"));
        let refused = engine.restore(BackupId(id), &target, None);
        assert!(
            matches!(refused, Err(Error::KeyProviderRequired)) && !target.exists(),
            "{context}: backup {id} without a key: {refused:?}"
        );
        let wrong: Option<Arc<dyn KeyProvider>> = Some(Keys::wrong(&[1]));
        let refused = engine.restore(BackupId(id), &target, wrong);
        assert!(
            matches!(refused, Err(Error::Corruption(_))) && !target.exists(),
            "{context}: backup {id} under the wrong key: {refused:?}"
        );
        engine
            .restore(BackupId(id), &target, keys())
            .unwrap_or_else(|e| panic!("{context}: backup {id} does not restore: {e}"));
        opens_under_its_key_holding(&target, &expected, &format!("{context}, backup {id}"));
    }
    assert!(
        plaintext_keys(&backup_dir(root)).is_empty(),
        "{context}: a key range in plaintext in the backups"
    );

    let mut engine = engine;
    let source = Db::open(db_dir(root), keyed())
        .unwrap_or_else(|e| panic!("{context}: the source does not reopen: {e}"));
    let held = fault::recovered_state(&source).unwrap();
    let id = engine
        .create_backup(&source)
        .unwrap_or_else(|e| panic!("{context}: no new backup after the cut: {e}"));
    drop(source);
    let target = checks.path().join("after");
    engine.restore(id, &target, keys()).unwrap();
    opens_under_its_key_holding(&target, &held, &format!("{context}, new backup"));
    id
}

/// After a cut during the purge of backup 1: backup 2 alone is listed and
/// restores, and a purge run again leaves the pool holding exactly the
/// tables of the one backup it keeps, whatever the cut left behind.
fn check_purge(root: &Path, history: &History, context: &str) {
    let newest = check_backups(root, history, &[2], context);
    let mut engine = BackupEngine::open(backup_dir(root)).unwrap();
    engine
        .purge_old_backups(1)
        .unwrap_or_else(|e| panic!("{context}: the purge does not run again: {e}"));
    let kept: Vec<_> = engine
        .list_backups()
        .map(|entry| {
            entry
                .unwrap()
                .unwrap_or_else(|bad| panic!("{context}: {bad}"))
        })
        .collect();
    assert_eq!(
        kept.iter().map(|info| info.id).collect::<Vec<_>>(),
        [newest],
        "{context}: the backups a purge keeps"
    );
    let pool = std::fs::read_dir(backup_dir(root).join("shared"))
        .unwrap()
        .count();
    assert_eq!(
        pool, kept[0].file_count,
        "{context}: the pool holds a table no backup lists"
    );
}

/// After a cut during the restore: the target has a MANIFEST exactly when
/// `finished`; an unfinished one restores again over what the cut left;
/// either way it opens under the right key only, holding every write.
fn check_restore(root: &Path, history: &History, finished: bool, context: &str) {
    let target = target_dir(root);
    assert_eq!(
        target.join("MANIFEST").exists(),
        finished,
        "{context}: whether the target holds a MANIFEST"
    );
    if marker(root, "restored").exists() {
        assert!(finished, "{context}: a returned restore left no MANIFEST");
    }
    if !finished {
        BackupEngine::open(backup_dir(root))
            .unwrap()
            .restore(BackupId(2), &target, keys())
            .unwrap_or_else(|e| panic!("{context}: the restore does not run again: {e}"));
    }
    let expected = sorted(history.state_after(history.len()));
    opens_under_its_key_holding(&target, &expected, context);
}

const SHARED: &str = "/backups/shared/";
const SHARED_DIR: &str = "/backups/shared";
const META_DIR: &str = "/backups/meta";
const META_1: &str = "/backups/meta/000001";
const META_2: &str = "/backups/meta/000002";
const TARGET_TABLES: &str = "/restored/sst/";
const TARGET_MANIFEST: &str = "/restored/MANIFEST";
const NONE: &[u64] = &[];
const FIRST: &[u64] = &[1];

#[test]
fn a_cut_during_a_backup_leaves_only_whole_backups_that_restore_under_the_key() {
    let tmp = TempDir::new().unwrap();
    // First, so a backup that never synced what it lists fails here, on
    // what the cut did, before the probe below counts its syncs.
    let (root, history) = cut(
        &tmp,
        "returned",
        BACKUP,
        Trigger::Workload,
        TearMode::TornSector,
    );
    check_backups(&root, &history, &[1, 2], "after backup 2 returned");

    let journal = probe(BACKUP);
    let first_meta = first_seq(&journal, OpKind::Write, META_1);
    let tables = calls_before(&journal, OpKind::Sync, SHARED, first_meta);
    let writes = calls_before(&journal, OpKind::Write, SHARED, first_meta);
    assert!(tables > 0, "backup 1 copied no table");
    assert!(
        journal.writes_to(SHARED).len() as u64 > writes,
        "backup 2 copied no new table, so no cut lands inside it"
    );
    let cuts = [
        (
            "first table, unsynced",
            at_call(DieKind::Write, SHARED, 1, false),
            NONE,
        ),
        (
            "first table, before its sync",
            at_call(DieKind::Fsync, SHARED, 1, true),
            NONE,
        ),
        (
            "last table of backup 1, synced",
            at_call(DieKind::Fsync, SHARED, tables, false),
            NONE,
        ),
        (
            "backup 1 metadata, unsynced",
            at_call(DieKind::Write, META_1, 1, false),
            NONE,
        ),
        (
            "backup 1 metadata, before its sync",
            at_call(DieKind::Fsync, META_1, 1, true),
            NONE,
        ),
        (
            "backup 1 metadata, synced",
            at_call(DieKind::Fsync, META_1, 1, false),
            NONE,
        ),
        (
            "backup 2's first new table",
            at_call(DieKind::Write, SHARED, writes + 1, false),
            FIRST,
        ),
        (
            "backup 2 metadata, before its sync",
            at_call(DieKind::Fsync, META_2, 1, true),
            FIRST,
        ),
    ];
    for (i, (name, trigger, listed)) in cuts.into_iter().enumerate() {
        for tear in TEARS {
            let label = format!("backup-{i}-{tear:?}");
            let (root, history) = cut(&tmp, &label, BACKUP, trigger.clone(), tear);
            check_backups(&root, &history, listed, &format!("{name}, {tear:?}"));
        }
    }
}

#[test]
fn a_cut_during_a_restore_leaves_a_target_that_finishes_and_opens_under_the_key_only() {
    let tmp = TempDir::new().unwrap();
    // First, for the reason the backup test gives.
    let (root, history) = cut(
        &tmp,
        "returned",
        RESTORE,
        Trigger::Workload,
        TearMode::TornSector,
    );
    check_restore(&root, &history, true, "after the restore returned");

    let journal = probe(RESTORE);
    let tables = journal.syncs_to(TARGET_TABLES).len() as u64;
    assert!(tables > 0, "the restore copied no table");
    let cuts = [
        (
            "first table, unsynced",
            at_call(DieKind::Write, TARGET_TABLES, 1, false),
        ),
        (
            "first table, before its sync",
            at_call(DieKind::Fsync, TARGET_TABLES, 1, true),
        ),
        (
            "last table, synced",
            at_call(DieKind::Fsync, TARGET_TABLES, tables, false),
        ),
        (
            "MANIFEST, unsynced",
            at_call(DieKind::Write, TARGET_MANIFEST, 1, false),
        ),
        (
            "MANIFEST, before its sync",
            at_call(DieKind::Fsync, TARGET_MANIFEST, 1, true),
        ),
        (
            "MANIFEST synced, not renamed",
            at_call(DieKind::Fsync, TARGET_MANIFEST, 1, false),
        ),
    ];
    for (i, (name, trigger)) in cuts.into_iter().enumerate() {
        for tear in TEARS {
            let label = format!("restore-{i}-{tear:?}");
            let (root, history) = cut(&tmp, &label, RESTORE, trigger.clone(), tear);
            check_restore(&root, &history, false, &format!("{name}, {tear:?}"));
        }
    }
}

#[test]
fn a_cut_during_a_purge_keeps_every_listed_backup_whole_and_the_next_purge_collects() {
    let tmp = TempDir::new().unwrap();
    // First, for the reason the backup test gives.
    let (root, history) = cut(
        &tmp,
        "returned",
        PURGE,
        Trigger::Workload,
        TearMode::TornSector,
    );
    check_purge(&root, &history, "after the purge returned");

    let journal = probe(PURGE);
    let unlinked = |needle: &str| {
        journal
            .records
            .iter()
            .find(|r| {
                r.kind == OpKind::Unlink
                    && r.succeeded()
                    && r.path.to_string_lossy().contains(needle)
            })
            .unwrap_or_else(|| panic!("the uncut purge removed nothing under {needle}"))
            .seq
    };
    // The sync of `meta/` after backup 1's metadata is removed, and the
    // sync of `shared/` after its tables are: each the first sync of its
    // directory after that removal.
    let meta_sync = calls_before(&journal, OpKind::Sync, META_DIR, unlinked(META_1)) + 1;
    let shared_sync = calls_before(&journal, OpKind::Sync, SHARED_DIR, unlinked(SHARED)) + 1;
    // The power-loss model keeps every unlink (common/fault/power.rs), so
    // backup 1 is gone after each cut; a disk that brought its metadata
    // back would bring back the state before the purge, every table kept.
    let cuts = [
        (
            "backup 1 metadata removed, meta/ before its sync",
            at_call(DieKind::Fsync, META_DIR, meta_sync, true),
        ),
        (
            "backup 1 tables removed, shared/ before its sync",
            at_call(DieKind::Fsync, SHARED_DIR, shared_sync, true),
        ),
        (
            "backup 1 tables removed, shared/ synced",
            at_call(DieKind::Fsync, SHARED_DIR, shared_sync, false),
        ),
    ];
    for (i, (name, trigger)) in cuts.into_iter().enumerate() {
        for tear in TEARS {
            let label = format!("purge-{i}-{tear:?}");
            let (root, history) = cut(&tmp, &label, PURGE, trigger.clone(), tear);
            check_purge(&root, &history, &format!("{name}, {tear:?}"));
        }
    }
}
