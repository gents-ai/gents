use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

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
    Development,
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
    Agent,
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub host_bash: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalFixtureDocument {
    pub collection: String,
    /// `$trial` values bind to the trial's DID at install; keys and other
    /// strings stay literal.
    #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
    pub document: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalFixtureFile {
    /// Trial-workspace-relative; may not leave it.
    pub path: String,
    pub contents: String,
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
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(
        feature = "typescript",
        ts(as = "Option<Vec<EvalFixtureFile>>", optional = nullable)
    )]
    pub files: Vec<EvalFixtureFile>,
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
    /// Builtin check registry name.
    pub check: String,
    #[serde(default)]
    #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
    pub params: serde_json::Value,
    pub tier: EvalTier,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(tag = "kind", rename_all = "snake_case"))]
pub enum EvalCapture {
    Schema {
        name: String,
        collections: Vec<String>,
    },
    Documents {
        name: String,
        collection: String,
        #[cfg_attr(feature = "typescript", ts(type = "unknown"))]
        filter: serde_json::Value,
        /// Empty reads every field.
        #[serde(default)]
        fields: Vec<String>,
    },
    File {
        name: String,
        glob: String,
    },
}

impl EvalCapture {
    pub fn name(&self) -> &str {
        match self {
            Self::Documents { name, .. } | Self::File { name, .. } | Self::Schema { name, .. } => {
                name
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalStage {
    pub stage_id: String,
    #[serde(default)]
    pub prompt: String,
    /// A document written instead of a prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub seed: Option<EvalFixtureDocument>,
    /// Opt-in wall-clock bound: when it expires the runner interrupts the
    /// request and the stage is graded `deadline`. Absent, the stage waits for
    /// the request's own terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub deadline_secs: Option<u64>,
    /// Keep observing after the request ends, until the trial home is quiet;
    /// evidence then covers every later request of the session.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub settle: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub continuation: Option<EvalContinuation>,
    /// Append previous stage check feedback to this prompt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub review_previous: bool,
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

/// Once a settling stage's home is quiet, sends `prompt` as a new user turn
/// and settles again, until the capture named `until` holds a row or `max`
/// prods have been sent. The number sent is stage evidence, never a failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalContinuation {
    pub prompt: String,
    pub max: u32,
    pub until: String,
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
    /// Per-collection row counts for a trial watcher; never graded.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[cfg_attr(
        feature = "typescript",
        ts(as = "Option<BTreeMap<String, u64>>", optional = nullable)
    )]
    pub goal: BTreeMap<String, u64>,
}

/// A pack-carried eval definition; identity is `(definition_id,
/// comparability_version, desired_state_document_digest)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EvalDefinition {
    pub definition_id: String,
    pub node_did: String,
    /// Bumped on semantic change; runs never compare across values.
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
    /// Written by the mutation owner; never part of the identity digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub updated_at: Option<String>,
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
        validate_fixture_files(self.fixtures.as_ref(), &format!("eval definition {id}"))?;
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
            validate_fixture_files(
                case.fixtures.as_ref(),
                &format!("eval definition {id} case {case_id}"),
            )?;
            ensure!(
                !case.stages.is_empty(),
                "eval definition {id} case {case_id} requires at least one stage"
            );
            ensure!(
                case.goal
                    .keys()
                    .all(|collection| !collection.trim().is_empty()),
                "eval definition {id} case {case_id} has a goal with an empty collection name"
            );
            let mut stage_ids = BTreeSet::new();
            let mut acceptance = 0usize;
            for (stage_index, stage) in case.stages.iter().enumerate() {
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
                    stage.deadline_secs != Some(0),
                    "eval definition {id} case {case_id} stage {stage_id} deadline_secs must be positive"
                );
                ensure!(
                    stage.seed.is_some() != !stage.prompt.trim().is_empty(),
                    "eval definition {id} case {case_id} stage {stage_id} needs exactly one of seed or prompt"
                );
                ensure!(
                    !stage.review_previous || (stage_index > 0 && stage.seed.is_none()
                        && !case.stages[stage_index - 1].checks.is_empty()),
                    "eval definition {id} case {case_id} stage {stage_id} review_previous requires a prompt and a preceding checked stage"
                );
                if let Some(seed) = &stage.seed {
                    ensure!(
                        !seed.collection.trim().is_empty(),
                        "eval definition {id} case {case_id} stage {stage_id} seed collection must be named"
                    );
                }
                if let Some(continuation) = &stage.continuation {
                    ensure!(
                        stage.settle,
                        "eval definition {id} case {case_id} stage {stage_id} continuation requires settle"
                    );
                    ensure!(
                        !continuation.prompt.trim().is_empty() && continuation.max >= 1,
                        "eval definition {id} case {case_id} stage {stage_id} continuation needs a prompt and a max of at least 1"
                    );
                    ensure!(
                        stage.capture.iter().any(|capture| matches!(
                            capture,
                            EvalCapture::Documents { name, .. } if name == &continuation.until
                        )),
                        "eval definition {id} case {case_id} stage {stage_id} continuation until names no documents capture {:?}",
                        continuation.until
                    );
                }
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
                        EvalCapture::Schema { collections, .. } => {
                            ensure!(!collections.is_empty(), "schema capture {name} needs collection names");
                            let mut names = BTreeSet::new();
                            for collection in collections {
                                crate::graphql::validate_collection_identifier(collection)?;
                                ensure!(names.insert(collection), "schema capture {name} repeats {collection}");
                            }
                        },
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

fn validate_fixture_files(fixtures: Option<&EvalFixtures>, owner: &str) -> Result<()> {
    for file in fixtures.map_or(&[][..], |fixtures| &fixtures.files) {
        let path = &file.path;
        let path_ref = std::path::Path::new(path);
        ensure!(
            !path.is_empty()
                && path_ref.is_relative()
                && path_ref
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_))),
            "{owner} fixture file path {path:?} must be a relative path inside the trial workspace"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition() -> serde_json::Value {
        json!({
            "definition_id": "monitor-findings",
            "node_did": "did:key:owner",
            "comparability_version": 1,
            "subject": {"kind": "agent", "inference_slots": ["primary"]},
            "cases": [{
                "case_id": "disk-warning",
                "split": "validation",
                "stages": [{
                    "stage_id": "check",
                    "prompt": "Run the monitor.",
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

    /// `mint_recreate_identity` stamps a fresh `updated_at` so reinstalling a
    /// byte-identical pack gets a new docID; that nonce must never reach the
    /// identity digest.
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
        let pack: crate::documents::PackConfig = serde_json::from_value(json!({
            "node": {"node_did": "did:key:owner"},
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
