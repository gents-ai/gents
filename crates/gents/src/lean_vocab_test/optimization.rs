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
pub(crate) struct LeanSeedTrigger {
    pub(crate) task_id: String,
    pub(crate) event_source_id: Option<String>,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanSeedSource {
    pub(crate) event_source_id: String,
    pub(crate) source_collection: String,
    pub(crate) event_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanSeedCase {
    pub(crate) split: String,
    pub(crate) seed_collections: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanSeedCoverageRow {
    pub(crate) name: String,
    pub(crate) target_task: Option<String>,
    pub(crate) task_enabled: bool,
    pub(crate) triggers: Vec<LeanSeedTrigger>,
    pub(crate) sources: Vec<LeanSeedSource>,
    pub(crate) cases: Vec<LeanSeedCase>,
    pub(crate) missing_splits: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanExactSeed {
    pub(crate) case_id: String,
    pub(crate) stage_id: String,
    pub(crate) collection: String,
    pub(crate) document: serde_json::Value,
    pub(crate) rendered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanSeedRenderingRow {
    pub(crate) name: String,
    pub(crate) target_task: Option<String>,
    pub(crate) task_enabled: bool,
    pub(crate) triggers: Vec<LeanSeedTrigger>,
    pub(crate) sources: Vec<LeanSeedSource>,
    pub(crate) text: String,
    pub(crate) seeds: Vec<LeanExactSeed>,
    pub(crate) failed_stages: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanOptimizationCases {
    pub(crate) params: LeanOptimizationParams,
    pub(crate) decisions: Vec<LeanDecisionRow>,
    pub(crate) gates: Vec<LeanGateRow>,
    pub(crate) costs: Vec<LeanCostRow>,
    pub(crate) seed_coverage: Vec<LeanSeedCoverageRow>,
    pub(crate) seed_rendering: Vec<LeanSeedRenderingRow>,
}
