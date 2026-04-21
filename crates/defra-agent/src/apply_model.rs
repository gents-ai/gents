//! Reference implementation of the apply model mirroring
//! `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`.
//!
//! This is test-only scaffolding: property tests and conformance tests
//! exercise it, but production apply lives in `defra-agent-cli`.
//! Conformance cases (`tests/apply_conformance.rs`) anchor the production
//! code to the semantics pinned here; property tests (`tests/apply_property.rs`)
//! exercise `diff`, `apply_one`, `apply_all` at generator scale.
//!
//! Variants, apply-order ranks, and `diff` ordering MUST agree with
//! both the Lean `ApplyReconcile` module and the Rust
//! `defra_agent_cli::collection::Collection` enum.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Collection {
    InferenceBackend,
    ToolSelection,
    InferenceProfile,
    ToolServiceRegistry,
    AgentPrincipal,
    AgentBehavior,
    ScheduledTask,
}

impl Collection {
    pub fn apply_order(self) -> u8 {
        match self {
            Collection::InferenceBackend
            | Collection::ToolSelection
            | Collection::InferenceProfile
            | Collection::ToolServiceRegistry => 0,
            Collection::AgentPrincipal => 1,
            Collection::AgentBehavior => 2,
            Collection::ScheduledTask => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocRef {
    pub collection: Collection,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub docs: BTreeMap<DocRef, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveState {
    pub desired: BTreeMap<DocRef, String>,
    pub live: BTreeMap<DocRef, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStep {
    Create(DocRef, String),
    Update(DocRef, String),
}

impl ApplyStep {
    pub fn target(&self) -> &DocRef {
        match self {
            ApplyStep::Create(d, _) | ApplyStep::Update(d, _) => d,
        }
    }
    pub fn payload(&self) -> &String {
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

/// References declared by a desired-fields payload. Abstract in the
/// Lean model (`referencesOf` returns `∅`) and abstract here — the
/// property tests keep it empty; conformance tests that care pin specific
/// references by including appropriate documents in the manifest.
pub fn references_of(_payload: &str) -> Vec<DocRef> {
    Vec::new()
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
        let s = ApplyStep::Create(d.clone(), "desired-payload".into());
        let out = apply_one(&l, &s);
        assert_eq!(out.live, live, "apply must not touch live");
        assert_eq!(out.desired.get(&d), Some(&"desired-payload".to_string()));
    }
}
