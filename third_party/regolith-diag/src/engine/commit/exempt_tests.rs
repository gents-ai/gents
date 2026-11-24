//! A key listed in `ValidationSet::exempt` is never validated, as a write or
//! as a merge, and the keys beside it still are. A presence-only read is lost
//! only when its key is gone, and does not stand for a delete of that key.

use std::collections::BTreeMap;

use tempfile::TempDir;

use super::super::{ConflictKey, EngineOptions, ReadRule, ValidationSet};
use super::*;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::{Access, WriteBatchOp, WriteKind};

/// `name` as the default column family stores it.
fn key_of(name: &[u8]) -> Vec<u8> {
    prefix_key(DEFAULT_CF_ID, name)
}

/// Commit a transaction that began before `a` and `c` were written, listing
/// `exempt` as exempt, putting `puts` and merging into `merged`. A fresh
/// database each time, so one commit's writes never answer for another's.
fn commit_after_newer_writes(exempt: &[&[u8]], puts: &[&[u8]], merged: &[&[u8]]) -> CommitOutcome {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), EngineOptions::default()).unwrap();
    let observed = engine.snapshot_seq();
    for name in [b"a", b"c"] {
        let newer = WriteBatchOp::Put {
            key: key_of(name),
            value: b"newer".to_vec(),
        };
        engine
            .apply_batch(vec![newer], DurabilityMode::Eventual, false)
            .unwrap();
    }
    let checks = ValidationSet {
        reads: Vec::new(),
        writes_at: Some(observed),
        blind_merges_commute: false,
        exempt: exempt.iter().copied().map(key_of).collect(),
        ranges: Vec::new(),
    };
    // An exempt key names its bytes, so it is put with the bytes that landed
    // beside it; an ordinary key is put with others, so it cannot pass as an
    // identical rewrite.
    let point_ops: BTreeMap<Vec<u8>, Option<Vec<u8>>> = puts
        .iter()
        .map(|name| {
            let bytes: &[u8] = if exempt.contains(name) {
                b"newer"
            } else {
                b"mine"
            };
            (key_of(name), Some(bytes.to_vec()))
        })
        .collect();
    let merges: Vec<(Vec<u8>, Vec<u8>)> = merged
        .iter()
        .map(|name| (key_of(name), b"op".to_vec()))
        .collect();
    engine
        .commit_optimistic(
            checks,
            point_ops,
            Vec::new(),
            merges,
            Vec::new(),
            DurabilityMode::Eventual,
        )
        .unwrap()
}

#[test]
fn an_exempt_key_is_written_beside_a_newer_write_and_an_ordinary_key_is_not() {
    let outcome = commit_after_newer_writes(&[b"c"], &[b"c"], &[]);
    assert!(matches!(outcome, CommitOutcome::Ok { .. }), "{outcome:?}");

    let outcome = commit_after_newer_writes(&[], &[b"c"], &[]);
    assert!(
        matches!(outcome, CommitOutcome::Conflict { .. }),
        "unlisted, the same write conflicts: {outcome:?}"
    );

    let outcome = commit_after_newer_writes(&[b"c"], &[b"a", b"c"], &[]);
    let CommitOutcome::Conflict(conflict) = outcome else {
        panic!("the ordinary key beside the exempt one must conflict: {outcome:?}");
    };
    assert_eq!(conflict.key(), key_of(b"a"));
    assert_eq!(
        (conflict.mine(), conflict.theirs()),
        (Access::Put, WriteKind::Put)
    );
}

#[test]
fn an_exempt_key_is_merged_beside_a_newer_write_and_an_ordinary_key_is_not() {
    let outcome = commit_after_newer_writes(&[b"c"], &[], &[b"c"]);
    assert!(matches!(outcome, CommitOutcome::Ok { .. }), "{outcome:?}");

    let outcome = commit_after_newer_writes(&[], &[], &[b"c"]);
    assert!(
        matches!(outcome, CommitOutcome::Conflict { .. }),
        "unlisted, the same merge conflicts: {outcome:?}"
    );

    let outcome = commit_after_newer_writes(&[b"c"], &[], &[b"a", b"c"]);
    let CommitOutcome::Conflict(conflict) = outcome else {
        panic!("the ordinary key beside the exempt one must conflict: {outcome:?}");
    };
    assert_eq!(conflict.key(), key_of(b"a"));
    assert_eq!(
        (conflict.mine(), conflict.theirs()),
        (Access::Merge, WriteKind::Put)
    );
}

