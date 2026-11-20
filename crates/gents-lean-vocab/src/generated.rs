use crate::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn lean_feature_matrix() -> &'static LeanFeatureMatrix {
    &lean_contract_snapshot().feature_matrix
}

pub fn lean_feature_surface_requirements() -> &'static [LeanFeatureSurfaceRequirement] {
    &lean_contract_snapshot().feature_surface_requirements
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanContractSnapshot {
    pub generated_by: String,
    pub families: BTreeMap<String, LeanFamily>,
    pub family_vocabularies: BTreeMap<String, serde_json::Value>,
    pub coverage_ledger: Vec<LeanCoverageEntry>,
    pub feature_surface_requirements: Vec<LeanFeatureSurfaceRequirement>,
    pub feature_matrix: LeanFeatureMatrix,
    pub recovery_sweep_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct LeanCoverageEntry {
    pub category: String,
    pub domain: String,
    pub consumed: bool,
    pub accepted_boundary: String,
    pub accepted_follow_up: String,
    pub feature: String,
    pub surfaces: Vec<LeanSurface>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFamily {
    pub input: Value,
    pub output: Value,
    pub cases: Vec<LeanFamilyCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFamilyCase {
    pub input: Value,
    pub expected: Value,
}

#[derive(Debug, Deserialize)]
pub struct LeanFeatureMatrixCell {
    pub coverage_strength: String,
    pub row_count: usize,
    pub pending_follow_ups: usize,
    pub deferred_note: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanFeatureSurfaceDeferral {
    pub surface: LeanSurface,
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanFeatureSurfaceRequirement {
    pub feature: String,
    pub required: Vec<LeanSurface>,
    pub deferred: Vec<LeanFeatureSurfaceDeferral>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeanSurface {
    AgentFacing,
    OperatorCli,
    OperatorUi,
    Api,
    RuntimeInternal,
}
