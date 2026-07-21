//! Table-driven conformance tests pinning the Rust `apply_model` diff/apply
//! outputs to the semantics expected by the Lean `ApplyReconcile` module.
//!
//! Each case is `(initial_live_state, manifest) → expected_behavior`. Keep
//! the case count small — exhaustive checking is the property tests' job;
//! this file anchors the model to specific concrete inputs an engineer can
//! reason about without running proptest.

use defra_agent::apply_model::{
    apply_all, apply_prefix, desired_references_closed, diff, diff_prune, manifest_realized,
    prefix_referrers_closed, references_of, retry_after_prefix, retry_after_prune_prefix,
    ApplyStep, Collection, DesiredFields, DocRef, LiveState, Manifest,
};
use std::collections::{BTreeMap, BTreeSet};

use crate::lean_vocab_test::{
    lean_apply_reconcile_cases, LeanApplyDesiredDoc, LeanApplyDocRef, LeanApplyLiveDoc,
    LeanApplyStep,
};

fn r(c: Collection, id: &str) -> DocRef {
    DocRef {
        collection: c,
        id: id.to_string(),
    }
}

fn manifest(pairs: &[(DocRef, &str)]) -> Manifest {
    let mut docs = BTreeMap::new();
    for (d, f) in pairs {
        docs.insert(d.clone(), DesiredFields::opaque(*f));
    }
    Manifest { docs }
}

fn live(desired: &[(DocRef, &str)], live: &[(DocRef, &str)]) -> LiveState {
    let mut desired_map = BTreeMap::new();
    for (d, f) in desired {
        desired_map.insert(d.clone(), DesiredFields::opaque(*f));
    }
    let mut live_map = BTreeMap::new();
    for (d, f) in live {
        live_map.insert(d.clone(), (*f).to_string());
    }
    LiveState {
        desired: desired_map,
        live: live_map,
    }
}

fn collection_from_lean(name: &str) -> Collection {
    Collection::ALL
        .into_iter()
        .find(|collection| collection.graphql_type() == name)
        .unwrap_or_else(|| panic!("unknown Lean apply collection {name:?}"))
}

fn doc_ref_from_lean(doc: &LeanApplyDocRef) -> DocRef {
    let collection = collection_from_lean(&doc.collection);
    assert!(
        !collection.unique_field().is_empty(),
        "production apply helpers require a unique field for {collection:?}",
    );
    DocRef {
        collection,
        id: doc.id.clone(),
    }
}

fn doc_ref_from_parts(collection: &str, id: &str) -> DocRef {
    let collection = collection_from_lean(collection);
    assert!(
        !collection.unique_field().is_empty(),
        "production apply helpers require a unique field for {collection:?}",
    );
    DocRef {
        collection,
        id: id.to_string(),
    }
}

fn desired_fields_from_lean(doc: &LeanApplyDesiredDoc) -> DesiredFields {
    DesiredFields::with_refs(
        doc.content.clone(),
        doc.refs.iter().map(doc_ref_from_lean).collect(),
    )
}

fn manifest_from_lean(docs: &[LeanApplyDesiredDoc]) -> Manifest {
    Manifest {
        docs: docs
            .iter()
            .map(|doc| {
                (
                    doc_ref_from_parts(&doc.collection, &doc.id),
                    desired_fields_from_lean(doc),
                )
            })
            .collect(),
    }
}

fn live_state_from_lean(desired: &[LeanApplyDesiredDoc], live: &[LeanApplyLiveDoc]) -> LiveState {
    LiveState {
        desired: desired
            .iter()
            .map(|doc| {
                (
                    doc_ref_from_parts(&doc.collection, &doc.id),
                    desired_fields_from_lean(doc),
                )
            })
            .collect(),
        live: live
            .iter()
            .map(|doc| {
                (
                    doc_ref_from_parts(&doc.collection, &doc.id),
                    doc.content.clone(),
                )
            })
            .collect(),
    }
}

fn desired_map_from_lean(docs: &[LeanApplyDesiredDoc]) -> BTreeMap<DocRef, DesiredFields> {
    docs.iter()
        .map(|doc| {
            (
                doc_ref_from_parts(&doc.collection, &doc.id),
                desired_fields_from_lean(doc),
            )
        })
        .collect()
}

fn doc_ref_set_from_lean(refs: &[LeanApplyDocRef]) -> BTreeSet<DocRef> {
    refs.iter().map(doc_ref_from_lean).collect()
}

fn doc_ref_set(refs: &[DocRef]) -> BTreeSet<DocRef> {
    refs.iter().cloned().collect()
}

fn step_contract(step: &ApplyStep) -> (String, DocRef, DesiredFields) {
    match step {
        ApplyStep::Create(doc, fields) => ("create".to_string(), doc.clone(), fields.clone()),
        ApplyStep::Update(doc, fields) => ("update".to_string(), doc.clone(), fields.clone()),
        ApplyStep::Delete(doc) => ("delete".to_string(), doc.clone(), DesiredFields::opaque("")),
    }
}

