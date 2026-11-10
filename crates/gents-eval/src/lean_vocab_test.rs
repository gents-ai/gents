#![allow(dead_code)]
use serde::Deserialize;
use std::sync::OnceLock;
#[path = "../../gents/src/lean_vocab_test/eval.rs"]
mod eval;
#[path = "../../gents/src/lean_vocab_test/optimization.rs"]
mod optimization;
use eval::*;
use optimization::*;

#[derive(Deserialize)]
pub(crate) struct Snapshot {
    eval_outcome_cases: Vec<LeanEvalOutcomeCase>,
    eval_document_capture_cases: Vec<LeanEvalDocumentCaptureCase>,
    optimization_cases: LeanOptimizationCases,
    identity_household_cases: Vec<Household>,
    request_transition_cases: Vec<Transition>,
    pack_eval_subject_cases: Vec<Subject>,
    pub(crate) identity_eval_record_cases: Vec<serde_json::Value>,
    pub(crate) identity_optimization_promotion_cases: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
pub(crate) struct Household {
    pub(crate) objects: Vec<Object>,
    #[serde(default)]
    pub(crate) task_checkpoint_cases: Vec<Checkpoint>,
}
#[derive(Deserialize)]
pub(crate) struct Object {
    pub(crate) resource: String,
    pub(crate) id: String,
    pub(crate) owner: String,
}
#[derive(Deserialize)]
pub(crate) struct Checkpoint {
    pub(crate) actor: String,
    pub(crate) cursor: u64,
    pub(crate) through: u64,
    pub(crate) receipt: bool,
    pub(crate) expected: u64,
}
#[derive(Deserialize)]
pub(crate) struct Transition {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) classification: String,
    pub(crate) action: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct Subject {
    pub(crate) name: String,
    pub(crate) selected: Option<Binding>,
    pub(crate) slot: String,
    pub(crate) expected: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct Binding {
    pub(crate) slot: String,
    pub(crate) agent_did: String,
}

pub(crate) fn lean_contract_snapshot() -> &'static Snapshot {
    static SNAPSHOT: OnceLock<Snapshot> = OnceLock::new();
    SNAPSHOT.get_or_init(|| {
        gents_lean_contract::load_contract_snapshot().expect("executable Lean contracts")
    })
}
pub(crate) fn lean_eval_outcome_cases() -> &'static [LeanEvalOutcomeCase] {
    &lean_contract_snapshot().eval_outcome_cases
}
pub(crate) fn lean_eval_document_capture_cases() -> &'static [LeanEvalDocumentCaptureCase] {
    &lean_contract_snapshot().eval_document_capture_cases
}
pub(crate) fn lean_optimization_cases() -> &'static LeanOptimizationCases {
    &lean_contract_snapshot().optimization_cases
}
pub(crate) fn lean_identity_household_cases() -> &'static [Household] {
    &lean_contract_snapshot().identity_household_cases
}
pub(crate) fn lean_request_transition_cases() -> &'static [Transition] {
    &lean_contract_snapshot().request_transition_cases
}
pub(crate) fn lean_pack_eval_subject_cases() -> &'static [Subject] {
    &lean_contract_snapshot().pack_eval_subject_cases
}
