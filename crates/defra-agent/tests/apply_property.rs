use defra_agent::apply_model::{
    apply_all, diff, references_of, Collection, DesiredFields, DocRef, LiveState, Manifest,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

// --- generators ---

fn collection_strategy() -> impl Strategy<Value = Collection> {
    prop_oneof![
        Just(Collection::AgentPrincipal),
        Just(Collection::AgentBehavior),
        Just(Collection::ToolSelection),
        Just(Collection::InferenceBackend),
        Just(Collection::InferenceProfile),
        Just(Collection::ToolServiceRegistry),
        Just(Collection::ScheduledTask),
    ]
}

// --- referential manifest generator (used by P2b) ---

/// Rank-0 collections that carry no references and can be referrers'
/// dependencies.
const LEAF_COLLECTIONS: &[Collection] = &[
    Collection::InferenceBackend,
    Collection::ToolSelection,
    Collection::InferenceProfile,
    Collection::ToolServiceRegistry,
];

fn leaf_docref_strategy() -> impl Strategy<Value = DocRef> {
    (
        prop_oneof![
            Just(Collection::InferenceBackend),
            Just(Collection::ToolSelection),
            Just(Collection::InferenceProfile),
            Just(Collection::ToolServiceRegistry),
        ],
        "[a-z]{1,4}",
    )
        .prop_map(|(collection, id)| DocRef { collection, id })
}

/// Generate a manifest with 1..5 leaf docs (empty refs) and 0..3
/// AgentBehavior docs whose `refs` point at one randomly-chosen leaf.
/// This ensures P2b is non-vacuous: behaviors must sort after their leaf
/// dependencies.
fn referential_manifest_strategy() -> impl Strategy<Value = Manifest> {
    prop::collection::btree_map(leaf_docref_strategy(), "[a-z]{1,4}", 1..5)
        .prop_flat_map(|leaves| {
            let leaf_keys: Vec<DocRef> = leaves.keys().cloned().collect();
            let leaves_clone = leaves.clone();
            let behavior_strategy = prop::collection::vec(
                (0..leaf_keys.len(), "[a-z]{1,4}", "[a-z]{1,4}").prop_map({
                    let leaf_keys = leaf_keys.clone();
                    move |(refi, behavior_id, content)| {
                        (
                            DocRef {
                                collection: Collection::AgentBehavior,
                                id: behavior_id,
                            },
                            DesiredFields::with_refs(content, vec![leaf_keys[refi].clone()]),
                        )
                    }
                }),
                0..3,
            );
            (Just(leaves_clone), behavior_strategy)
        })
        .prop_map(|(leaves, behaviors)| {
            let mut docs: BTreeMap<DocRef, DesiredFields> = BTreeMap::new();
            for (k, v) in leaves {
                docs.insert(k, DesiredFields::opaque(v));
            }
            for (k, v) in behaviors {
                docs.insert(k, v);
            }
            Manifest { docs }
        })
}

// Suppress dead-code warning: LEAF_COLLECTIONS is the authoritative
// documentation of which ranks are "leaves" but the strategy expands it
// inline via prop_oneof! to avoid a runtime index.
#[allow(dead_code)]
const _LEAF_COLLECTIONS_USED: &[Collection] = LEAF_COLLECTIONS;

fn docref_strategy() -> impl Strategy<Value = DocRef> {
    (collection_strategy(), "[a-z]{1,4}").prop_map(|(collection, id)| DocRef { collection, id })
}

fn desired_fields_strategy() -> impl Strategy<Value = DesiredFields> {
    "[a-z]{1,4}".prop_map(DesiredFields::opaque)
}

fn manifest_strategy() -> impl Strategy<Value = Manifest> {
    prop::collection::btree_map(docref_strategy(), desired_fields_strategy(), 0..8)
        .prop_map(|docs| Manifest { docs })
}

fn live_state_strategy() -> impl Strategy<Value = LiveState> {
    (
        prop::collection::btree_map(docref_strategy(), desired_fields_strategy(), 0..8),
        prop::collection::btree_map(docref_strategy(), "[a-z]{1,4}", 0..8),
    )
        .prop_map(|(desired, live)| LiveState { desired, live })
}

proptest! {
    /// P1 (bucket partition): diff's four buckets partition the union of
    /// manifest/live doc ids with no overlap.
    #[test]
    fn diff_buckets_partition(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let report = diff(&m, &l);
        let union: std::collections::BTreeSet<_> =
            m.docs.keys().chain(l.desired.keys()).cloned().collect();
        let mut seen = std::collections::BTreeSet::new();
        for d in report.create.iter()
            .chain(report.update.iter())
            .chain(report.unchanged.iter())
            .chain(report.live_only.iter())
        {
            prop_assert!(seen.insert(d.clone()), "duplicate in diff buckets: {:?}", d);
        }
        prop_assert_eq!(seen, union);
    }

    /// P2 (ordering preserves references — vacuous): applying `diff M L` one
    /// step at a time produces no dangling references after every step.
    /// NOTE: this property is vacuously true for the general
    /// `manifest_strategy` because `desired_fields_strategy` only emits
    /// empty-refs payloads. P2b below covers the substantive case using
    /// `referential_manifest_strategy`, which generates manifests with real
    /// cross-document references.
    #[test]
    fn apply_ordering_preserves_references(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let steps = diff(&m, &l).into_steps();
        let mut acc = l.clone();
        for s in &steps {
            acc = apply_all(&acc, &[s.clone()]);
            for payload in acc.desired.values() {
                for r in references_of(payload) {
                    prop_assert!(
                        acc.desired.contains_key(&r),
                        "dangling reference {:?} after applying {:?}",
                        r,
                        s,
                    );
                }
            }
        }
    }

    /// P3 (diff determinism): `diff` is deterministic — equal inputs produce
    /// equal DiffReports regardless of underlying iteration order.
    #[test]
    fn diff_is_deterministic(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let a = diff(&m, &l);
        let b = diff(&m, &l);
        prop_assert_eq!(a, b);
    }

    /// P4 (apply preserves live): `apply_all` does not touch the live
    /// projection — the Rust analog of the Lean `apply_preserves_live`
    /// lemma.
    #[test]
    fn apply_preserves_live(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let steps = diff(&m, &l).into_steps();
        let after = apply_all(&l, &steps);
        prop_assert_eq!(after.live, l.live);
    }

    /// P2b (referential version of apply-ordering-preserves-references):
    /// for manifests carrying real references, every intermediate state
    /// after an apply step has no dangling reference. Because `diff` sorts
    /// steps by `apply_order`, any leaf reference is written before its
    /// behavior referrer. If this fails, that is a real sort-order bug in
    /// `apply_model::diff` — do NOT suppress.
    #[test]
    fn apply_ordering_preserves_real_references(
        m in referential_manifest_strategy(),
    ) {
        let l = LiveState {
            desired: BTreeMap::new(),
            live: BTreeMap::new(),
        };
        let steps = diff(&m, &l).into_steps();
        let mut acc = l.clone();
        for s in &steps {
            acc = apply_all(&acc, &[s.clone()]);
            for (d, payload) in &acc.desired {
                for r in references_of(payload) {
                    prop_assert!(
                        acc.desired.contains_key(&r),
                        "dangling reference {:?} from {:?} after step {:?}",
                        r,
                        d,
                        s,
                    );
                }
            }
        }
    }
}

#[allow(dead_code)]
fn _manifest_constructor_is_used(m: Manifest, l: LiveState) -> BTreeMap<DocRef, DesiredFields> {
    // Suppress "unused" warnings from imports when proptest-cfg'd.
    let _ = diff(&m, &l);
    m.docs
}
