use std::collections::BTreeSet;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

/// The model-judge check. It is development-tier in v1: a subject's output is
/// untrusted input to a judge model.
pub const LLM_JUDGE_CHECK: &str = "llm_judge";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EvalSplit {
    Train,
    Validation,
    HeldOut,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EvalTier {
    /// Reported, may carry feedback, never enters a score.
    Development,
    /// Deterministic checks only in v1. The only tier that enters a score.
    Acceptance,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EvalReducer {
    #[default]
    WeightedMean,
    All,
    LastStage,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EvalSubjectKind {
    Behavior,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalSubject {
    pub kind: EvalSubjectKind,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub inference_slots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalFixtureDocument {
    pub collection: String,
    /// An app-collection row. Its schema belongs to that collection.
    #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
    pub document: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalFixtures {
    /// Pack-asset digests to materialize in the trial workspace.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub assets: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(
        feature = "typescript",
        ts(as = "Option<Vec<EvalFixtureDocument>>", optional = nullable)
    )]
    pub documents: Vec<EvalFixtureDocument>,
    /// App-collection SDL to install in the trial before its documents.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub schemas: Vec<String>,
}

fn default_weight() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalCheckRef {
    /// A name in the builtin check registry. Definitions never contain code.
    pub check: String,
    /// Interpreted by the named check. Its schema belongs to that check.
    #[serde(default)]
    #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
    pub params: serde_json::Value,
    pub tier: EvalTier,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalStage {
    pub stage_id: String,
    pub prompt: String,
    pub deadline_secs: u64,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(
        feature = "typescript",
        ts(as = "Option<Vec<EvalCheckRef>>", optional = nullable)
    )]
    pub checks: Vec<EvalCheckRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalCase {
    pub case_id: String,
    pub split: EvalSplit,
    #[serde(default)]
    pub reducer: EvalReducer,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub fixtures: Option<EvalFixtures>,
    pub stages: Vec<EvalStage>,
}

/// A pack-carried eval definition. Identity is `(definition_id,
/// comparability_version, desired_state_document_digest)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalDefinition {
    pub definition_id: String,
    pub agent_did: String,
    /// Bumped by the author when cases, checks, reducers or judge settings
    /// change. Runs never compare across different values.
    pub comparability_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub title: Option<String>,
    pub subject: EvalSubject,
    /// Fixtures shared by every case; a case may add its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub fixtures: Option<EvalFixtures>,
    pub cases: Vec<EvalCase>,
    /// Written by the mutation owner, including the recreate-identity nonce on
    /// create. Observation metadata, never part of the identity digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub updated_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl EvalDefinition {
    pub fn validate(&self) -> Result<()> {
        let id = &self.definition_id;
        ensure!(
            !id.trim().is_empty(),
            "eval definition requires a definition_id"
        );
        ensure!(
            self.comparability_version >= 1,
            "eval definition {id} comparability_version must be at least 1"
        );
        ensure!(
            !self.cases.is_empty(),
            "eval definition {id} requires at least one case"
        );
        let mut case_ids = BTreeSet::new();
        for case in &self.cases {
            let case_id = &case.case_id;
            ensure!(
                !case_id.trim().is_empty(),
                "eval definition {id} has a case with no case_id"
            );
            ensure!(
                case_ids.insert(case_id),
                "eval definition {id} has duplicate case_id {case_id}"
            );
            ensure!(
                !case.stages.is_empty(),
                "eval definition {id} case {case_id} requires at least one stage"
            );
            let mut stage_ids = BTreeSet::new();
            let mut acceptance = 0usize;
            for stage in &case.stages {
                let stage_id = &stage.stage_id;
                ensure!(
                    !stage_id.trim().is_empty(),
                    "eval definition {id} case {case_id} has a stage with no stage_id"
                );
                ensure!(
                    stage_ids.insert(stage_id),
                    "eval definition {id} case {case_id} has duplicate stage_id {stage_id}"
                );
                ensure!(
                    stage.deadline_secs > 0,
                    "eval definition {id} case {case_id} stage {stage_id} deadline_secs must be positive"
                );
                for check in &stage.checks {
                    ensure!(
                        !check.check.trim().is_empty(),
                        "eval definition {id} case {case_id} stage {stage_id} has an empty check name"
                    );
                    ensure!(
                        check.weight >= 1,
                        "eval definition {id} case {case_id} check {} weight must be at least 1",
                        check.check
                    );
                    if check.tier == EvalTier::Acceptance {
                        ensure!(
                            check.check != LLM_JUDGE_CHECK,
                            "eval definition {id} case {case_id}: {LLM_JUDGE_CHECK} is development-tier only"
                        );
                        acceptance += 1;
                    }
                }
            }
            ensure!(
                acceptance > 0,
                "eval definition {id} case {case_id} requires at least one acceptance-tier check"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition() -> serde_json::Value {
        json!({
            "definition_id": "monitor-findings",
            "agent_did": "did:key:owner",
            "comparability_version": 1,
            "subject": {"kind": "behavior", "inference_slots": ["primary"]},
            "cases": [{
                "case_id": "disk-warning",
                "split": "validation",
                "stages": [{
                    "stage_id": "check",
                    "prompt": "Run the monitor.",
                    "deadline_secs": 600,
                    "checks": [
                        {"check": "mailbox_findings", "params": {"expect": ["disk"]}, "tier": "acceptance"},
                        {"check": "llm_judge", "params": {"rubric": "clear"}, "tier": "development"}
                    ]
                }]
            }]
        })
    }

    fn parse(value: serde_json::Value) -> EvalDefinition {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_minimal_definition_validates_and_defaults() {
        let parsed = parse(definition());
        parsed.validate().unwrap();
        assert_eq!(parsed.cases[0].reducer, EvalReducer::WeightedMean);
        assert_eq!(parsed.cases[0].stages[0].checks[0].weight, 1);
        assert!(parsed.tags.is_empty());
        let serialized = serde_json::to_value(&parsed).unwrap();
        assert!(
            serialized.get("tags").is_none(),
            "an empty list is omitted, never []"
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut value = definition();
        value["status"] = "running".into();
        assert!(serde_json::from_value::<EvalDefinition>(value).is_err());
    }

    fn invalid(mutate: impl FnOnce(&mut serde_json::Value), needle: &str) {
        let mut value = definition();
        mutate(&mut value);
        let error = parse(value).validate().unwrap_err();
        assert!(format!("{error:#}").contains(needle), "{error:#}");
    }

    #[test]
    fn structural_rules_are_enforced() {
        invalid(
            |v| v["comparability_version"] = 0.into(),
            "comparability_version",
        );
        invalid(|v| v["cases"] = json!([]), "at least one case");
        invalid(
            |v| v["cases"][0]["stages"] = json!([]),
            "at least one stage",
        );
        invalid(
            |v| v["cases"][0]["stages"][0]["deadline_secs"] = 0.into(),
            "deadline_secs",
        );
        invalid(
            |v| v["cases"][0]["stages"][0]["checks"][0]["weight"] = 0.into(),
            "weight",
        );
        invalid(
            |v| v["cases"][0]["stages"][0]["checks"][0]["check"] = " ".into(),
            "check name",
        );
        invalid(
            |v| {
                let case = v["cases"][0].clone();
                v["cases"].as_array_mut().unwrap().push(case);
            },
            "duplicate case_id",
        );
        invalid(
            |v| {
                let stage = v["cases"][0]["stages"][0].clone();
                v["cases"][0]["stages"].as_array_mut().unwrap().push(stage);
            },
            "duplicate stage_id",
        );
    }

    #[test]
    fn a_case_needs_an_acceptance_check_and_judges_stay_development() {
        invalid(
            |v| v["cases"][0]["stages"][0]["checks"][0]["tier"] = "development".into(),
            "acceptance-tier check",
        );
        invalid(
            |v| v["cases"][0]["stages"][0]["checks"][1]["tier"] = "acceptance".into(),
            "llm_judge",
        );
    }

    /// `mint_recreate_identity` stamps a fresh `updated_at` so that reinstalling
    /// a byte-identical pack gets a new content-addressed docID. That nonce must
    /// never reach the identity digest, or every reinstall would look changed.
    #[test]
    fn updated_at_is_carried_but_never_enters_the_identity_digest() {
        let digest = |updated_at: &str| {
            let mut value = definition();
            value["updated_at"] = updated_at.into();
            let parsed = parse(value);
            assert_eq!(parsed.updated_at.as_deref(), Some(updated_at));
            crate::config_client::desired_state_document_digest(
                &serde_json::to_value(&parsed).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(
            digest("2026-01-01T00:00:00.000000000Z"),
            digest("2026-09-21T12:34:56.000000000Z")
        );
    }

    #[test]
    fn a_pack_config_carries_eval_definitions() {
        let pack: crate::document_config::PackConfig = serde_json::from_value(json!({
            "agent_principal": {"agent_did": "did:key:owner"},
            "eval_definitions": [definition()]
        }))
        .unwrap();
        assert_eq!(pack.eval_definitions.len(), 1);
        let plan = crate::config_client::DesiredStateApplyPlan::from_pack_config(&pack).unwrap();
        assert!(plan
            .documents()
            .iter()
            .any(|document| document.collection == crate::Collection::EvalDefinition));
    }
}
