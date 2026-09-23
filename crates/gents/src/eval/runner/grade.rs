//! Grading: one [`VerdictRow`] per (stage, check) of a case.
//!
//! Grading is pure and reads only the case and the trial's evidence. A stage
//! the evidence never reached, and a stage that failed, still produce a row
//! for every check the case declared, so a case's denominator does not depend
//! on how far the trial got.

use serde_json::{json, Value};

use crate::document_config::{EvalCase, EvalCheckRef, EvalStage, EvalTier};
use crate::eval::checks::CheckRegistry;
use crate::eval::runner::executor::{StageEvidence, TrialEvidence};
use crate::eval::{classify, EvidenceClass, OutcomeKind, ProviderReason};

/// One graded (stage, check) pair. `raw` is always an object carrying a
/// `reason_code`; `score_bp` is `None` when the row is not evidence about the
/// subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerdictRow {
    pub stage_id: String,
    pub check: String,
    pub check_version: String,
    pub tier: EvalTier,
    pub kind: OutcomeKind,
    pub provider_reason: Option<ProviderReason>,
    pub score_bp: Option<u32>,
    pub weight: u32,
    pub raw: Value,
    pub feedback: Option<String>,
}

/// One row per (stage, check) of `case`, in case order.
///
/// `feedback` is passed through from the check unchanged; whether it may be
/// written is the caller's decision, and `documents::append_verdict` refuses
/// it off the train split.
pub fn grade(
    case: &EvalCase,
    evidence: &TrialEvidence,
    registry: &CheckRegistry,
) -> Vec<VerdictRow> {
    let mut rows = Vec::new();
    for stage in &case.stages {
        let observed = evidence
            .stages
            .iter()
            .find(|candidate| candidate.stage_id == stage.stage_id);
        for check in &stage.checks {
            rows.push(match observed {
                None => synthetic(
                    stage,
                    check,
                    OutcomeKind::SkippedPrerequisite,
                    None,
                    Some(0),
                    "skipped_prerequisite",
                ),
                Some(observed) => match observed.failure_kind {
                    Some(kind) => failed(stage, check, kind, observed.provider_reason),
                    None => checked(stage, check, observed, registry),
                },
            });
        }
    }
    rows
}

/// A stage that ended in `kind`. A provider failure with no reason is not
/// evidence of anything: it is recorded as [`OutcomeKind::Unknown`], which is
/// how the "no provider outcome without a reason" constraint is enforced.
fn failed(
    stage: &EvalStage,
    check: &EvalCheckRef,
    kind: OutcomeKind,
    provider_reason: Option<ProviderReason>,
) -> VerdictRow {
    if kind == OutcomeKind::Provider && provider_reason.is_none() {
        return synthetic(
            stage,
            check,
            OutcomeKind::Unknown,
            None,
            None,
            "provider_without_reason",
        );
    }
    let score_bp = (classify(kind, provider_reason) == EvidenceClass::Fail).then_some(0);
    synthetic(
        stage,
        check,
        kind,
        provider_reason,
        score_bp,
        "stage_failed",
    )
}

/// A stage that ran to completion: the named check reads its evidence.
fn checked(
    stage: &EvalStage,
    check: &EvalCheckRef,
    observed: &StageEvidence,
    registry: &CheckRegistry,
) -> VerdictRow {
    let Some(implementation) = registry.get(&check.check) else {
        return synthetic(
            stage,
            check,
            OutcomeKind::Grader,
            None,
            None,
            "unknown_check",
        );
    };
    let verdict = implementation.evaluate(&check.params, observed);
    VerdictRow {
        stage_id: stage.stage_id.clone(),
        check: check.check.clone(),
        check_version: implementation.version().to_string(),
        tier: check.tier,
        kind: verdict.kind,
        provider_reason: None,
        score_bp: verdict.score_bp,
        weight: check.weight,
        raw: verdict.raw,
        feedback: verdict.feedback,
    }
}

