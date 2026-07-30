//! Reference implementation of the apply model mirroring
//! `crates/gents/proofs/Proofs/ApplyReconcile.lean`.
//!
//! This is test-only scaffolding: property tests and conformance tests
//! exercise it, but production apply lives in `gents-cli`.
//! Conformance cases (`tests/apply_conformance.rs`) anchor the production
//! code to the semantics pinned here; property tests (`tests/apply_property.rs`)
//! exercise `diff`, `apply_one`, `apply_prefix`, `retry_after_prefix`, and
//! `apply_all` at generator scale.
//!
//! Variants, apply-order ranks, and `diff` ordering MUST agree with
//! both the Lean `ApplyReconcile` module and the Rust
//! `gents::Collection` enum.

use std::collections::{BTreeMap, BTreeSet};

pub use crate::Collection;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocRef {
    pub collection: Collection,
    pub id: String,
}

/// Mirrors the Lean `ApplyReconcile.DesiredFields` structure.
/// `content` is the opaque payload; `refs` holds cross-document
/// references that must point to strictly-lower-rank DocRefs for the
/// manifest to be WellFormed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DesiredFields {
    pub content: String,
    pub refs: Vec<DocRef>,
}

impl DesiredFields {
    pub fn opaque(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            refs: Vec::new(),
        }
    }

    pub fn with_refs(content: impl Into<String>, refs: Vec<DocRef>) -> Self {
        Self {
            content: content.into(),
            refs,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub docs: BTreeMap<DocRef, DesiredFields>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveState {
    pub desired: BTreeMap<DocRef, DesiredFields>,
    pub live: BTreeMap<DocRef, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStep {
    Create(DocRef, DesiredFields),
    Update(DocRef, DesiredFields),
    Delete(DocRef),
}

impl ApplyStep {
    pub fn target(&self) -> &DocRef {
        match self {
            ApplyStep::Create(d, _) | ApplyStep::Update(d, _) | ApplyStep::Delete(d) => d,
        }
    }

    pub fn payload(&self) -> Option<&DesiredFields> {
        match self {
            ApplyStep::Create(_, f) | ApplyStep::Update(_, f) => Some(f),
            ApplyStep::Delete(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReport {
    pub create: Vec<DocRef>,
    pub update: Vec<DocRef>,
    pub delete: Vec<DocRef>,
    pub unchanged: Vec<DocRef>,
    pub live_only: Vec<DocRef>,
    steps: Vec<ApplyStep>,
}

impl DiffReport {
    pub fn into_steps(self) -> Vec<ApplyStep> {
        self.steps
    }

    pub fn steps(&self) -> &[ApplyStep] {
        &self.steps
    }
}

/// References declared by a desired-fields payload. Projects `.refs` from the
/// `DesiredFields` struct, mirroring the Lean `referencesOf` function.
pub fn references_of(payload: &DesiredFields) -> Vec<DocRef> {
    payload.refs.clone()
}

/// Default convergence diff. Mirrors Lean `diffManaged`: writes manifest rows
/// and retracts absent rows only for manifest-authoritative collections.
pub fn diff(m: &Manifest, l: &LiveState) -> DiffReport {
    diff_inner(m, l, false)
}

pub fn diff_prune(m: &Manifest, l: &LiveState) -> DiffReport {
    diff_inner(m, l, true)
}

fn diff_inner(m: &Manifest, l: &LiveState, prune: bool) -> DiffReport {
    let mut create = Vec::new();
    let mut update = Vec::new();
    let mut delete = Vec::new();
    let mut unchanged = Vec::new();
    let mut live_only = Vec::new();
    let mut steps = Vec::new();

    let mut all: BTreeSet<&DocRef> = BTreeSet::new();
    all.extend(m.docs.keys());
    all.extend(l.desired.keys());

    for d in &all {
        match (m.docs.get(*d), l.desired.get(*d)) {
            (Some(f), None) => {
                create.push((*d).clone());
                steps.push(ApplyStep::Create((*d).clone(), f.clone()));
            }
            (Some(f), Some(g)) if f == g => unchanged.push((*d).clone()),
            (Some(f), Some(_)) => {
                update.push((*d).clone());
                steps.push(ApplyStep::Update((*d).clone(), f.clone()));
            }
            (None, Some(_)) => {
                live_only.push((*d).clone());
                if (prune || d.collection.manifest_authoritative()) && delete_safe(l, d) {
                    delete.push((*d).clone());
                }
            }
            (None, None) => unreachable!("BTreeSet union contains neither side"),
        }
    }

    steps.sort_by_key(|s| (s.target().collection.apply_order(), s.target().id.clone()));
    delete.sort_by_key(|d| (std::cmp::Reverse(d.collection.apply_order()), d.id.clone()));
    steps.extend(delete.iter().cloned().map(ApplyStep::Delete));

    DiffReport {
        create,
        update,
        delete,
        unchanged,
        live_only,
        steps,
    }
}

pub fn delete_safe(l: &LiveState, target: &DocRef) -> bool {
    l.desired
        .values()
        .flat_map(references_of)
        .all(|reference| &reference != target)
}

pub fn apply_one(l: &LiveState, s: &ApplyStep) -> LiveState {
    let mut desired = l.desired.clone();
    match s {
        ApplyStep::Create(doc, fields) | ApplyStep::Update(doc, fields) => {
            desired.insert(doc.clone(), fields.clone());
        }
        ApplyStep::Delete(doc) => {
            desired.remove(doc);
        }
    }
    LiveState {
        desired,
        live: l.live.clone(),
    }
}

pub fn apply_all(l: &LiveState, steps: &[ApplyStep]) -> LiveState {
    steps.iter().fold(l.clone(), |acc, s| apply_one(&acc, s))
}

pub fn apply_prefix(l: &LiveState, steps: &[ApplyStep], prefix_len: usize) -> LiveState {
    debug_assert!(
        prefix_len <= steps.len(),
        "apply prefix length must not exceed the diff length",
    );
    let len = prefix_len.min(steps.len());
    apply_all(l, &steps[..len])
}

pub fn retry_after_prefix(m: &Manifest, l: &LiveState, prefix_len: usize) -> LiveState {
    let initial_steps = diff(m, l).into_steps();
    let prefix_state = apply_prefix(l, &initial_steps, prefix_len);
    let retry_steps = diff(m, &prefix_state).into_steps();
    apply_all(&prefix_state, &retry_steps)
}

pub fn retry_after_prune_prefix(m: &Manifest, l: &LiveState, prefix_len: usize) -> LiveState {
    let initial_steps = diff_prune(m, l).into_steps();
    let prefix_state = apply_prefix(l, &initial_steps, prefix_len);
    let retry_steps = diff_prune(m, &prefix_state).into_steps();
    apply_all(&prefix_state, &retry_steps)
}

/// Manifest-realized predicate mirrored from Lean:
/// every manifest document is present with the requested desired payload.
pub fn manifest_realized(m: &Manifest, l: &LiveState) -> bool {
    m.docs
        .iter()
        .all(|(doc, desired)| l.desired.get(doc) == Some(desired))
}

pub fn desired_references_closed(l: &LiveState) -> bool {
    l.desired
        .values()
        .flat_map(references_of)
        .all(|r| l.desired.contains_key(&r))
}

pub fn prefix_referrers_closed(prefix: &[ApplyStep], l: &LiveState) -> bool {
    prefix.iter().all(|step| {
        if matches!(step, ApplyStep::Delete(_)) {
            return true;
        }
        l.desired.get(step.target()).is_some_and(|payload| {
            references_of(payload)
                .into_iter()
                .all(|r| l.desired.contains_key(&r))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_one_touches_only_desired() {
        let d = DocRef {
            collection: Collection::InferenceBackend,
            id: "b1".into(),
        };
        let mut live = BTreeMap::new();
        live.insert(d.clone(), "live-payload".to_string());
        let l = LiveState {
            desired: BTreeMap::new(),
            live: live.clone(),
        };
        let s = ApplyStep::Create(d.clone(), DesiredFields::opaque("desired-payload"));
        let out = apply_one(&l, &s);
        assert_eq!(out.live, live, "apply must not touch live");
        assert_eq!(
            out.desired.get(&d).map(|f| f.content.as_str()),
            Some("desired-payload"),
        );
    }

    #[test]
    fn retry_after_prefix_converges_and_is_idempotent() {
        let backend = DocRef {
            collection: Collection::InferenceBackend,
            id: "b1".into(),
        };
        let behavior = DocRef {
            collection: Collection::AgentBehavior,
            id: "a1".into(),
        };
        let mut docs = BTreeMap::new();
        docs.insert(backend.clone(), DesiredFields::opaque("backend"));
        docs.insert(
            behavior.clone(),
            DesiredFields::with_refs("behavior", vec![backend.clone()]),
        );
        let m = Manifest { docs };
        let mut live = BTreeMap::new();
        live.insert(behavior.clone(), "runtime-live".to_string());
        let l = LiveState {
            desired: BTreeMap::new(),
            live,
        };

        let steps = diff(&m, &l).into_steps();
        let prefix = apply_prefix(&l, &steps, 1);
        assert_eq!(prefix.live, l.live);
        assert!(desired_references_closed(&prefix));
        assert!(prefix_referrers_closed(&steps[..1], &prefix));

        let retried = retry_after_prefix(&m, &l, 1);
        let full = apply_all(&l, &steps);
        assert_eq!(retried, full);
        assert!(manifest_realized(&m, &retried));

        let rediff = diff(&m, &retried).into_steps();
        assert_eq!(apply_all(&retried, &rediff), retried);
    }

    #[test]
    fn prune_diff_deletes_unreferenced_live_only_doc() {
        let backend = DocRef {
            collection: Collection::InferenceBackend,
            id: "orphan".into(),
        };
        let m = Manifest {
            docs: BTreeMap::new(),
        };
        let l = LiveState {
            desired: BTreeMap::from([(backend.clone(), DesiredFields::opaque("old"))]),
            live: BTreeMap::new(),
        };

        let default_report = diff(&m, &l);
        assert_eq!(default_report.live_only, vec![backend.clone()]);
        assert!(default_report.delete.is_empty());
        assert!(default_report.steps().is_empty());

        let prune_report = diff_prune(&m, &l);
        assert_eq!(prune_report.delete, vec![backend.clone()]);
        assert_eq!(prune_report.steps(), &[ApplyStep::Delete(backend.clone())]);
        let after = apply_all(&l, prune_report.steps());
        assert!(!after.desired.contains_key(&backend));
        assert_eq!(after.live, l.live);
    }

    #[test]
    fn default_diff_retracts_manifest_authoritative_pairing() {
        let pairing = DocRef {
            collection: Collection::PeerPairingDesired,
            id: "peer".into(),
        };
        let m = Manifest {
            docs: BTreeMap::new(),
        };
        let l = LiveState {
            desired: BTreeMap::from([(pairing.clone(), DesiredFields::opaque("owned"))]),
            live: BTreeMap::new(),
        };

        let report = diff(&m, &l);
        assert_eq!(report.delete, vec![pairing.clone()]);
        assert_eq!(report.steps(), &[ApplyStep::Delete(pairing)]);
    }

    #[test]
    fn prune_diff_blocks_referenced_dependency_until_referrer_is_deleted() {
        let backend = DocRef {
            collection: Collection::InferenceBackend,
            id: "backend".into(),
        };
        let behavior = DocRef {
            collection: Collection::AgentBehavior,
            id: "behavior".into(),
        };
        let m = Manifest {
            docs: BTreeMap::new(),
        };
        let l = LiveState {
            desired: BTreeMap::from([
                (backend.clone(), DesiredFields::opaque("backend")),
                (
                    behavior.clone(),
                    DesiredFields::with_refs("behavior", vec![backend.clone()]),
                ),
            ]),
            live: BTreeMap::new(),
        };

        let first = diff_prune(&m, &l);
        assert_eq!(first.delete, vec![behavior.clone()]);
        let after_first = apply_all(&l, first.steps());
        assert!(after_first.desired.contains_key(&backend));
        assert!(!after_first.desired.contains_key(&behavior));

        let second = diff_prune(&m, &after_first);
        assert_eq!(second.delete, vec![backend.clone()]);
    }
}
