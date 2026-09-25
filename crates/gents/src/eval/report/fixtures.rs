//! Hand-built eval documents for the report's unit tests: a definition of
//! one-stage cases, a frozen run over it, and trials with one acceptance
//! verdict each, built slot by slot.

use serde_json::json;

use crate::document_config::{EvalDefinition, EvalSplit, EvalTier};
use crate::eval::runner::freeze::definition_ref;
use crate::eval::{
    Anchor, CellSpec, OutcomeKind, RunOrigin, RunRecord, StageCompletion, SubjectRef,
    TrialCompletion, TrialIdentity, TrialRecord, TrialUsage, VerdictRecord, DENOMINATOR_POLICY_V1,
    TAXONOMY_VERSION,
};

pub(crate) const RUN: &str = "run";

/// One validation case per id, each one stage `check` with one acceptance
/// `captured_rows_count` check.
pub(crate) fn definition(cases: &[&str]) -> EvalDefinition {
    let cases: Vec<serde_json::Value> = cases
        .iter()
        .map(|case_id| {
            json!({
                "case_id": case_id,
                "split": "validation",
                "stages": [{
                    "stage_id": "check",
                    "prompt": "Run the monitor.",
                    "deadline_secs": 600,
                    "checks": [{
                        "check": "captured_rows_count",
                        "params": {"name": "findings", "min": 1},
                        "tier": "acceptance",
                    }],
                }],
            })
        })
        .collect();
    serde_json::from_value(json!({
        "definition_id": "report-def",
        "agent_did": "did:key:owner",
        "comparability_version": 1,
        "subject": {"kind": "behavior", "inference_slots": ["primary"]},
        "cases": cases,
    }))
    .expect("the fixture definition parses")
}

/// A run frozen over every case of `definition`, one row per cell id.
pub(crate) fn record(
    run_id: &str,
    definition: &EvalDefinition,
    cells: &[&str],
    trials_per_case: u32,
) -> RunRecord {
    let mut case_ids: Vec<String> = definition
        .cases
        .iter()
        .map(|case| case.case_id.clone())
        .collect();
    case_ids.sort();
    RunRecord {
        run_id: run_id.into(),
        owner: "did:key:owner".into(),
        evaluator_did: "did:key:owner".into(),
        origin: RunOrigin {
            definition: definition_ref(definition).expect("the fixture definition digests"),
            split: EvalSplit::Validation,
            case_ids,
            cells: cells
                .iter()
                .map(|cell_id| CellSpec {
                    cell_id: (*cell_id).into(),
                    label: (*cell_id).into(),
                    subject: SubjectRef {
                        pack_digest: "sha256:pack".into(),
                        behavior_id: "monitor".into(),
                    },
                    inference_profile_id: "local".into(),
                })
                .collect(),
            trials_per_case,
            seed_base: 1_000,
            deadline_secs: None,
            concurrency: 1,
            denominator_policy: DENOMINATOR_POLICY_V1.into(),
            taxonomy_version: TAXONOMY_VERSION.into(),
            max_infra_retries: 1,
            check_registry_version: "checks-1".into(),
            source_commit: "fixture".into(),
            source_dirty: false,
            purpose: "eval".into(),
            breaker_threshold: 5,
        },
        created_at: "2026-09-22T00:00:00Z".into(),
        invalidated: None,
    }
}

/// Where one attempt sits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct At {
    pub cell: &'static str,
    pub case: &'static str,
    pub index: u32,
    pub attempt: u32,
}

pub(crate) fn at(cell: &'static str, case: &'static str, index: u32, attempt: u32) -> At {
    At {
        cell,
        case,
        index,
        attempt,
    }
}

/// What one attempt came to.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Outcome {
    /// Launched and never completed: a null completion and no verdict.
    Open,
    /// Completed with one acceptance verdict of this kind and score.
    Verdict(OutcomeKind, Option<u32>),
}

pub(crate) fn pass() -> Outcome {
    Outcome::Verdict(OutcomeKind::Passed, Some(10_000))
}

pub(crate) fn fail() -> Outcome {
    Outcome::Verdict(OutcomeKind::ModelAcceptance, Some(0))
}

pub(crate) fn trial_id(run_id: &str, at: At) -> String {
    format!(
        "{run_id}-{}-{}-{}-{}",
        at.cell, at.case, at.index, at.attempt
    )
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Rows {
    pub trials: Vec<TrialRecord>,
    pub verdicts: Vec<VerdictRecord>,
}

impl Rows {
    /// One attempt; a completed one reports ten input and ten output tokens.
    pub(crate) fn add(mut self, run_id: &str, at: At, outcome: Outcome) -> Self {
        let trial_id = trial_id(run_id, at);
        let completion = match outcome {
            Outcome::Open => None,
            Outcome::Verdict(..) => Some(TrialCompletion {
                ended_at: "2026-09-22T00:01:00Z".into(),
                stages: vec![StageCompletion {
                    stage_id: "check".into(),
                    request_id: Some(format!("{trial_id}-request")),
                    terminal_state: None,
                    failure_kind: None,
                    provider_reason: None,
                }],
                usage: TrialUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(10),
                },
                anchor: Anchor {
                    terminal_states: Vec::new(),
                    requests: 1,
                    inference_calls: 1,
                },
                evidence_digest: Some(format!("sha256:{trial_id}")),
            }),
        };
        self.trials.push(TrialRecord {
            identity: TrialIdentity {
                trial_id: trial_id.clone(),
                run_id: run_id.into(),
                cell_id: at.cell.into(),
                case_id: at.case.into(),
                trial_index: at.index,
                attempt: at.attempt,
                trial_agent_did: "did:key:trial".into(),
                session_id: format!("session-{trial_id}"),
                seed: 1_000 + i64::from(at.index),
                home_hint: Some(format!("{run_id}/trials/{trial_id}")),
            },
            created_at: "2026-09-22T00:00:00Z".into(),
            completion,
        });
        if let Outcome::Verdict(kind, score_bp) = outcome {
            self.verdicts.push(VerdictRecord {
                verdict_id: format!("{trial_id}-v"),
                run_id: run_id.into(),
                trial_id,
                stage_id: "check".into(),
                check: "captured_rows_count".into(),
                check_version: "1".into(),
                tier: EvalTier::Acceptance,
                kind,
                provider_reason: None,
                score_bp,
                weight: 1,
                raw: json!({"reason_code": "fixture"}),
                feedback: None,
                regrade_of: None,
            });
        }
        self
    }

    /// The attempt added last reported no usage at all.
    pub(crate) fn unmetered(mut self) -> Self {
        if let Some(completion) = self
            .trials
            .last_mut()
            .and_then(|trial| trial.completion.as_mut())
        {
            completion.usage = TrialUsage::default();
        }
        self
    }
}
