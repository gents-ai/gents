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

/// Evidence a stage collects from the trial after it ends, keyed by `name`
/// for the checks of that stage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EvalCapture {
    /// App-collection rows matching a DefraDB filter.
    Documents {
        name: String,
        collection: String,
        /// A DefraDB filter object for `collection`.
        #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
        filter: serde_json::Value,
        /// Fields to read; empty reads every field.
        #[serde(default)]
        fields: Vec<String>,
    },
    /// Workspace files matching a glob.
    File { name: String, glob: String },
}

impl EvalCapture {
    pub fn name(&self) -> &str {
        match self {
            Self::Documents { name, .. } | Self::File { name, .. } => name,
        }
    }
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
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(
        feature = "typescript",
        ts(as = "Option<Vec<EvalCapture>>", optional = nullable)
    )]
    pub capture: Vec<EvalCapture>,
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
                let mut check_names = BTreeSet::new();
                for check in &stage.checks {
                    ensure!(
                        !check.check.trim().is_empty(),
                        "eval definition {id} case {case_id} stage {stage_id} has an empty check name"
                    );
                    ensure!(
                        check_names.insert(check.check.as_str()),
                        "eval definition {id} case {case_id} stage {stage_id} has duplicate check {}",
                        check.check
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
                let mut capture_names = BTreeSet::new();
                for capture in &stage.capture {
                    let name = capture.name();
                    ensure!(
                        !name.trim().is_empty(),
                        "eval definition {id} case {case_id} stage {stage_id} has an empty capture name"
                    );
                    ensure!(
                        capture_names.insert(name),
                        "eval definition {id} case {case_id} stage {stage_id} has duplicate capture name {name}"
                    );
                    match capture {
                        EvalCapture::Documents { collection, .. } => ensure!(
                            !collection.trim().is_empty(),
                            "eval definition {id} case {case_id} stage {stage_id} capture {name} has an empty collection"
                        ),
                        EvalCapture::File { glob, .. } => ensure!(
                            !glob.trim().is_empty(),
                            "eval definition {id} case {case_id} stage {stage_id} capture {name} has an empty glob"
                        ),
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

    #[test]
    fn a_stage_with_a_documents_capture_round_trips() {
        let mut value = definition();
        value["cases"][0]["stages"][0]["capture"] = json!([
            {
                "kind": "documents",
                "name": "findings",
                "collection": "MonitorFinding",
                "filter": {"severity": {"_eq": "warning"}},
                "fields": ["title", "severity"]
            },
            {"kind": "file", "name": "report", "glob": "reports/*.md"}
        ]);
        let parsed = parse(value.clone());
        parsed.validate().unwrap();
        assert_eq!(
            parsed.cases[0].stages[0].capture[0],
            EvalCapture::Documents {
                name: "findings".into(),
                collection: "MonitorFinding".into(),
                filter: json!({"severity": {"_eq": "warning"}}),
                fields: vec!["title".into(), "severity".into()],
            }
        );
        assert_eq!(
            parsed.cases[0].stages[0].capture[1],
            EvalCapture::File {
                name: "report".into(),
                glob: "reports/*.md".into(),
            }
        );
        let serialized = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            serialized["cases"][0]["stages"][0]["capture"],
            value["cases"][0]["stages"][0]["capture"]
        );
        assert_eq!(parse(serialized), parsed);

        let bare = parse(definition());
        assert!(bare.cases[0].stages[0].capture.is_empty());
        let serialized = serde_json::to_value(&bare).unwrap();
        assert!(
            serialized["cases"][0]["stages"][0].get("capture").is_none(),
            "an empty capture list is omitted, never []"
        );

        let mut defaulted = definition();
        defaulted["cases"][0]["stages"][0]["capture"] = json!([
            {"kind": "documents", "name": "rows", "collection": "Row", "filter": {}}
        ]);
        let parsed = parse(defaulted);
        assert!(matches!(
            &parsed.cases[0].stages[0].capture[0],
            EvalCapture::Documents { fields, .. } if fields.is_empty()
        ));

        let mut unknown = definition();
        unknown["cases"][0]["stages"][0]["capture"] =
            json!([{"kind": "file", "name": "r", "glob": "*", "extra": 1}]);
        assert!(serde_json::from_value::<EvalDefinition>(unknown).is_err());
    }

    #[test]
    fn duplicate_check_refs_in_one_stage_are_refused() {
        invalid(
            |v| {
                let check = v["cases"][0]["stages"][0]["checks"][0].clone();
                v["cases"][0]["stages"][0]["checks"]
                    .as_array_mut()
                    .unwrap()
                    .push(check);
            },
            "case disk-warning stage check has duplicate check mailbox_findings",
        );
    }

    #[test]
    fn capture_names_are_unique_and_capture_fields_non_empty() {
        invalid(
            |v| {
                v["cases"][0]["stages"][0]["capture"] = json!([
                    {"kind": "file", "name": "out", "glob": "a/*"},
                    {"kind": "documents", "name": "out", "collection": "Row", "filter": {}}
                ]);
            },
            "duplicate capture name out",
        );
        invalid(
            |v| {
                v["cases"][0]["stages"][0]["capture"] =
                    json!([{"kind": "file", "name": " ", "glob": "a/*"}]);
            },
            "empty capture name",
        );
        invalid(
            |v| {
                v["cases"][0]["stages"][0]["capture"] = json!([
                    {"kind": "documents", "name": "rows", "collection": "", "filter": {}}
                ]);
            },
            "capture rows has an empty collection",
        );
        invalid(
            |v| {
                v["cases"][0]["stages"][0]["capture"] =
                    json!([{"kind": "file", "name": "out", "glob": ""}]);
            },
            "capture out has an empty glob",
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
