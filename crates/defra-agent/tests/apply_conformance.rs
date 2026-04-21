//! Table-driven conformance tests pinning the Rust `apply_model` diff/apply
//! outputs to the semantics expected by the Lean `ApplyReconcile` module.
//!
//! Each case is `(initial_live_state, manifest) → expected_behavior`. Keep
//! the case count small — exhaustive checking is the property tests' job;
//! this file anchors the model to specific concrete inputs an engineer can
//! reason about without running proptest.

use defra_agent::apply_model::{
    apply_all, diff, ApplyStep, Collection, DocRef, LiveState, Manifest,
};
use std::collections::BTreeMap;

fn r(c: Collection, id: &str) -> DocRef {
    DocRef {
        collection: c,
        id: id.to_string(),
    }
}

fn manifest(pairs: &[(DocRef, &str)]) -> Manifest {
    let mut docs = BTreeMap::new();
    for (d, f) in pairs {
        docs.insert(d.clone(), (*f).to_string());
    }
    Manifest { docs }
}

fn live(desired: &[(DocRef, &str)], live: &[(DocRef, &str)]) -> LiveState {
    let mut desired_map = BTreeMap::new();
    for (d, f) in desired {
        desired_map.insert(d.clone(), (*f).to_string());
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
        after.desired.get(&backend),
        Some(&"b1-desired".to_string()),
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
            assert_eq!(f, "b1-new");
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
