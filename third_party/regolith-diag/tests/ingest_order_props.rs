//! The invariants the ingest models check, checked against the engine.
//!
//! A random sequence of puts, deletes, range deletes, flushes, compactions,
//! ingests, snapshots and reopens over a handful of keys is applied to
//! regolith and to a reference model, a map per point in time. After every
//! step:
//!
//! - every key read at the head is the newest version the model holds
//!   (LsmOrder.tla `ReadNewest`: wherever an ingested table was placed, in
//!   L0 or deeper, and whichever memtables it flushed or left alone);
//! - every live snapshot reads every key as it did when taken
//!   (IngestPublication.tla `RepeatableSnapshot`);
//! - a full scan agrees with the model.
//!
//! The few keys make every ingest overlap something somewhere: a memtable, an
//! L0 table, a deeper level, or an earlier ingest.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::path::Path;

use proptest::prelude::*;
use regolith::{Db, IngestOptions, Options, Snapshot, SstFileWriter};
use tempfile::TempDir;

const KEYS: u8 = 8;

#[derive(Clone, Debug)]
enum Op {
    Put(u8),
    Delete(u8),
    DeleteRange(u8, u8),
    Flush,
    CompactRange(u8, u8),
    CompactAll,
    /// An ingested file: each key it holds, written or deleted.
    Ingest(BTreeMap<u8, bool>),
    Snapshot,
    Release,
    Reopen,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..KEYS).prop_map(Op::Put),
        1 => (0..KEYS).prop_map(Op::Delete),
        1 => (0..KEYS, 0..KEYS).prop_map(|(a, b)| Op::DeleteRange(a.min(b), a.max(b) + 1)),
        2 => Just(Op::Flush),
        1 => (0..KEYS, 0..KEYS).prop_map(|(a, b)| Op::CompactRange(a.min(b), a.max(b) + 1)),
        1 => Just(Op::CompactAll),
        3 => proptest::collection::btree_map(0..KEYS, any::<bool>(), 1..5).prop_map(Op::Ingest),
        2 => Just(Op::Snapshot),
        1 => Just(Op::Release),
        1 => Just(Op::Reopen),
    ]
}

fn key(k: u8) -> Vec<u8> {
    format!("key{k}").into_bytes()
}

fn options() -> Options {
    Options::default().max_background_compactions(0)
}

/// The head of the model, and a copy per live snapshot.
type State = BTreeMap<u8, Vec<u8>>;

fn ingest_file(
    dir: &Path,
    n: usize,
    entries: &BTreeMap<u8, bool>,
    value: &[u8],
) -> std::path::PathBuf {
    let path = dir.join(format!("ingest-{n}.sst"));
    let mut writer = SstFileWriter::create(&path, &options()).unwrap();
    for (&k, &put) in entries {
        if put {
            writer.put(&key(k), value).unwrap();
        } else {
            writer.delete(&key(k)).unwrap();
        }
    }
    writer.finish().unwrap();
    path
}

fn check(db: &Db, head: &State, snapshots: &[(Snapshot, State)], step: usize) {
    for k in 0..KEYS {
        assert_eq!(
            db.get(&key(k)).unwrap(),
            head.get(&k).cloned(),
            "step {step}: the head read of key{k} is not the newest version"
        );
        for (i, (snapshot, state)) in snapshots.iter().enumerate() {
            assert_eq!(
                snapshot.get(&key(k)).unwrap(),
                state.get(&k).cloned(),
                "step {step}: snapshot {i} read key{k} differently than when it was taken"
            );
        }
    }
    let scanned: Vec<(Vec<u8>, Vec<u8>)> = db.scan(None, None).unwrap();
    let expected: Vec<(Vec<u8>, Vec<u8>)> =
        head.iter().map(|(&k, v)| (key(k), v.clone())).collect();
    assert_eq!(
        scanned, expected,
        "step {step}: a scan disagrees with the model"
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn every_read_is_the_newest_version_and_every_snapshot_repeats(
        ops in proptest::collection::vec(op(), 1..40)
    ) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("db");
        let mut db = Db::open(&db_path, options()).unwrap();
        let mut head = State::new();
        let mut snapshots: Vec<(Snapshot, State)> = Vec::new();

        for (step, op) in ops.iter().enumerate() {
            let value = format!("v{step}").into_bytes();
            match op {
                Op::Put(k) => {
                    db.put(&key(*k), &value).unwrap();
                    head.insert(*k, value);
                }
                Op::Delete(k) => {
                    db.delete(&key(*k)).unwrap();
                    head.remove(k);
                }
                Op::DeleteRange(a, b) => {
                    db.delete_range(&key(*a), &key(*b)).unwrap();
                    head.retain(|k, _| !(*a..*b).contains(k));
                }
                Op::Flush => db.flush().unwrap(),
                Op::CompactRange(a, b) => db.compact_range(Some(&key(*a)), Some(&key(*b))).wait().unwrap(),
                Op::CompactAll => db.compact_range(None, None).wait().unwrap(),
                Op::Ingest(entries) => {
                    let path = ingest_file(dir.path(), step, entries, &value);
                    db.ingest_external_files(
                        &[path],
                        IngestOptions { snapshot_consistency: false, ..IngestOptions::default() },
                    )
                    .wait().unwrap();
                    for (&k, &put) in entries {
                        if put {
                            head.insert(k, value.clone());
                        } else {
                            head.remove(&k);
                        }
                    }
                }
                Op::Snapshot => {
                    if snapshots.len() < 3 {
                        snapshots.push((db.snapshot(), head.clone()));
                    }
                }
                Op::Release => {
                    snapshots.pop();
                }
                Op::Reopen => {
                    snapshots.clear();
                    db.close().unwrap();
                    drop(db);
                    db = Db::open(&db_path, options()).unwrap();
                }
            }
            check(&db, &head, &snapshots, step);
        }
    }
}
