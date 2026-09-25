//! Deserialization types for the Lean optimization promotion policy rows
//! (`Optimization.sufficient`, `noCaseRegression`, `costOk` and `decideGates` in
//! `Proofs/Optimization.lean`, emitted as `optimization_cases` by
//! `Proofs/Conformance/Optimization.lean`, function
//! `Conformance.Optimization.optimizationCasesJson`). Field names and types
//! mirror the emitted JSON exactly and decode strictly: contract drift must fail
//! loudly here instead of being masked by serde defaults.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanOptimizationParams {
    pub(crate) min_pairs: u64,
    pub(crate) max_not_evidence_bp: u64,
    pub(crate) max_asymmetry_bp: u64,
    pub(crate) case_tolerance_bp: u64,
    pub(crate) alpha_ppm: u64,
    pub(crate) max_rounds: u32,
    pub(crate) max_token_increase_bp: u64,
    pub(crate) alpha_effective_ppm: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanDecisionRow {
    pub(crate) mode: String,
    pub(crate) sufficient: bool,
    pub(crate) no_case_regression: bool,
    pub(crate) cost_ok: bool,
    pub(crate) improves: bool,
    pub(crate) decision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCasePairs {
    pub(crate) pairs: u64,
    pub(crate) sum_baseline: u64,
    pub(crate) sum_candidate: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanGateRow {
    pub(crate) name: String,
    pub(crate) cases_match: bool,
    pub(crate) cases: Vec<LeanCasePairs>,
    pub(crate) keys: u64,
    pub(crate) dropped_baseline: u64,
    pub(crate) dropped_candidate: u64,
    pub(crate) sufficient: bool,
    pub(crate) no_case_regression: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCostRow {
    pub(crate) name: String,
    pub(crate) baseline_tokens: u64,
    pub(crate) baseline_trials: u64,
    pub(crate) candidate_tokens: u64,
    pub(crate) candidate_trials: u64,
    pub(crate) cost_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanOptimizationCases {
    pub(crate) params: LeanOptimizationParams,
    pub(crate) decisions: Vec<LeanDecisionRow>,
    pub(crate) gates: Vec<LeanGateRow>,
    pub(crate) costs: Vec<LeanCostRow>,
}
