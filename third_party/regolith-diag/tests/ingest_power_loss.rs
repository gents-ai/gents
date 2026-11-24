//! An ingest across a power cut (D48).
//!
//! The child opens a new database, writes the first half of a workload
//! under `Eventual` durability, ingests a table, records that the ingest
//! returned, writes the second half and dies without closing. It runs under the `LD_PRELOAD`
//! shim, which kills it at a chosen I/O call of the ingest; the directory is
//! then rebuilt as the filesystem would have left it, every byte that was
//! never synced discarded, and the database is reopened. Whatever the cut:
//!
//! - the database opens;
//! - the ingest is all or nothing, and complete once the call returned;
//! - the workload's keys are a gap-free prefix of its writes, and once the
//!   ingest survived, every write ordered before it survived too: its
//!   manifest record is durable only after the log holding them is synced;
//! - the reopened sequence counter is above the ingest's, so a write after
//!   the reopen is newer than every ingested entry.
//!
//! One workload ingests beside the memtable, so nothing is flushed for it;
//! the other carries a key the memtable holds, so the ingest flushes it
//! first and lands in L0.
//!
//! The database holds no table before the ingest, so the ingest's table,
//! and the table of the flush it may run first, is the first one: a cut
//! tears the manifest's unsynced tail while that table sits in the table
//! directory, and the open must still succeed (E29).
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
use common::keys::Keys;
use regolith::{Db, IngestOptions, Options, SstFileWriter};
use tempfile::TempDir;

const BESIDE: &str = "ingest_beside_the_memtable";
const OVER: &str = "ingest_over_the_memtable";
const INGESTED: usize = 64;
const INGESTED_VALUE: &[u8] = b"ingested";

#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

fn dispatch(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == BESIDE => workload(spec, false),
        Phase::Custom(name) if name == OVER => workload(spec, true),
        _ => fault::builtin_workload(spec),
    }
}

fn ingested_key(i: usize) -> Vec<u8> {
    format!("ing_{i:04}").into_bytes()
}

/// Beside the database directory, so the shim records none of it: the
/// source belongs to the caller, and `SstFileWriter` syncs it.
fn sidecar(db: &Path, ext: &str) -> PathBuf {
    db.with_extension(ext)
}

fn apply(db: &Db, key: &[u8], value: &OpValue) {
    match value {
        OpValue::Put(v) => db.put(key, v).expect("child: put"),
        OpValue::Delete => db.delete(key).expect("child: delete"),
    }
}

fn workload(spec: &ChildSpec, over: bool) {
    let db = Db::open(&spec.db_path, spec.options()).expect("child: open");
    let history = spec.history();
    let ops = history.ops();
    let half = ops.len() / 2;
    for op in &ops[..half] {
        apply(&db, &op.key, &op.value);
    }

    let source = sidecar(&spec.db_path, "ingest-source");
    // Under the database's own options, so an encrypted run ingests a
    // sealed table, installed as it is.
    let mut writer = SstFileWriter::create(&source, &spec.options()).expect("child: writer");
    let mut keys: Vec<Vec<u8>> = (0..INGESTED).map(ingested_key).collect();
    if over {
        // The key of the last write before the ingest, which the memtable
        // holds.
        keys.push(ops[half - 1].key.clone());
        keys.sort();
    }
    for key in &keys {
        writer.put(key, INGESTED_VALUE).expect("child: writer put");
    }
    writer.finish().expect("child: writer finish");
    db.ingest_external_files(
        &[source],
        IngestOptions {
            snapshot_consistency: false,
            ..IngestOptions::default()
        },
    )
    .wait()
    .expect("child: ingest");
    let marker = sidecar(&spec.db_path, "ingested");
    std::fs::write(&marker, b"1").expect("child: marker");
    std::fs::File::open(&marker)
        .and_then(|f| f.sync_all())
        .expect("child: sync marker");

    for op in &ops[half..] {
        apply(&db, &op.key, &op.value);
    }
    fault::kill_self();
}

fn trigger(kind: DieKind, path: &str, nth: u64, before: bool) -> Trigger {
    Trigger::Syscall {
        kind,
        path_contains: path.to_string(),
        nth,
        before,
    }
}

/// What a cut left behind.
#[derive(Debug, PartialEq, Eq)]
enum Ingest {
    Absent,
    Present,
}