fn step_contract_from_lean(step: &LeanApplyStep) -> (String, DocRef, DesiredFields) {
    (
        step.action.clone(),
        doc_ref_from_lean(&step.target),
        DesiredFields::with_refs(
            step.content.clone(),
            step.refs.iter().map(doc_ref_from_lean).collect(),
        ),
    )
}

fn assert_steps_match_lean(case_name: &str, actual: &[ApplyStep], expected: &[LeanApplyStep]) {
    let actual = actual.iter().map(step_contract).collect::<Vec<_>>();
    let expected = expected
        .iter()
        .map(step_contract_from_lean)
        .collect::<Vec<_>>();
    assert_eq!(
        actual, expected,
        "apply_model diff steps must match Lean case {case_name}"
    );
}

fn assert_steps_are_production_apply_ordered(case_name: &str, steps: &[ApplyStep]) {
    for pair in steps.windows(2) {
        let left = pair[0].target();
        let right = pair[1].target();
        let left_key = (left.collection.apply_order(), left.id.as_str());
        let right_key = (right.collection.apply_order(), right.id.as_str());
        assert!(
            left_key <= right_key,
            "case {case_name} emitted out-of-order production apply step: {left:?} before {right:?}",
        );
    }
}

fn assert_manifest_refs_point_to_lower_apply_ranks(
    case_name: &str,
    manifest: &[LeanApplyDesiredDoc],
) {
    for doc in manifest {
        let referrer = doc_ref_from_parts(&doc.collection, &doc.id);
        for referenced in &doc.refs {
            let referenced = doc_ref_from_lean(referenced);
            assert!(
                referenced.collection.apply_order() < referrer.collection.apply_order(),
                "case {case_name} has non-prefix-safe reference {referrer:?} -> {referenced:?}",
            );
        }
    }
}

#[test]
fn generated_apply_reconcile_cases_drive_apply_model_and_production_ordering() {
    let cases = lean_apply_reconcile_cases();
    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    for required in [
        "empty_manifest",
        "backend_before_behavior_ordering",
        "update_existing_backend",
        "live_only_no_op",
        "managed_pairing_absent_retracts_without_prune",
        "prune_live_only_unreferenced_backend",
        "prune_blocks_referenced_dependency",
        "prefix_retry_convergence_idempotence",
        "referrer_closure",
    ] {
        assert!(
            names.contains(required),
            "Lean did not emit required ApplyReconcile case {required:?}",
        );
    }

    for case in cases {
        let manifest = manifest_from_lean(&case.manifest);
        let live = live_state_from_lean(&case.pre_desired, &case.pre_live);
        let report = if case.prune_mode {
            diff_prune(&manifest, &live)
        } else {
            diff(&manifest, &live)
        };
        let steps = report.steps().to_vec();

        assert_eq!(
            doc_ref_set(&report.create),
            doc_ref_set_from_lean(&case.expected_create),
            "create bucket mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            doc_ref_set(&report.update),
            doc_ref_set_from_lean(&case.expected_update),
            "update bucket mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            doc_ref_set(&report.delete),
            doc_ref_set_from_lean(&case.expected_delete),
            "delete bucket mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            doc_ref_set(&report.unchanged),
            doc_ref_set_from_lean(&case.expected_unchanged),
            "unchanged bucket mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            doc_ref_set(&report.live_only),
            doc_ref_set_from_lean(&case.expected_live_only),
            "live-only bucket mismatch for Lean case {}",
            case.name,
        );

        assert_steps_match_lean(&case.name, &steps, &case.expected_steps);
        assert_steps_are_production_apply_ordered(&case.name, &steps);
        assert_manifest_refs_point_to_lower_apply_ranks(&case.name, &case.manifest);

        let prefix = apply_prefix(&live, &steps, case.prefix_len);
        assert_eq!(
            prefix.desired,
            desired_map_from_lean(&case.expected_prefix_desired),
            "prefix desired projection mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            prefix_referrers_closed(&steps[..case.prefix_len], &prefix),
            case.prefix_referrers_closed,
            "prefix referrer-closure mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            desired_references_closed(&prefix),
            case.desired_references_closed_after_prefix,
            "prefix desired reference-closure mismatch for Lean case {}",
            case.name,
        );

        let after = apply_all(&live, &steps);
        assert_eq!(
            after.desired,
            desired_map_from_lean(&case.expected_after_desired),
            "complete apply desired projection mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            after.live == live.live,
            case.live_preserved,
            "live projection preservation mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            manifest_realized(&manifest, &after),
            case.manifest_realized_after,
            "manifest realization mismatch for Lean case {}",
            case.name,
        );

        let retry_steps = if case.prune_mode {
            diff_prune(&manifest, &prefix).into_steps()
        } else {
            diff(&manifest, &prefix).into_steps()
        };
        assert_eq!(
            retry_steps.len(),
            case.expected_retry_step_count,
            "retry step count mismatch for Lean case {}",
            case.name,
        );
        let retry = apply_all(&prefix, &retry_steps);
        assert_eq!(
            retry.desired,
            desired_map_from_lean(&case.expected_retry_desired),
            "retry desired projection mismatch for Lean case {}",
            case.name,
        );
        let helper_retry = if case.prune_mode {
            retry_after_prune_prefix(&manifest, &live, case.prefix_len)
        } else {
            retry_after_prefix(&manifest, &live, case.prefix_len)
        };
        assert_eq!(
            helper_retry, retry,
            "retry_after_prefix helper mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            retry == after,
            case.retry_converges,
            "retry convergence mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            retry, after,
            "retry must converge to complete apply for Lean case {}",
            case.name,
        );

        let rediff = if case.prune_mode {
            diff_prune(&manifest, &after).into_steps()
        } else {
            diff(&manifest, &after).into_steps()
        };
        assert_eq!(
            rediff.len(),
            case.expected_rediff_step_count,
            "post-convergence rediff count mismatch for Lean case {}",
            case.name,
        );
        assert_eq!(
            apply_all(&after, &rediff) == after,
            case.idempotent_after,
            "idempotence mismatch for Lean case {}",
            case.name,
        );
    }
}

