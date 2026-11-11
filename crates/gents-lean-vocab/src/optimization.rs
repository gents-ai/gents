use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanOptimizationParams {
    pub min_pairs: u64,
    pub max_not_evidence_bp: u64,
    pub max_asymmetry_bp: u64,
    pub case_tolerance_bp: u64,
    pub alpha_ppm: u64,
    pub max_rounds: u32,
    pub max_token_increase_bp: u64,
    pub alpha_effective_ppm: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanDecisionRow {
    pub mode: String,
    pub sufficient: bool,
    pub no_case_regression: bool,
    pub cost_ok: bool,
    pub improves: bool,
    pub decision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanCasePairs {
    pub pairs: u64,
    pub sum_baseline: u64,
    pub sum_candidate: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGateRow {
    pub name: String,
    pub cases_match: bool,
    pub cases: Vec<LeanCasePairs>,
    pub keys: u64,
    pub dropped_baseline: u64,
    pub dropped_candidate: u64,
    pub sufficient: bool,
    pub no_case_regression: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanCostRow {
    pub name: String,
    pub baseline_tokens: u64,
    pub baseline_trials: u64,
    pub candidate_tokens: u64,
    pub candidate_trials: u64,
    pub cost_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedTrigger {
    pub task_id: String,
    pub event_source_id: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedSource {
    pub event_source_id: String,
    pub source_collection: String,
    pub event_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedCase {
    pub split: String,
    pub seed_collections: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedCoverageRow {
    pub name: String,
    pub target_task: Option<String>,
    pub task_enabled: bool,
    pub triggers: Vec<LeanSeedTrigger>,
    pub sources: Vec<LeanSeedSource>,
    pub cases: Vec<LeanSeedCase>,
    pub missing_splits: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanExactSeed {
    pub case_id: String,
    pub stage_id: String,
    pub collection: String,
    pub document: serde_json::Value,
    pub rendered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanSeedRenderingRow {
    pub name: String,
    pub target_task: Option<String>,
    pub task_enabled: bool,
    pub triggers: Vec<LeanSeedTrigger>,
    pub sources: Vec<LeanSeedSource>,
    pub text: String,
    pub seeds: Vec<LeanExactSeed>,
    pub failed_stages: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanOptimizationCases {
    pub params: LeanOptimizationParams,
    pub decisions: Vec<LeanDecisionRow>,
    pub gates: Vec<LeanGateRow>,
    pub costs: Vec<LeanCostRow>,
    pub seed_coverage: Vec<LeanSeedCoverageRow>,
    pub seed_rendering: Vec<LeanSeedRenderingRow>,
}
