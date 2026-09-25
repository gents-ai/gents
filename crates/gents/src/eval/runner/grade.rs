//! Grading: one [`VerdictRow`] per (stage, check) of a case.
//!
//! Grading is pure and reads only the case and the trial's evidence. A stage
//! the evidence never reached, and a stage that failed, still produce a row
//! for every check the case declared, so a case's denominator does not depend
//! on how far the trial got.
//!
//! Evidence that holds no stage at all is the one case where absence is not a
//! skipped prerequisite: nothing ran, so nothing about the subject was
//! observed. Those rows are [`OutcomeKind::Infrastructure`], which scores the
//! case-trial `NotEvidence` and agrees with
//! [`crate::eval::runner::completion_is_not_evidence`], the judgement the loop
//! plans from.

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
    // A trial that observed nothing did not skip its prerequisites: it never
    // ran. Every row it produces is about the infrastructure, not the subject.
    let observed_nothing = evidence.stages.is_empty();
    let mut rows = Vec::new();
    for stage in &case.stages {
        let observed = evidence
            .stages
            .iter()
            .find(|candidate| candidate.stage_id == stage.stage_id);
        for check in &stage.checks {
            rows.push(match observed {
                None if observed_nothing => synthetic(
                    stage,
                    check,
                    OutcomeKind::Infrastructure,
                    None,
                    None,
                    "no_evidence",
                ),
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
///
/// A `failure_kind` that does not classify as a failure is malformed evidence:
/// an executor that reports one is claiming a stage both failed and passed.
/// Read literally the row would carry no score and
/// [`crate::eval::scoring`] would floor a passing class at full credit, so it
/// is recorded as [`OutcomeKind::Unknown`] instead of as free marks.
fn failed(
    stage: &EvalStage,
    check: &EvalCheckRef,
    kind: OutcomeKind,
    provider_reason: Option<ProviderReason>,
) -> VerdictRow {
    if classify(kind, provider_reason) == EvidenceClass::Pass {
        return synthetic(
            stage,
            check,
            OutcomeKind::Unknown,
            None,
            None,
            "malformed_failure_kind",
        );
    }
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
    use crate::eval::runner::{completion, completion_is_not_evidence};
    use crate::eval::{
        case_trial_score, classify, CaseTrialScore, EvidenceClass, OutcomeKind, ProviderReason,
        VerdictView,
    };

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

    /// The graded rows as scoring reads them. One check per stage in these
    /// fixtures, so a row's position is its stage's position.
    fn views(rows: &[VerdictRow]) -> Vec<VerdictView> {
        rows.iter()
            .enumerate()
            .map(|(index, row)| VerdictView {
                verdict_id: index.to_string(),
                stage_index: index,
                check: row.check.clone(),
                tier: row.tier,
                kind: row.kind,
                provider_reason: row.provider_reason,
                score_bp: row.score_bp,
                weight: row.weight,
                regrade_of: None,
            })
            .collect()
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

    /// A trial that observed nothing is not a subject that failed to reach its
    /// stages: scoring must exclude it, and planning must read the same trial
    /// the same way, or a slot that learned nothing would be scored zero and
    /// never retried.
    #[test]
    fn a_trial_with_no_evidence_is_infrastructure_and_neither_scores_nor_counts() {
        let case = case(&[
            (
                "s1",
                &[("captured_rows_count", json!({"name":"items","min":1}))],
            ),
            (
                "s2",
                &[("captured_rows_count", json!({"name":"items","min":1}))],
            ),
        ]);
        let evidence = ScriptedExecutor::not_evidence("did:x");
        let rows = grade(&case, &evidence, &CheckRegistry::builtin());

        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(
                (row.kind, row.score_bp, row.check_version.as_str()),
                (OutcomeKind::Infrastructure, None, "0"),
                "{row:?}"
            );
            assert_eq!(row.raw["reason_code"], "no_evidence");
            assert_eq!(row.feedback, None);
        }
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &views(&rows)),
            CaseTrialScore::NotEvidence
        );
        // The same trial, read the way the loop plans from it. Asserted here so
        // scoring and planning can never disagree about one trial again.
        assert!(completion_is_not_evidence(&completion(&case, &evidence)));
    }

    /// An executor that reports a non-failure kind as a stage's `failure_kind`
    /// is claiming the stage both failed and passed. Scoring floors a passing
    /// class with no score at full credit, so the row must not read as a pass.
    #[test]
    fn a_failure_kind_that_does_not_classify_as_a_failure_is_malformed() {
        let evidence = ScriptedExecutor::failed_evidence("did:x", "s1", OutcomeKind::Passed, None);
        let rows = grade(
            &case(&[(
                "s1",
                &[("captured_rows_count", json!({"name":"items","min":1}))],
            )]),
            &evidence,
            &CheckRegistry::builtin(),
        );
        assert_eq!(
            (rows[0].kind, rows[0].score_bp, rows[0].provider_reason),
            (OutcomeKind::Unknown, None, None)
        );
        assert_eq!(rows[0].raw["reason_code"], "malformed_failure_kind");
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
