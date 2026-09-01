use gents::apply_model::{
    apply_all, apply_prefix, desired_references_closed, diff, manifest_realized,
    prefix_referrers_closed, references_of, retry_after_prefix, Collection, DesiredFields, DocRef,
    LiveState, Manifest,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn collection_strategy() -> impl Strategy<Value = Collection> {
    prop_oneof![
        Just(Collection::AgentPrincipal),
        Just(Collection::AgentBehavior),
        Just(Collection::ToolSelection),
        Just(Collection::InferenceBackend),
        Just(Collection::InferenceProfile),
        Just(Collection::ToolServiceRegistry),
        Just(Collection::ProjectionAcpBinding),
        Just(Collection::Task),
        Just(Collection::Schedule),
        Just(Collection::EventTrigger),
    ]
}

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

#[allow(dead_code)]
const _LEAF_COLLECTIONS_USED: &[Collection] = LEAF_COLLECTIONS;

fn referential_manifest_strategy_with_principal() -> impl Strategy<Value = Manifest> {
    referential_manifest_strategy().prop_flat_map(|m| {
        let behavior_refs: Vec<DocRef> = m
            .docs
            .keys()
            .filter(|d| d.collection == Collection::AgentBehavior)
            .cloned()
            .collect();

        if behavior_refs.is_empty() {
            return Just(m).boxed();
        }

        let m_clone = m.clone();
        (
            Just(m_clone),
            prop::option::of((0..behavior_refs.len(), "[a-z]{1,4}").prop_map({
                let behavior_refs = behavior_refs.clone();
                move |(idx, id)| {
                    (
                        DocRef {
                            collection: Collection::AgentPrincipal,
                            id,
                        },
                        DesiredFields::with_refs(
                            "principal-content",
                            vec![behavior_refs[idx].clone()],
                        ),
                    )
                }
            })),
        )
            .prop_map(|(mut m, principal_opt)| {
                if let Some((pref, pfields)) = principal_opt {
                    m.docs.insert(pref, pfields);
                }
                m
            })
            .boxed()
    })
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

    #[test]
    fn apply_ordering_preserves_references(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let steps = diff(&m, &l).into_steps();
        let mut acc = l.clone();
        for s in &steps {
            acc = apply_all(&acc, std::slice::from_ref(s));
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

    #[test]
    fn diff_is_deterministic(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let a = diff(&m, &l);
        let b = diff(&m, &l);
        prop_assert_eq!(a, b);
    }

    #[test]
    fn apply_preserves_live(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let steps = diff(&m, &l).into_steps();
        let after = apply_all(&l, &steps);
        prop_assert_eq!(&after.live, &l.live);
    }

    #[test]
    fn every_apply_prefix_preserves_live(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let steps = diff(&m, &l).into_steps();
        for prefix_len in 0..=steps.len() {
            let after_prefix = apply_prefix(&l, &steps, prefix_len);
            prop_assert_eq!(&after_prefix.live, &l.live);
        }
    }

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
            acc = apply_all(&acc, std::slice::from_ref(s));
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

    #[test]
    fn apply_orders_behavior_before_principal(
        m in referential_manifest_strategy_with_principal(),
    ) {
        let l = LiveState {
            desired: BTreeMap::new(),
            live: BTreeMap::new(),
        };
        let steps = diff(&m, &l).into_steps();
        let mut acc = l.clone();
        for s in &steps {
            acc = apply_all(&acc, std::slice::from_ref(s));
            for payload in acc.desired.values() {
                for r in references_of(payload) {
                    prop_assert!(
                        acc.desired.contains_key(&r),
                        "dangling reference {:?} after step {:?}",
                        r,
                        s,
                    );
                }
            }
        }
    }

    #[test]
    fn every_apply_prefix_closes_written_referrers(
        m in referential_manifest_strategy_with_principal(),
    ) {
        let l = LiveState {
            desired: BTreeMap::new(),
            live: BTreeMap::new(),
        };
        let steps = diff(&m, &l).into_steps();
        for prefix_len in 0..=steps.len() {
            let after_prefix = apply_prefix(&l, &steps, prefix_len);
            prop_assert!(
                desired_references_closed(&after_prefix),
                "prefix {prefix_len} left a dangling reference in the desired projection",
            );
            prop_assert!(
                prefix_referrers_closed(&steps[..prefix_len], &after_prefix),
                "prefix {prefix_len} left an already-written referrer dangling",
            );
        }
    }

    #[test]
    fn retry_after_any_prefix_matches_complete_apply(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let steps = diff(&m, &l).into_steps();
        let complete = apply_all(&l, &steps);
        for prefix_len in 0..=steps.len() {
            let retried = retry_after_prefix(&m, &l, prefix_len);
            prop_assert_eq!(&retried, &complete);
        }
    }

    #[test]
    fn apply_is_idempotent_after_convergence(
        m in manifest_strategy(),
        l in live_state_strategy(),
    ) {
        let converged = apply_all(&l, &diff(&m, &l).into_steps());
        prop_assert!(manifest_realized(&m, &converged));
        let reapplied = apply_all(&converged, &diff(&m, &converged).into_steps());
        prop_assert_eq!(reapplied, converged);
    }
}

#[allow(dead_code)]
fn _manifest_constructor_is_used(m: Manifest, l: LiveState) -> BTreeMap<DocRef, DesiredFields> {
    let _ = diff(&m, &l);
    m.docs
}
