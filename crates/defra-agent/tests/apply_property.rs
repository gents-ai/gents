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

    /// P2 (ordering preserves references): applying `diff M L` one step at a
    /// time produces an intermediate state with no dangling references after
    /// every step. With `desired_fields_strategy` producing only empty-refs
    /// payloads, this is vacuously true; Task B5 will strengthen the generator
    /// to produce real references and make this property substantive.
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
}

#[allow(dead_code)]
fn _manifest_constructor_is_used(m: Manifest, l: LiveState) -> BTreeMap<DocRef, DesiredFields> {
    // Suppress "unused" warnings from imports when proptest-cfg'd.
    let _ = diff(&m, &l);
    m.docs
}
