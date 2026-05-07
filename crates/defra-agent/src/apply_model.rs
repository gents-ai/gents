//! Reference implementation of the apply model mirroring
//! `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`.
//!
//! This is test-only scaffolding: property tests and conformance tests
//! exercise it, but production apply lives in `defra-agent-cli`.
//! Conformance cases (`tests/apply_conformance.rs`) anchor the production
//! code to the semantics pinned here; property tests (`tests/apply_property.rs`)
//! exercise `diff`, `apply_one`, `apply_prefix`, `retry_after_prefix`, and
//! `apply_all` at generator scale.
//!
//! Variants, apply-order ranks, and `diff` ordering MUST agree with
//! both the Lean `ApplyReconcile` module and the Rust
//! `defra_agent::Collection` enum.

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
    /// Shorthand constructor for payloads with no references.
    pub fn opaque(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            refs: Vec::new(),
        }
    }

    /// Constructor for payloads with references.
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
    pub live: BTreeMap<DocRef, String>, // live stays String — opaque runtime payload
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStep {
    Create(DocRef, DesiredFields),
    Update(DocRef, DesiredFields),
}

impl ApplyStep {
    pub fn target(&self) -> &DocRef {
        match self {
            ApplyStep::Create(d, _) | ApplyStep::Update(d, _) => d,
        }
    }
    pub fn payload(&self) -> &DesiredFields {
        match self {
            ApplyStep::Create(_, f) | ApplyStep::Update(_, f) => f,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReport {
    pub create: Vec<DocRef>,
    pub update: Vec<DocRef>,
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

pub fn diff(m: &Manifest, l: &LiveState) -> DiffReport {
    let mut create = Vec::new();
    let mut update = Vec::new();
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
            (None, Some(_)) => live_only.push((*d).clone()),
            (None, None) => unreachable!("BTreeSet union contains neither side"),
        }
    }

    steps.sort_by_key(|s| (s.target().collection.apply_order(), s.target().id.clone()));

    DiffReport {
        create,
        update,
        unchanged,
        live_only,
        steps,
    }
}

pub fn apply_one(l: &LiveState, s: &ApplyStep) -> LiveState {
    let mut desired = l.desired.clone();
    desired.insert(s.target().clone(), s.payload().clone());
    LiveState {
        desired,
        live: l.live.clone(),
    }
}

pub fn apply_all(l: &LiveState, steps: &[ApplyStep]) -> LiveState {
    steps.iter().fold(l.clone(), |acc, s| apply_one(&acc, s))
}

/// Apply the first `prefix_len` steps from a previously computed diff.
/// This is the Rust analog of Lean `ApplyPrefix.state`: it models a crash or
/// interruption after a durable prefix of the ordered write sequence.
pub fn apply_prefix(l: &LiveState, steps: &[ApplyStep], prefix_len: usize) -> LiveState {
    debug_assert!(
        prefix_len <= steps.len(),
        "apply prefix length must not exceed the diff length",
    );
    let len = prefix_len.min(steps.len());
    apply_all(l, &steps[..len])
}

/// Recompute diff after an applied prefix and run the retry pass to completion.
pub fn retry_after_prefix(m: &Manifest, l: &LiveState, prefix_len: usize) -> LiveState {
    let initial_steps = diff(m, l).into_steps();
    let prefix_state = apply_prefix(l, &initial_steps, prefix_len);
    let retry_steps = diff(m, &prefix_state).into_steps();
    apply_all(&prefix_state, &retry_steps)
}

/// Manifest-realized predicate mirrored from Lean:
/// every manifest document is present with the requested desired payload.
pub fn manifest_realized(m: &Manifest, l: &LiveState) -> bool {
    m.docs
        .iter()
        .all(|(doc, desired)| l.desired.get(doc) == Some(desired))
}

/// Full desired-projection reference closure.
pub fn desired_references_closed(l: &LiveState) -> bool {
    l.desired
        .values()
        .flat_map(references_of)
        .all(|r| l.desired.contains_key(&r))
}

/// Product-facing corollary scoped to documents already written by a prefix.
pub fn prefix_referrers_closed(prefix: &[ApplyStep], l: &LiveState) -> bool {
    prefix.iter().all(|step| {
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
}