fn cut(name: &str, trigger: Trigger, tear: TearMode, encrypted: bool) -> Ingest {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::Custom(name.to_string()), &db_path)
        .delete_every(0)
        .encrypted(encrypted);
    let reopen = if encrypted {
        Options::default().key_provider(Keys::new(&[1]))
    } else {
        Options::default()
    };
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
            "{name}: the cut landed on {:?}, not on {path_contains}\n{}",
            last.path,
            out.journal
        );
    }
    let report = fault::simulate_power_loss_with(
        &db_path,
        &out.journal,
        CutPoint::End,
        &PowerLossOptions::default().tear(tear),
    );
    let returned = sidecar(&db_path, "ingested").exists();
    if encrypted {
        // The run really was encrypted: without the key nothing opens.
        assert!(
            matches!(
                Db::open_read_only(&db_path, Options::default()),
                Err(regolith::Error::KeyProviderRequired)
            ),
            "{name}: an encrypted run left a database that opens without its key"
        );
    }

    let db = Db::open(&db_path, reopen).unwrap_or_else(|e| {
        panic!(
            "{name}, {trigger:?}, {tear:?}: the database refuses to open after a power cut \
             during an ingest: {e}\n{}",
            report.summary()
        )
    });
    let mut state = fault::recovered_state(&db).unwrap();
    let ingested: Vec<_> = state
        .iter()
        .filter(|(k, _)| k.starts_with(b"ing_"))
        .cloned()
        .collect();
    state.retain(|(k, _)| !k.starts_with(b"ing_"));
    let outcome = match ingested.len() {
        0 => Ingest::Absent,
        INGESTED => Ingest::Present,
        n => panic!("{name}, {trigger:?}: {n} of {INGESTED} ingested keys survived"),
    };
    assert!(
        ingested.iter().all(|(_, v)| v.as_slice() == INGESTED_VALUE),
        "{name}: an ingested key reads a value it was never given"
    );
    if returned {
        assert_eq!(
            outcome,
            Ingest::Present,
            "{name}, {trigger:?}: an ingest that returned did not survive"
        );
    }

    // The key the memtable held reads as ingested exactly when the ingest
    // survived; the prefix check then sees the write the ingest replaced.
    let ops = history.ops();
    if name == OVER {
        let overlapped = &ops[ops.len() / 2 - 1].key;
        if let Some(slot) = state.iter_mut().find(|(k, _)| k == overlapped)
            && outcome == Ingest::Present
        {
            assert_eq!(
                slot.1, INGESTED_VALUE,
                "{name}: the ingest lost to an older write"
            );
            let OpValue::Put(written) = &ops[ops.len() / 2 - 1].value else {
                unreachable!("the spec disables deletes")
            };
            slot.1 = written.clone();
        }
    }
    let prefix = fault::validate_prefix_of_state(&state, &history)
        .unwrap_or_else(|e| panic!("{name}, {trigger:?}: {e}\n{}", report.summary()));
    if outcome == Ingest::Present {
        assert!(
            prefix.k >= ops.len() / 2,
            "{name}, {trigger:?}: the ingest survived but only {} of the {} writes before it did",
            prefix.k,
            ops.len() / 2
        );
        db.put(&ingested_key(0), b"after").unwrap();
        assert_eq!(
            db.get(&ingested_key(0)).unwrap().as_deref(),
            Some(&b"after"[..]),
            "{name}: a write after the reopen is older than the ingest"
        );
    }
    outcome
}

/// The cut under both tear modes, on a plain and on an encrypted database,
/// which must all agree.
fn both_tears(name: &str, trigger: Trigger) -> Ingest {
    let truncated = cut(name, trigger.clone(), TearMode::Truncate, false);
    for encrypted in [false, true] {
        let torn = cut(name, trigger.clone(), TearMode::TornSector, encrypted);
        assert_eq!(
            truncated, torn,
            "the tear modes disagree (encrypted: {encrypted})"
        );
    }
    let sealed = cut(name, trigger, TearMode::Truncate, true);
    assert_eq!(truncated, sealed, "an encrypted database disagrees");
    truncated
}

/// The staged copy's name. File ids are handed out in order: the first log
/// takes 1 and the ingest reserves 2 before it copies, whether or not it
/// later flushes.
const STAGED: &str = "000002.sst";

/// The manifest sync that makes the ingest's record durable: after the
/// manifest's creation, and after the record of the ingest's own flush when
/// it has a memtable to flush first.
fn manifest_sync(name: &str) -> u64 {
    if name == OVER { 3 } else { 2 }
}

#[test]
fn a_cut_while_the_file_is_copied_leaves_no_ingest() {
    for name in [BESIDE, OVER] {
        assert_eq!(
            both_tears(name, trigger(DieKind::Write, STAGED, 1, false)),
            Ingest::Absent
        );
    }
}

#[test]
fn a_cut_once_the_copy_is_synced_leaves_no_ingest() {
    for name in [BESIDE, OVER] {
        assert_eq!(
            both_tears(name, trigger(DieKind::Fsync, STAGED, 1, false)),
            Ingest::Absent
        );
    }
}

#[test]
fn a_cut_before_the_manifest_record_is_synced_leaves_no_ingest() {
    for name in [BESIDE, OVER] {
        assert_eq!(
            both_tears(
                name,
                trigger(DieKind::Fsync, "MANIFEST", manifest_sync(name), true)
            ),
            Ingest::Absent
        );
    }
}

#[test]
fn a_cut_once_the_manifest_record_is_synced_keeps_the_ingest_and_every_write_before_it() {
    for name in [BESIDE, OVER] {
        assert_eq!(
            both_tears(
                name,
                trigger(DieKind::Fsync, "MANIFEST", manifest_sync(name), false)
            ),
            Ingest::Present
        );
    }
}

#[test]
fn a_cut_after_the_ingest_returned_keeps_it() {
    for name in [BESIDE, OVER] {
        assert_eq!(both_tears(name, Trigger::Workload), Ingest::Present);
    }
}
