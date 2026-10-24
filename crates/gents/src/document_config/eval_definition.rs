use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::error::DocumentConfigError;

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
    pub deadline_secs: u64,
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
    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        let id = &self.definition_id;
        if id.trim().is_empty() {
            return Err(DocumentConfigError::EvalDefinitionIdBlank);
        }
        if self.comparability_version < 1 {
            return Err(DocumentConfigError::EvalComparabilityVersion { id: id.clone() });
        }
        if self.cases.is_empty() {
            return Err(DocumentConfigError::EvalNoCases { id: id.clone() });
        }
        validate_fixture_files(self.fixtures.as_ref(), &format!("eval definition {id}"))?;
        let mut case_ids = BTreeSet::new();
        for case in &self.cases {
            let case_id = &case.case_id;
            if case_id.trim().is_empty() {
                return Err(DocumentConfigError::EvalCaseIdBlank { id: id.clone() });
            }
            if !case_ids.insert(case_id) {
                return Err(DocumentConfigError::EvalDuplicateCase {
                    id: id.clone(),
                    case_id: case_id.clone(),
                });
            }
            validate_fixture_files(
                case.fixtures.as_ref(),
                &format!("eval definition {id} case {case_id}"),
            )?;
            if case.stages.is_empty() {
                return Err(DocumentConfigError::EvalCaseNoStages {
                    id: id.clone(),
                    case_id: case_id.clone(),
                });
            }
            if !case
                .goal
                .keys()
                .all(|collection| !collection.trim().is_empty())
            {
                return Err(DocumentConfigError::EvalGoalCollectionBlank {
                    id: id.clone(),
                    case_id: case_id.clone(),
                });
            }
            let mut stage_ids = BTreeSet::new();
            let mut acceptance = 0usize;
            for (stage_index, stage) in case.stages.iter().enumerate() {
                let stage_id = &stage.stage_id;
                if stage_id.trim().is_empty() {
                    return Err(DocumentConfigError::EvalStageIdBlank {
                        id: id.clone(),
                        case_id: case_id.clone(),
                    });
                }
                let stage_error = |build: fn(String, String, String) -> DocumentConfigError| {
                    build(id.clone(), case_id.clone(), stage_id.clone())
                };
                if !stage_ids.insert(stage_id) {
                    return Err(DocumentConfigError::EvalDuplicateStage {
                        id: id.clone(),
                        case_id: case_id.clone(),
                        stage_id: stage_id.clone(),
                    });
                }
                if !(stage.deadline_secs > 0) {
                    return Err(stage_error(|id, case_id, stage_id| {
                        DocumentConfigError::EvalStageDeadline {
                            id,
                            case_id,
                            stage_id,
                        }
                    }));
                }
                if stage.seed.is_some() == !stage.prompt.trim().is_empty() {
                    return Err(stage_error(|id, case_id, stage_id| {
                        DocumentConfigError::EvalStageSeedOrPrompt {
                            id,
                            case_id,
                            stage_id,
                        }
                    }));
                }
                if stage.review_previous
                    && !(stage_index > 0
                        && stage.seed.is_none()
                        && !case.stages[stage_index - 1].checks.is_empty())
                {
                    return Err(stage_error(|id, case_id, stage_id| {
                        DocumentConfigError::EvalReviewPrevious {
                            id,
                            case_id,
                            stage_id,
                        }
                    }));
                }
                if let Some(seed) = &stage.seed {
                    if seed.collection.trim().is_empty() {
                        return Err(stage_error(|id, case_id, stage_id| {
                            DocumentConfigError::EvalSeedCollection {
                                id,
                                case_id,
                                stage_id,
                            }
                        }));
                    }
                }
                if let Some(continuation) = &stage.continuation {
                    if !stage.settle {
                        return Err(stage_error(|id, case_id, stage_id| {
                            DocumentConfigError::EvalContinuationRequiresSettle {
                                id,
                                case_id,
                                stage_id,
                            }
                        }));
                    }
                    if continuation.prompt.trim().is_empty() || continuation.max < 1 {
                        return Err(stage_error(|id, case_id, stage_id| {
                            DocumentConfigError::EvalContinuationInvalid {
                                id,
                                case_id,
                                stage_id,
                            }
                        }));
                    }
                    if !stage.capture.iter().any(|capture| {
                        matches!(
                            capture,
                            EvalCapture::Documents { name, .. } if name == &continuation.until
                        )
                    }) {
                        return Err(DocumentConfigError::EvalContinuationUntil {
                            id: id.clone(),
                            case_id: case_id.clone(),
                            stage_id: stage_id.clone(),
                            until: continuation.until.clone(),
                        });
                    }
                }
                let mut check_names = BTreeSet::new();
                for check in &stage.checks {
                    if check.check.trim().is_empty() {
                        return Err(stage_error(|id, case_id, stage_id| {
                            DocumentConfigError::EvalCheckNameBlank {
                                id,
                                case_id,
                                stage_id,
                            }
                        }));
                    }
                    if !check_names.insert(check.check.as_str()) {
                        return Err(DocumentConfigError::EvalDuplicateCheck {
                            id: id.clone(),
                            case_id: case_id.clone(),
                            stage_id: stage_id.clone(),
                            check: check.check.clone(),
                        });
                    }
                    if !(check.weight >= 1) {
                        return Err(DocumentConfigError::EvalCheckWeight {
                            id: id.clone(),
                            case_id: case_id.clone(),
                            check: check.check.clone(),
                        });
                    }
                    if check.tier == EvalTier::Acceptance {
                        if check.check == LLM_JUDGE_CHECK {
                            return Err(DocumentConfigError::EvalJudgeAcceptance {
                                id: id.clone(),
                                case_id: case_id.clone(),
                            });
                        }
                        acceptance += 1;
                    }
                }
                let mut capture_names = BTreeSet::new();
                for capture in &stage.capture {
                    let name = capture.name();
                    if name.trim().is_empty() {
                        return Err(stage_error(|id, case_id, stage_id| {
                            DocumentConfigError::EvalCaptureNameBlank {
                                id,
                                case_id,
                                stage_id,
                            }
                        }));
                    }
                    if !capture_names.insert(name) {
                        return Err(DocumentConfigError::EvalDuplicateCapture {
                            id: id.clone(),
                            case_id: case_id.clone(),
                            stage_id: stage_id.clone(),
                            name: name.to_owned(),
                        });
                    }
                    match capture {
                        EvalCapture::Schema { collections, .. } => {
                            if collections.is_empty() {
                                return Err(DocumentConfigError::EvalSchemaCaptureEmpty {
                                    name: name.to_owned(),
                                });
                            }
                            let mut names = BTreeSet::new();
                            for collection in collections {
                                crate::graphql::validate_collection_identifier(collection)?;
                                if !names.insert(collection) {
                                    return Err(DocumentConfigError::EvalSchemaCaptureRepeats {
                                        name: name.to_owned(),
                                        collection: collection.clone(),
                                    });
                                }
                            }
                        }
                        EvalCapture::Documents { collection, .. } => {
                            if collection.trim().is_empty() {
                                return Err(DocumentConfigError::EvalDocumentsCaptureBlank {
                                    id: id.clone(),
                                    case_id: case_id.clone(),
                                    stage_id: stage_id.clone(),
                                    name: name.to_owned(),
                                });
                            }
                        }
                        EvalCapture::File { glob, .. } => {
                            if glob.trim().is_empty() {
                                return Err(DocumentConfigError::EvalFileCaptureBlank {
                                    id: id.clone(),
                                    case_id: case_id.clone(),
                                    stage_id: stage_id.clone(),
                                    name: name.to_owned(),
                                });
                            }
                        }
                    }
                }
            }
            if acceptance == 0 {
                return Err(DocumentConfigError::EvalAcceptanceRequired {
                    id: id.clone(),
                    case_id: case_id.clone(),
                });
            }
        }
        Ok(())
    }
}

fn validate_fixture_files(
    fixtures: Option<&EvalFixtures>,
    owner: &str,
) -> Result<(), DocumentConfigError> {
    for file in fixtures.map_or(&[][..], |fixtures| &fixtures.files) {
        let path = &file.path;
        let path_ref = std::path::Path::new(path);
        if path.is_empty()
            || !path_ref.is_relative()
            || !path_ref
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(DocumentConfigError::EvalFixturePath {
                owner: owner.to_owned(),
                path: path.clone(),
            });
        }
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
        let pack: crate::document_config::PackConfig = serde_json::from_value(json!({
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
