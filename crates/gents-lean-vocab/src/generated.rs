use crate::*;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn lean_feature_surface_requirements() -> &'static [LeanFeatureSurfaceRequirement] {
    &lean_contract_snapshot().feature_surface_requirements
}

#[derive(Debug, Deserialize)]
pub struct LeanBoundary {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanContractSnapshot {
    pub generated_by: String,
    pub families: BTreeMap<String, LeanFamily>,
    pub family_vocabularies: BTreeMap<String, serde_json::Value>,
    pub coverage_ledger: LeanCoverageLedger,
    pub feature_surface_requirements: Vec<LeanFeatureSurfaceRequirement>,
}

#[derive(Debug, Deserialize)]
pub struct LeanCoverageLedger {
    pub boundaries: Vec<LeanBoundary>,
    pub open: Vec<String>,
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
pub struct LeanFeatureSurfaceRequirement {
    pub feature: String,
    pub required: Vec<LeanSurfaceEvidence>,
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

#[derive(Debug, Deserialize)]
pub struct LeanSurfaceEvidence {
    pub surface: LeanSurface,
    pub family: String,
}