#[test]
fn empty_manifest_over_empty_state_produces_no_steps() {
    let m = manifest(&[]);
    let l = live(&[], &[]);
    assert!(diff(&m, &l).into_steps().is_empty());
}

#[test]
fn manifest_with_backend_and_behavior_orders_backend_first() {
    let backend = r(Collection::InferenceBackend, "b1");
    let behavior = r(Collection::AgentBehavior, "a1");
    let m = manifest(&[
        (backend.clone(), "b1-desired"),
        (behavior.clone(), "a1-desired"),
    ]);
    let l = live(&[], &[]);

    let steps: Vec<ApplyStep> = diff(&m, &l).into_steps();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].target(), &backend, "backend must be created first");
    assert_eq!(steps[1].target(), &behavior);
}

#[test]
fn unchanged_desired_produces_no_step() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = manifest(&[(backend.clone(), "b1-desired")]);
    let l = live(&[(backend.clone(), "b1-desired")], &[]);

    assert!(diff(&m, &l).into_steps().is_empty());
}

#[test]
fn live_only_document_is_reported_but_emits_no_step() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = manifest(&[]);
    let l = live(&[(backend.clone(), "b1-desired")], &[]);

    let report = diff(&m, &l);
    assert!(report.live_only.contains(&backend));
    assert!(
        report.into_steps().is_empty(),
        "live-only docs must not produce steps"
    );
}

#[test]
fn apply_preserves_live_projection_end_to_end() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = manifest(&[(backend.clone(), "b1-desired")]);
    let l = live(&[], &[(backend.clone(), "live-probe-data")]);

    let steps = diff(&m, &l).into_steps();
    let after = apply_all(&l, &steps);
    assert_eq!(
        after.live.get(&backend),
        Some(&"live-probe-data".to_string()),
        "apply must not touch the live projection"
    );
    assert_eq!(
        after.desired.get(&backend).map(|f| f.content.as_str()),
        Some("b1-desired"),
        "apply must install the desired payload"
    );
}

#[test]
fn update_is_emitted_when_desired_differs_from_live_desired() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = manifest(&[(backend.clone(), "b1-new")]);
    let l = live(&[(backend.clone(), "b1-old")], &[]);

    let report = diff(&m, &l);
    assert!(report.update.contains(&backend));
    let steps = report.into_steps();
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        ApplyStep::Update(d, f) => {
            assert_eq!(d, &backend);
            assert_eq!(f.content, "b1-new");
        }
        other => panic!("expected Update, got {:?}", other),
    }
}

#[test]
fn diff_sorts_same_applyorder_collections_by_id() {
    let b1 = r(Collection::InferenceBackend, "b1");
    let b2 = r(Collection::InferenceBackend, "b2");
    let m = manifest(&[(b2.clone(), "b2"), (b1.clone(), "b1")]);
    let l = live(&[], &[]);

    let steps = diff(&m, &l).into_steps();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].target(), &b1, "ids sort ascending within a rank");
    assert_eq!(steps[1].target(), &b2);
}