fn put_c() -> WriteBatchOp {
    WriteBatchOp::Put {
        key: key_of(b"c"),
        value: b"bytes".to_vec(),
    }
}

fn delete_c() -> WriteBatchOp {
    WriteBatchOp::Delete { key: key_of(b"c") }
}

/// A range delete over `c` and its neighbours `a` and `b`.
fn delete_range_over_c() -> WriteBatchOp {
    WriteBatchOp::DeleteRange {
        start: key_of(b"a"),
        end: key_of(b"d"),
    }
}

/// Commit a transaction that read `c` while it held a value, listed as
/// `presence_only` or not, and that also deletes `c` when `delete` is set,
/// after `history` landed. A fresh database each time.
fn commit_read_of_a_present_key(
    history: Vec<WriteBatchOp>,
    presence_only: bool,
    delete: bool,
    flush: bool,
) -> CommitOutcome {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), EngineOptions::default()).unwrap();
    engine
        .apply_batch(vec![put_c()], DurabilityMode::Eventual, false)
        .unwrap();
    let observed = engine.snapshot_seq();
    for op in history {
        engine
            .apply_batch(vec![op], DurabilityMode::Eventual, false)
            .unwrap();
    }
    if flush {
        engine.flush_active_memtable().unwrap();
    }
    let checks = ValidationSet {
        reads: vec![ConflictKey {
            key: key_of(b"c"),
            observed_seq: observed,
            found: true,
            access: if presence_only {
                Access::ReadPresence
            } else {
                Access::Read
            },
            rule: ReadRule::Seq,
        }],
        writes_at: Some(observed),
        blind_merges_commute: false,
        exempt: Vec::new(),
        ranges: Vec::new(),
    };
    let point_ops: BTreeMap<Vec<u8>, Option<Vec<u8>>> =
        delete.then(|| (key_of(b"c"), None)).into_iter().collect();
    engine
        .commit_optimistic(
            checks,
            point_ops,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            DurabilityMode::Eventual,
        )
        .unwrap()
}

#[test]
fn a_presence_only_read_is_lost_only_when_its_key_is_gone() {
    for flush in [false, true] {
        let committed = |history: Vec<WriteBatchOp>, presence_only: bool| {
            let outcome = commit_read_of_a_present_key(history, presence_only, false, flush);
            matches!(outcome, CommitOutcome::Ok { .. })
        };
        assert!(
            committed(vec![put_c()], true),
            "flush={flush}: a newer put leaves the key present"
        );
        assert!(
            !committed(vec![put_c()], false),
            "flush={flush}: a read in full conflicts with the same put"
        );
        assert!(
            !committed(vec![delete_c()], true),
            "flush={flush}: a delete takes the key away"
        );
        assert!(
            !committed(vec![delete_range_over_c()], true),
            "flush={flush}: so does a range delete over it"
        );
        assert!(
            committed(vec![delete_c(), put_c()], true),
            "flush={flush}: a put after the delete brings it back"
        );
        assert!(
            committed(vec![delete_range_over_c(), put_c()], true),
            "flush={flush}: a put newer than the range delete is not hidden by it"
        );
        assert!(
            !committed(vec![put_c(), delete_range_over_c()], true),
            "flush={flush}: a range delete newer than the put hides it"
        );
    }
}

#[test]
fn a_presence_only_read_does_not_stand_for_a_delete_of_the_same_key() {
    for flush in [false, true] {
        let outcome = commit_read_of_a_present_key(vec![put_c()], true, true, flush);
        let CommitOutcome::Conflict(conflict) = outcome else {
            panic!(
                "flush={flush}: the delete is checked on its own against the newer put: {outcome:?}"
            );
        };
        assert_eq!(conflict.key(), key_of(b"c"));
        assert_eq!(
            (conflict.mine(), conflict.theirs()),
            (Access::Delete, WriteKind::Put),
            "flush={flush}"
        );
    }
}