/// A row no check produced: its version is `"0"` and it carries no feedback.
fn synthetic(
    stage: &EvalStage,
    check: &EvalCheckRef,
    kind: OutcomeKind,
    provider_reason: Option<ProviderReason>,
    score_bp: Option<u32>,
    reason_code: &str,
) -> VerdictRow {
    VerdictRow {
        stage_id: stage.stage_id.clone(),
        check: check.check.clone(),
        check_version: "0".to_string(),
        tier: check.tier,
        kind,
        provider_reason,
        score_bp,
        weight: check.weight,
        raw: json!({ "reason_code": reason_code }),
        feedback: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::{EvalReducer, EvalSplit};
    use crate::eval::runner::scripted::ScriptedExecutor;
    use crate::eval::{classify, EvidenceClass, OutcomeKind, ProviderReason};

    fn case(stages: &[(&str, &[(&str, Value)])]) -> EvalCase {
        EvalCase {
            case_id: "k".into(),
            split: EvalSplit::Train,
            reducer: EvalReducer::WeightedMean,
            fixtures: None,
            stages: stages
                .iter()
                .map(|(id, checks)| EvalStage {
                    stage_id: id.to_string(),
                    prompt: "p".into(),
                    deadline_secs: 60,
                    capture: vec![],
                    checks: checks
                        .iter()
                        .map(|(name, params)| EvalCheckRef {
                            check: name.to_string(),
                            params: params.clone(),
                            tier: EvalTier::Acceptance,
                            weight: 2,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_completed_stage_runs_its_checks_and_copies_weight_and_version() {
        let ev =
            ScriptedExecutor::passed_evidence("did:x", "s1", "items", vec![json!({}), json!({})]);
        let rows = grade(
            &case(&[(
                "s1",
                &[("captured_rows_count", json!({"name":"items","min":2}))],
            )]),
            &ev,
            &CheckRegistry::builtin(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (
                rows[0].kind,
                rows[0].score_bp,
                rows[0].weight,
                rows[0].check_version.as_str()
            ),
            (OutcomeKind::Passed, Some(10000), 2, "1")
        );
    }

    #[test]
    fn a_stage_that_never_ran_yields_skipped_prerequisite_rows_for_every_check() {
        let ev = ScriptedExecutor::failed_evidence("did:x", "s1", OutcomeKind::Deadline, None);
        let rows = grade(
            &case(&[
                (
                    "s1",
                    &[("captured_rows_count", json!({"name":"items","min":1}))],
                ),
                (
                    "s2",
                    &[
                        ("captured_rows_count", json!({"name":"items","min":1})),
                        ("captured_rows_count", json!({"name":"other","min":1})),
                    ],
                ),
            ]),
            &ev,
            &CheckRegistry::builtin(),
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, OutcomeKind::Deadline);
        assert!(rows[1..]
            .iter()
            .all(|r| r.kind == OutcomeKind::SkippedPrerequisite && r.score_bp == Some(0)));
    }

    #[test]
    fn a_provider_failure_without_a_reason_is_downgraded_to_unknown() {
        let ev = ScriptedExecutor::failed_evidence("did:x", "s1", OutcomeKind::Provider, None);
        let rows = grade(
            &case(&[(
                "s1",
                &[("captured_rows_count", json!({"name":"items","min":1}))],
            )]),
            &ev,
            &CheckRegistry::builtin(),
        );
        assert_eq!(rows[0].kind, OutcomeKind::Unknown);
        assert_eq!(rows[0].raw["reason_code"], "provider_without_reason");
        let ev = ScriptedExecutor::failed_evidence(
            "did:x",
            "s1",
            OutcomeKind::Provider,
            Some(ProviderReason::Unavailable),
        );
        let rows = grade(
            &case(&[(
                "s1",
                &[("captured_rows_count", json!({"name":"items","min":1}))],
            )]),
            &ev,
            &CheckRegistry::builtin(),
        );
        assert_eq!(
            (rows[0].kind, rows[0].provider_reason, rows[0].score_bp),
            (
                OutcomeKind::Provider,
                Some(ProviderReason::Unavailable),
                None
            )
        );
        assert_eq!(
            classify(rows[0].kind, rows[0].provider_reason),
            EvidenceClass::NotEvidence
        );
    }

    #[test]
    fn an_unknown_check_name_is_a_grader_outcome_not_a_panic() {
        let ev = ScriptedExecutor::passed_evidence("did:x", "s1", "items", vec![]);
        let rows = grade(
            &case(&[("s1", &[("no_such_check", json!({}))])]),
            &ev,
            &CheckRegistry::builtin(),
        );
        assert_eq!(
            (rows[0].kind, rows[0].score_bp),
            (OutcomeKind::Grader, None)
        );
        assert_eq!(rows[0].raw["reason_code"], "unknown_check");
    }
}
