//! The keys a commit's batch replaces outright, for the conflict check of a
//! blind merge. It borrows the batch in the layout `grouped_batch_ops` builds,
//! so building it allocates nothing and a lookup searches instead of hashing.

use crate::WriteBatchOp;

/// The keys a batch replaces outright: put, deleted, or covered by a range
/// delete. A merged key outside it is one the batch only merges into.
///
/// Borrows the batch in the layout `grouped_batch_ops` builds, so a lookup
/// is a binary search over the sorted point operations plus a scan of the
/// range deletes, and building it allocates nothing.
pub(super) struct Replaced<'batch> {
    points: &'batch [WriteBatchOp],
    ranges: &'batch [WriteBatchOp],
}

impl<'batch> Replaced<'batch> {
    /// Split `ops` into its point operations and its range deletes. `ops`
    /// must be laid out as `grouped_batch_ops` lays it out; debug builds
    /// check that.
    pub(super) fn of(ops: &'batch [WriteBatchOp]) -> Self {
        let (points, rest) = ops.split_at(ops.partition_point(is_point));
        let ranges =
            &rest[..rest.partition_point(|op| matches!(op, WriteBatchOp::DeleteRange { .. }))];
        debug_assert!(
            rest.iter().all(|op| !is_point(op)),
            "point operations lead the batch"
        );
        debug_assert!(
            points
                .windows(2)
                .all(|w| point_key(&w[0]) < point_key(&w[1])),
            "point operations are sorted by key"
        );
        Self { points, ranges }
    }

    /// Whether the batch puts, deletes or range-deletes `key`.
    pub(super) fn contains(&self, key: &[u8]) -> bool {
        self.points
            .binary_search_by(|op| point_key(op).cmp(key))
            .is_ok()
            || self.ranges.iter().any(|op| {
                matches!(
                    op,
                    WriteBatchOp::DeleteRange { start, end }
                        if start.as_slice() <= key && key < end.as_slice()
                )
            })
    }
}

/// Whether `op` is a put or a delete of one key.
fn is_point(op: &WriteBatchOp) -> bool {
    matches!(op, WriteBatchOp::Put { .. } | WriteBatchOp::Delete { .. })
}