#[test]
fn manifest_with_behavior_referencing_backend_orders_backend_first() {
    let backend = r(Collection::InferenceBackend, "b1");
    let behavior = r(Collection::AgentBehavior, "a1");

    // a1 references b1 — so b1 must be written first.
    let m = Manifest {
        docs: {
            let mut docs = BTreeMap::new();
            docs.insert(backend.clone(), DesiredFields::opaque("b1-desired"));
            docs.insert(
                behavior.clone(),
                DesiredFields::with_refs("a1-desired", vec![backend.clone()]),
            );
            docs
        },
    };
    let l = LiveState {
        desired: BTreeMap::new(),
        live: BTreeMap::new(),
    };

    let steps: Vec<ApplyStep> = diff(&m, &l).into_steps();
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps[0].target(),
        &backend,
        "referenced backend must be written before the behavior that references it",
    );
    assert_eq!(steps[1].target(), &behavior);

    // After full apply, the behavior's reference resolves in acc.desired.
    let after = apply_all(&l, &steps);
    assert!(after.desired.contains_key(&backend));
    assert!(after.desired.contains_key(&behavior));
    for rf in references_of(after.desired.get(&behavior).unwrap()) {
        assert!(
            after.desired.contains_key(&rf),
            "reference {:?} should resolve after apply",
            rf,
        );
    }
}

#[test]
fn manifest_with_principal_referencing_behavior_orders_behavior_first() {
    // Models AgentPrincipal.default_behavior_id as a structural reference.
    // Principal (rank 3) must be written AFTER its referenced behavior
    // (rank 1) so the control watcher does not observe a principal
    // pointing at a missing behavior.
    let behavior = r(Collection::AgentBehavior, "a1");
    let principal = r(Collection::AgentPrincipal, "did:x");

    let m = Manifest {
        docs: {
            let mut docs = BTreeMap::new();
            docs.insert(behavior.clone(), DesiredFields::opaque("a1-desired"));
            docs.insert(
                principal.clone(),
                DesiredFields::with_refs("did:x-desired", vec![behavior.clone()]),
            );
            docs
        },
    };
    let l = LiveState {
        desired: BTreeMap::new(),
        live: BTreeMap::new(),
    };

    let steps: Vec<ApplyStep> = diff(&m, &l).into_steps();
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps[0].target(),
        &behavior,
        "referenced behavior must be written before the principal that references it",
    );
    assert_eq!(steps[1].target(), &principal);

    // After full apply, the principal's default-behavior reference resolves.
    let after = apply_all(&l, &steps);
    assert!(after.desired.contains_key(&behavior));
    assert!(after.desired.contains_key(&principal));
    for r in references_of(after.desired.get(&principal).unwrap()) {
        assert!(
            after.desired.contains_key(&r),
            "principal's default_behavior reference {:?} should resolve after apply",
            r,
        );
    }
}

#[test]
fn retry_from_every_prefix_converges_to_complete_apply() {
    let backend = r(Collection::InferenceBackend, "b1");
    let behavior = r(Collection::AgentBehavior, "a1");
    let task = r(Collection::Task, "t1");
    let m = Manifest {
        docs: {
            let mut docs = BTreeMap::new();
            docs.insert(backend.clone(), DesiredFields::opaque("b1-desired"));
            docs.insert(
                behavior.clone(),
                DesiredFields::with_refs("a1-desired", vec![backend.clone()]),
            );
            docs.insert(
                task.clone(),
                DesiredFields::with_refs("t1-desired", vec![behavior.clone()]),
            );
            docs
        },
    };
    let l = live(&[], &[(behavior.clone(), "runtime-owned")]);

    let steps = diff(&m, &l).into_steps();
    let complete = apply_all(&l, &steps);
    assert!(manifest_realized(&m, &complete));
    for prefix_len in 0..=steps.len() {
        let prefix = apply_prefix(&l, &steps, prefix_len);
        assert_eq!(
            prefix.live, l.live,
            "prefix {prefix_len} must preserve runtime/live fields",
        );
        assert!(
            desired_references_closed(&prefix),
            "prefix {prefix_len} should keep the full desired projection reference-closed",
        );
        assert!(
            prefix_referrers_closed(&steps[..prefix_len], &prefix),
            "prefix {prefix_len} should not leave already-written referrers dangling",
        );
        assert_eq!(
            retry_after_prefix(&m, &l, prefix_len),
            complete,
            "retry after prefix {prefix_len} must converge to complete apply",
        );
    }
}

#[test]
fn complete_apply_is_idempotent_after_convergence() {
    let backend = r(Collection::InferenceBackend, "b1");
    let m = manifest(&[(backend.clone(), "b1-new")]);
    let l = live(
        &[(backend.clone(), "b1-old")],
        &[(backend.clone(), "runtime")],
    );

    let converged = apply_all(&l, &diff(&m, &l).into_steps());
    assert!(manifest_realized(&m, &converged));
    let reapplied = apply_all(&converged, &diff(&m, &converged).into_steps());

    assert_eq!(reapplied, converged);
}
