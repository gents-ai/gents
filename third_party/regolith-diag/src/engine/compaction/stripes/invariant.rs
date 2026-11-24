//! The invariant stripes exist to keep: a reader that can exist sees the
//! same value of a key before and after the reduction.

use proptest::prelude::*;

use super::fixtures::{Append, KEY, Partial, Sum, del, entry, show};
use super::*;

/// Which operator a generated case runs against.
#[derive(Clone, Copy, Debug)]
enum Flavor {
    AppendFolding,
    AppendUnfolding,
    Sum,
}

impl Flavor {
    /// The operator this flavor runs.
    fn operator(self) -> Box<dyn MergeOperator> {
        match self {
            Self::AppendFolding => Box::new(Append(Partial::Always)),
            Self::AppendUnfolding => Box::new(Append(Partial::Never)),
            Self::Sum => Box::new(Sum),
        }
    }

    /// A value or operand carrying `n`, encoded for this flavor.
    fn payload(self, n: u8) -> Vec<u8> {
        match self {
            Self::AppendFolding | Self::AppendUnfolding => vec![b'a' + n],
            Self::Sum => i64::from(n).to_be_bytes().to_vec(),
        }
    }
}

/// What a reader at `at` makes of `group`, which is newest first.
fn read(merge: &dyn MergeOperator, group: &[Entry], at: u64) -> Option<Vec<u8>> {
    let mut operands: Vec<&[u8]> = Vec::new();
    let mut base = None;
    for (key, value) in group {
        let (_, seq, value_type) = decode_internal_key(key);
        if seq > at {
            continue;
        }
        match value_type {
            VALUE_TYPE_MERGE => operands.push(value),
            VALUE_TYPE_VALUE => {
                base = Some(value.as_slice());
                break;
            }
            _ => break,
        }
    }
    if operands.is_empty() {
        return base.map(<[u8]>::to_vec);
    }
    operands.reverse();
    merge.full_merge(KEY, base, &operands)
}

/// A group newest first as `(seq, kind, payload)`, the live snapshots, and the operator.
fn case() -> impl Strategy<Value = (Vec<(u64, u8, u8)>, Vec<u64>, Flavor)> {
    let flavor = prop_oneof![
        Just(Flavor::AppendFolding),
        Just(Flavor::AppendUnfolding),
        Just(Flavor::Sum),
    ];
    let versions = proptest::collection::btree_set(1u64..40, 0..10).prop_flat_map(|seqs| {
        let n = seqs.len();
        (
            Just(seqs),
            proptest::collection::vec((0u8..6, 0u8..8), n..=n),
        )
    });
    let live = proptest::collection::btree_set(0u64..44, 0..6);
    (versions, live, flavor).prop_map(|((seqs, picks), live, flavor)| {
        let versions = seqs
            .into_iter()
            .rev()
            .zip(picks)
            .map(|(seq, (kind, payload))| (seq, kind, payload))
            .collect();
        (versions, live.into_iter().collect(), flavor)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn reduction_leaves_every_reader_its_view(
        (versions, live, flavor) in case(),
    ) {
        let merge = flavor.operator();
        let group: Vec<Entry> = versions
            .iter()
            .map(|&(seq, kind, payload)| match kind {
                0 => del(seq),
                1 | 2 => entry(seq, VALUE_TYPE_VALUE, &flavor.payload(payload)),
                _ => entry(seq, VALUE_TYPE_MERGE, &flavor.payload(payload)),
            })
            .collect();

        let reduced = Stripes::new(&live, None, Some(merge.as_ref()), 1).reduce_group(group.clone());

        for &reader in live.iter().chain(&[u64::MAX]) {
            prop_assert_eq!(
                read(merge.as_ref(), &reduced, reader),
                read(merge.as_ref(), &group, reader),
                "reader {} over {:?} reduced to {:?}", reader, show(&group), show(&reduced)
            );
        }
        prop_assert!(reduced.len() <= group.len());
    }
}