/// The key of a point operation; empty for any other, which `Replaced::of`
/// keeps out of the point run.
fn point_key(op: &WriteBatchOp) -> &[u8] {
    match op {
        WriteBatchOp::Put { key, .. } | WriteBatchOp::Delete { key } => key,
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column_family::{DEFAULT_CF_ID, prefix_key};
    use crate::engine::{
        CommitOutcome, DurabilityMode, EngineOptions, RegolithEngine, ValidationSet,
        grouped_batch_ops,
    };
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    /// A batch in the layout a commit builds: `points` as `(key, is_put)`,
    /// then range deletes, then a merge into each of `merged`.
    fn batch(
        points: &[(&[u8], bool)],
        ranges: &[(&[u8], &[u8])],
        merged: &[&[u8]],
    ) -> Vec<WriteBatchOp> {
        grouped_batch_ops(
            points
                .iter()
                .map(|&(key, is_put)| (key.to_vec(), is_put.then(|| b"v".to_vec())))
                .collect(),
            ranges
                .iter()
                .map(|&(start, end)| (start.to_vec(), end.to_vec()))
                .collect(),
            merged
                .iter()
                .map(|&key| (key.to_vec(), b"op".to_vec()))
                .collect(),
        )
    }

    #[test]
    fn a_put_key_is_replaced() {
        let ops = batch(&[(b"b", true)], &[], &[]);
        let replaced = Replaced::of(&ops);
        assert!(replaced.contains(b"b"));
        assert!(!replaced.contains(b"a"));
        assert!(!replaced.contains(b"bb"));
        assert!(!replaced.contains(b"c"));
    }

    #[test]
    fn a_deleted_key_is_replaced() {
        let ops = batch(&[(b"b", false)], &[], &[]);
        let replaced = Replaced::of(&ops);
        assert!(replaced.contains(b"b"));
        assert!(!replaced.contains(b"a"));
        assert!(!replaced.contains(b"c"));
    }

    #[test]
    fn a_key_the_batch_only_merges_into_is_not_replaced() {
        let ops = batch(&[(b"a", true), (b"z", false)], &[(b"p", b"q")], &[b"m"]);
        let replaced = Replaced::of(&ops);
        assert!(!replaced.contains(b"m"));
        assert!(replaced.contains(b"a"));
        assert!(replaced.contains(b"z"));
        assert!(replaced.contains(b"p"));
    }

    #[test]
    fn a_range_delete_covers_its_start_but_not_its_end() {
        let ops = batch(&[], &[(b"c", b"f")], &[]);
        let replaced = Replaced::of(&ops);
        assert!(!replaced.contains(b"b"));
        assert!(replaced.contains(b"c"));
        assert!(replaced.contains(b"d"));
        assert!(replaced.contains(b"ez"));
        assert!(!replaced.contains(b"f"));
        assert!(!replaced.contains(b"g"));
    }

    #[test]
    fn keys_before_the_first_and_after_the_last_point_operation_are_not_replaced() {
        let ops = batch(&[(b"d", true), (b"f", false), (b"h", true)], &[], &[b"x"]);
        let replaced = Replaced::of(&ops);
        assert!(!replaced.contains(b""));
        assert!(!replaced.contains(b"a"));
        assert!(!replaced.contains(b"e"));
        assert!(!replaced.contains(b"i"));
        assert!(replaced.contains(b"d"));
        assert!(replaced.contains(b"f"));
        assert!(replaced.contains(b"h"));
    }

    #[test]
    fn an_empty_batch_replaces_nothing() {
        let ops = batch(&[], &[], &[]);
        let replaced = Replaced::of(&ops);
        assert!(!replaced.contains(b""));
        assert!(!replaced.contains(b"a"));
    }

    /// `ops` as `(kind, key, bytes)`, the bytes being a put's value, a
    /// range's end or a merge's operand, so a layout compares exactly.
    fn shape(ops: &[WriteBatchOp]) -> Vec<(&'static str, &[u8], &[u8])> {
        ops.iter()
            .map(|op| match op {
                WriteBatchOp::Put { key, value } => ("put", key.as_slice(), value.as_slice()),
                WriteBatchOp::Delete { key } => ("delete", key.as_slice(), &[][..]),
                WriteBatchOp::DeleteRange { start, end } => {
                    ("range", start.as_slice(), end.as_slice())
                }
                WriteBatchOp::Merge { key, operand } => {
                    ("merge", key.as_slice(), operand.as_slice())
                }
            })
            .collect()
    }

    #[test]
    fn a_batch_lays_out_points_by_key_then_ranges_then_merges_by_key_in_buffered_order() {
        let point_ops =
            BTreeMap::from([(b"q".to_vec(), Some(b"v".to_vec())), (b"d".to_vec(), None)]);
        let range_deletes = vec![
            (b"x".to_vec(), b"y".to_vec()),
            (b"m".to_vec(), b"n".to_vec()),
        ];
        // Keys a, b, a, c, b: each key's operands interleaved with the others'.
        let merges = [
            (b"a", b"1"),
            (b"b", b"2"),
            (b"a", b"3"),
            (b"c", b"4"),
            (b"b", b"5"),
        ]
        .map(|(key, operand)| (key.to_vec(), operand.to_vec()))
        .to_vec();

        let ops = grouped_batch_ops(point_ops, range_deletes, merges);

        let expected: [(&str, &[u8], &[u8]); 9] = [
            ("delete", b"d", b""),
            ("put", b"q", b"v"),
            ("range", b"x", b"y"),
            ("range", b"m", b"n"),
            ("merge", b"a", b"1"),
            ("merge", b"a", b"3"),
            ("merge", b"b", b"2"),
            ("merge", b"b", b"5"),
            ("merge", b"c", b"4"),
        ];
        assert_eq!(shape(&ops), expected);
    }

    /// `name` as the default column family stores it.
    fn key_of(name: &[u8]) -> Vec<u8> {
        prefix_key(DEFAULT_CF_ID, name)
    }

    /// Commit a blind-merge batch that merges into `k` and range-deletes
    /// `ranges`, after another operand for `k` landed since its snapshot.
    fn commit_beside_a_newer_operand(ranges: Vec<(Vec<u8>, Vec<u8>)>) -> CommitOutcome {
        let dir = TempDir::new().unwrap();
        let engine = RegolithEngine::open(dir.path(), EngineOptions::default()).unwrap();
        let write = |op| {
            engine
                .apply_batch(vec![op], DurabilityMode::Eventual, false)
                .unwrap()
        };
        write(WriteBatchOp::Put {
            key: key_of(b"k"),
            value: b"v".to_vec(),
        });
        let observed = engine.snapshot_seq();
        write(WriteBatchOp::Merge {
            key: key_of(b"k"),
            operand: b"op".to_vec(),
        });
        let checks = ValidationSet {
            reads: Vec::new(),
            writes_at: Some(observed),
            blind_merges_commute: true,
            exempt: Vec::new(),
            ranges: Vec::new(),
        };
        engine
            .commit_optimistic(
                checks,
                BTreeMap::new(),
                ranges,
                vec![(key_of(b"k"), b"op".to_vec())],
                Vec::new(),
                DurabilityMode::Eventual,
            )
            .unwrap()
    }

    /// A transaction buffers no range delete today, so only a commit made
    /// straight on the engine carries one beside a merge.
    #[test]
    fn a_merge_beside_a_newer_operand_commits_unless_the_batch_range_deletes_its_key() {
        let outcome = commit_beside_a_newer_operand(Vec::new());
        assert!(matches!(outcome, CommitOutcome::Ok { .. }), "{outcome:?}");

        let outcome = commit_beside_a_newer_operand(vec![(key_of(b"a"), key_of(b"z"))]);
        assert!(
            matches!(outcome, CommitOutcome::Conflict { .. }),
            "{outcome:?}"
        );

        let outcome = commit_beside_a_newer_operand(vec![(key_of(b"a"), key_of(b"k"))]);
        assert!(matches!(outcome, CommitOutcome::Ok { .. }), "{outcome:?}");
    }

    /// A key of up to two letters over `a..=d`, the empty key included.
    fn key() -> impl Strategy<Value = Vec<u8>> {
        proptest::collection::vec(b'a'..=b'd', 0..3)
    }

    /// Every key `key()` can produce.
    fn alphabet() -> Vec<Vec<u8>> {
        let letters = b'a'..=b'd';
        let mut keys = vec![Vec::new()];
        keys.extend(letters.clone().map(|a| vec![a]));
        keys.extend(
            letters
                .clone()
                .flat_map(|a| letters.clone().map(move |b| vec![a, b])),
        );
        keys
    }

    proptest! {
        #[test]
        fn contains_matches_a_linear_model_over_every_key(
            point_ops in proptest::collection::btree_map(key(), proptest::option::of(key()), 0..8),
            range_deletes in proptest::collection::vec(
                (key(), key())
                    .prop_map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
                    .prop_filter("a range needs start < end", |(start, end)| start < end),
                0..4,
            ),
            merges in proptest::collection::vec((key(), key()), 0..6),
        ) {
            let expected: Vec<(Vec<u8>, bool)> = alphabet()
                .into_iter()
                .map(|k| {
                    let replaced = point_ops.contains_key(&k)
                        || range_deletes.iter().any(|(start, end)| *start <= k && k < *end);
                    (k, replaced)
                })
                .collect();
            let ops = grouped_batch_ops(point_ops, range_deletes, merges);
            let replaced = Replaced::of(&ops);
            for (k, expected) in expected {
                prop_assert_eq!(replaced.contains(&k), expected, "key {:?}", k);
            }
        }
    }
}
