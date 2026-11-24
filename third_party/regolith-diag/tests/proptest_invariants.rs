//! Property-based tests for regolith's core invariants.
//!
//! Every test in this file uses `proptest` to generate randomized
//! inputs and verify that the database satisfies its contract
//! across thousands of scenarios - something hand-written unit
//! tests can't cover.
//!
//! Run with:
//!
//! ```sh
//! cargo test --test proptest_invariants
//! # Or with more cases:
//! PROPTEST_CASES=1024 cargo test --test proptest_invariants
//! ```

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;

use proptest::prelude::*;
use regolith::{Error, WriteBatch};
use tempfile::TempDir;

mod common;

use common::open;

// ── helpers ────────────────────────────────────────────────────

/// Strategy that generates a key as 1-32 random bytes.
fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 1..=32)
}

/// Strategy that generates a value as 0-128 random bytes.
fn value_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=128)
}

// ── property tests ─────────────────────────────────────────────

proptest! {
    /// After N random puts, `scan(None, None)` returns every key
    /// in sorted order with no duplicates and no missing keys.
    /// This exercises the memtable, flush, and read paths end-to-end.
    #[test]
    fn scan_returns_all_keys_sorted(
        entries in prop::collection::vec(
            (key_strategy(), value_strategy()),
            1..=200,
        ),
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);

        // Build the expected state: last-write-wins per key.
        let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for (k, v) in &entries {
            regolith::through_stalls(|| db.put(k, v)).unwrap();
            expected.insert(k.clone(), v.clone());
        }

        let result = db.scan(None, None).unwrap();

        // Same number of distinct keys.
        prop_assert_eq!(result.len(), expected.len());

        // Keys are in ascending order and values match.
        for ((rk, rv), (ek, ev)) in result.iter().zip(expected.iter()) {
            prop_assert_eq!(rk, ek);
            prop_assert_eq!(rv, ev);
        }
    }

    /// A snapshot at the current seq sees exactly the writes that
    /// preceded it and none of the writes that follow it.
    #[test]
    fn snapshot_isolation(
        before in prop::collection::vec(
            (key_strategy(), value_strategy()),
            1..=50,
        ),
        after in prop::collection::vec(
            (key_strategy(), value_strategy()),
            1..=50,
        ),
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);

        // Phase 1 - write "before" entries.
        let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for (k, v) in &before {
            regolith::through_stalls(|| db.put(k, v)).unwrap();
            expected.insert(k.clone(), v.clone());
        }

        let snap = db.snapshot();

        // Phase 2 - write "after" entries (invisible to snap).
        for (k, v) in &after {
            regolith::through_stalls(|| db.put(k, v)).unwrap();
        }

        // Snapshot reads must match "before" state exactly.
        for (k, v) in &expected {
            let got = snap.get(k).unwrap();
            prop_assert_eq!(got.as_ref(), Some(v));
        }

        // A key that was only written in "after" and not in
        // "before" must be invisible through the snapshot.
        for (k, _) in &after {
            if !expected.contains_key(k) {
                let got = snap.get(k).unwrap();
                prop_assert!(got.is_none(),
                    "key {:?} should be invisible through the snapshot", k);
            }
        }
    }

    /// After random writes + a full compaction, every key that was
    /// live before compaction is still readable with the correct
    /// (latest) value.
    #[test]
    fn compaction_preserves_latest_values(
        entries in prop::collection::vec(
            (key_strategy(), value_strategy()),
            1..=300,
        ),
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);

        let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for (k, v) in &entries {
            regolith::through_stalls(|| db.put(k, v)).unwrap();
            expected.insert(k.clone(), v.clone());
        }

        db.compact_range(None, None).wait().unwrap();

        for (k, v) in &expected {
            let got = db.get(k).unwrap();
            prop_assert_eq!(got.as_ref(), Some(v),
                "key {:?} changed or disappeared after compaction", k);
        }
    }

    /// Deleting a key makes it invisible to subsequent reads, even
    /// after compaction collapses the tombstone.
    #[test]
    fn delete_then_compact_hides_key(
        entries in prop::collection::vec(
            (key_strategy(), value_strategy()),
            1..=100,
        ),
        delete_indices in prop::collection::vec(
            0..100usize,
            0..=30,
        ),
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);

        let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for (k, v) in &entries {
            regolith::through_stalls(|| db.put(k, v)).unwrap();
            expected.insert(k.clone(), v.clone());
        }

        let keys_vec: Vec<Vec<u8>> = expected.keys().cloned().collect();
        let mut deleted = std::collections::HashSet::new();
        for &idx in &delete_indices {
            if keys_vec.is_empty() { break; }
            let k = &keys_vec[idx % keys_vec.len()];
            regolith::through_stalls(|| db.delete(k)).unwrap();
            deleted.insert(k.clone());
        }
        for k in &deleted {
            expected.remove(k);
        }

        db.compact_range(None, None).wait().unwrap();

        // Deleted keys must be gone.
        for k in &deleted {
            prop_assert!(db.get(k).unwrap().is_none(),
                "deleted key {:?} is still visible after compaction", k);
        }
        // Surviving keys must be intact.
        for (k, v) in &expected {
            let got = db.get(k).unwrap();
            prop_assert_eq!(got.as_ref(), Some(v));
        }
    }

    /// A `WriteBatch` is atomic: either every write in the batch
    /// is visible, or none of them are. (Since regolith doesn't have
    /// partial-batch failure modes today, the "none" case only
    /// happens on I/O error, which proptest can't trigger. So this
    /// test verifies the "all" case under concurrent reads.)
    #[test]
    fn write_batch_atomicity(
        batches in prop::collection::vec(
            prop::collection::vec(
                (key_strategy(), value_strategy()),
                1..=20,
            ),
            1..=10,
        ),
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);

        let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for batch_entries in &batches {
            let mut batch = WriteBatch::new();
            for (k, v) in batch_entries {
                batch.put(k, v);
                expected.insert(k.clone(), v.clone());
            }
            regolith::through_stalls(|| db.write(batch.clone())).unwrap();
        }

        for (k, v) in &expected {
            let got = db.get(k).unwrap();
            prop_assert_eq!(got.as_ref(), Some(v));
        }
    }

    /// The iterator yields the same (key, value) sequence as
    /// `scan(None, None)`. This catches any divergence between the
    /// two read paths - they share the merge layer internally, but
    /// any caching/prefetching difference could desync them.
    #[test]
    fn iter_matches_scan(
        entries in prop::collection::vec(
            (key_strategy(), value_strategy()),
            1..=100,
        ),
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);
        for (k, v) in &entries {
            regolith::through_stalls(|| db.put(k, v)).unwrap();
        }
        let scanned = db.scan(None, None).unwrap();
        let mut iter_pairs = Vec::new();
        let mut it = db.iter();
        it.seek_to_first();
        while it.valid() {
            iter_pairs.push((it.key().unwrap().to_vec(), it.value().unwrap().to_vec()));
            it.next();
        }
        prop_assert_eq!(iter_pairs, scanned);
    }

    /// Reopening a clean-closed database yields exactly the same
    /// scan output. This is the "ReopenIdempotent" invariant -
    /// nothing about serialization should be lossy.
    #[test]
    fn reopen_preserves_scan_output(
        entries in prop::collection::vec(
            (key_strategy(), value_strategy()),
            1..=100,
        ),
    ) {
        let dir = TempDir::new().unwrap();
        let before = {
            let db = open(&dir);
            for (k, v) in &entries {
                regolith::through_stalls(|| db.put(k, v)).unwrap();
            }
            db.scan(None, None).unwrap()
        };
        let db = open(&dir);
        let after = db.scan(None, None).unwrap();
        prop_assert_eq!(before, after);
    }

    /// Deleted keys stay deleted across reopen - tombstones cannot
    /// resurrect values from older memtables or SSTables. This is
    /// a foundational LSM invariant; any regression here is
    /// immediately obvious to users as "my delete didn't stick".
    #[test]
    fn no_resurrection_across_reopen(
        entries in prop::collection::vec(
            (key_strategy(), value_strategy()),
            1..=80,
        ),
        delete_picks in prop::collection::vec(0..80usize, 0..=20),
    ) {
        let dir = TempDir::new().unwrap();
        let deleted_keys: Vec<Vec<u8>> = {
            let db = open(&dir);
            let mut recorded: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
            for (k, v) in &entries {
                regolith::through_stalls(|| db.put(k, v)).unwrap();
                recorded.insert(k.clone(), v.clone());
            }
            let keys: Vec<Vec<u8>> = recorded.keys().cloned().collect();
            let mut deleted = Vec::new();
            for idx in &delete_picks {
                if keys.is_empty() { break; }
                let k = &keys[idx % keys.len()];
                regolith::through_stalls(|| db.delete(k)).unwrap();
                deleted.push(k.clone());
            }
            db.compact_range(None, None).wait().unwrap();
            deleted
        };
        let db = open(&dir);
        for k in &deleted_keys {
            prop_assert!(
                db.get(k).unwrap().is_none(),
                "key {:?} resurrected across reopen",
                k
            );
        }
    }

    /// Range scans return exactly the BTreeMap slice for the given
    /// bounds. Verifies regolith's `scan(start, end)` honours the
    /// `[start, end)` half-open convention.
    #[test]
    fn range_scan_matches_btree_slice(
        entries in prop::collection::vec(
            (key_strategy(), value_strategy()),
            1..=80,
        ),
        bounds in (key_strategy(), key_strategy()),
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);
        let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for (k, v) in &entries {
            regolith::through_stalls(|| db.put(k, v)).unwrap();
            expected.insert(k.clone(), v.clone());
        }
        let (mut lo, mut hi) = bounds;
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        let got = db.scan(Some(&lo), Some(&hi)).unwrap();
        let want: Vec<(Vec<u8>, Vec<u8>)> = expected
            .range(lo.clone()..hi.clone())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        prop_assert_eq!(got, want);
    }

    /// Any interleaving of ops across two live column families, the
    /// default one, and a dropped one is rejected as a whole iff it
    /// touches the dropped one; otherwise every key lands with exactly
    /// last-write-wins semantics per (column family, key). Exercises the
    /// full range of op orders `validate_batch_cf_liveness`'s run cache
    /// must handle: the first/last/middle and repeated-run shapes hand
    /// written in `src/lib.rs` are single points in the space this walks.
    #[test]
    fn batch_column_family_liveness_is_exact_for_any_op_order(
        ops in prop::collection::vec(
            (0u8..4, key_strategy(), any::<bool>()),
            1..=48,
        ),
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);

        // Slot 0 is the default column family, 1 and 2 are live column
        // families, 3 is a column family whose handle stays around after
        // it is dropped, so it can still prefix keys into raw ops.
        let a = db.create_column_family("a").unwrap();
        let b = db.create_column_family("b").unwrap();
        let gone = db.create_column_family("gone").unwrap();
        regolith::through_stalls(|| db.put_cf(&a, b"sentinel", b"a-sentinel")).unwrap();
        regolith::through_stalls(|| db.put_cf(&b, b"sentinel", b"b-sentinel")).unwrap();
        db.drop_column_family(gone.clone()).unwrap();

        let has_dropped = ops.iter().any(|(slot, _, _)| *slot == 3);

        let mut batch = WriteBatch::new();
        // Last-write-wins per (slot, key), independent of which column
        // family the key's slot maps to.
        let mut expected: BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>> = BTreeMap::new();
        for (slot, key, is_put) in &ops {
            let value = key.clone();
            match slot {
                0 if *is_put => {
                    batch.put(key, &value);
                    expected.insert((0, key.clone()), Some(value));
                }
                0 => {
                    batch.delete(key);
                    expected.insert((0, key.clone()), None);
                }
                1 if *is_put => {
                    batch.put_cf(&a, key, &value);
                    expected.insert((1, key.clone()), Some(value));
                }
                1 => {
                    batch.delete_cf(&a, key);
                    expected.insert((1, key.clone()), None);
                }
                2 if *is_put => {
                    batch.put_cf(&b, key, &value);
                    expected.insert((2, key.clone()), Some(value));
                }
                2 => {
                    batch.delete_cf(&b, key);
                    expected.insert((2, key.clone()), None);
                }
                _ if *is_put => batch.put_cf(&gone, key, &value),
                _ => batch.delete_cf(&gone, key),
            }
        }

        let result = regolith::through_stalls(|| db.write(batch.clone()));

        if has_dropped {
            prop_assert!(matches!(result, Err(Error::InvalidColumnFamily(_))));
            for (slot, key) in expected.keys() {
                let got = match slot {
                    0 => db.get(key).unwrap(),
                    1 => db.get_cf(&a, key).unwrap(),
                    2 => db.get_cf(&b, key).unwrap(),
                    _ => unreachable!(),
                };
                prop_assert_eq!(
                    got, None,
                    "key {:?} in slot {} must be absent after a rejected batch",
                    key, slot
                );
            }
            prop_assert_eq!(db.get_cf(&a, b"sentinel").unwrap(), Some(b"a-sentinel".to_vec()));
            prop_assert_eq!(db.get_cf(&b, b"sentinel").unwrap(), Some(b"b-sentinel".to_vec()));
        } else {
            prop_assert!(result.is_ok());
            for ((slot, key), want) in &expected {
                let got = match slot {
                    0 => db.get(key).unwrap(),
                    1 => db.get_cf(&a, key).unwrap(),
                    2 => db.get_cf(&b, key).unwrap(),
                    _ => unreachable!(),
                };
                prop_assert_eq!(
                    &got, want,
                    "key {:?} in slot {} did not match last-write-wins",
                    key, slot
                );
            }
        }
    }
}
