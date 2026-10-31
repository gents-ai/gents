use serde_json::{json, Value};

use crate::document_config::{EvalCase, EvalCheckRef, EvalStage, EvalTier};
use crate::eval::checks::{failure_feedback, CheckRegistry};
use crate::eval::runner::executor::{StageEvidence, TrialEvidence};
use crate::eval::{classify, EvidenceClass, OutcomeKind, ProviderReason};

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

/// One row per (stage, check) of `case`, in case order; whether `feedback` may
/// be written to a verdict is the caller's decision (`documents::append_verdict`
/// refuses it off the train split).
pub fn grade(
    case: &EvalCase,
    evidence: &TrialEvidence,
    registry: &CheckRegistry,
) -> Vec<VerdictRow> {
    let observed_nothing = evidence.stages.is_empty();
    let mut rows = Vec::new();
    for stage in &case.stages {
        let observed = evidence
            .stages
            .iter()
            .find(|candidate| candidate.stage_id == stage.stage_id);
        let feedback_row = stage
            .checks
            .iter()
            .position(|check| check.tier == EvalTier::Acceptance);
        for (index, check) in stage.checks.iter().enumerate() {
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
                    Some(kind) => failed(stage, check, kind, observed, Some(index) == feedback_row),
                    None => checked(stage, check, observed, registry),
                },
            });
        }
    }
    rows
}

/// A stage that ended in `kind` must not read as a pass: a passing class with
/// no score is floored at full credit, so a `Provider` failure without a reason
/// and a `failure_kind` that classifies as passing (malformed evidence) are
/// recorded as [`OutcomeKind::Unknown`].
fn failed(
    stage: &EvalStage,
    check: &EvalCheckRef,
    kind: OutcomeKind,
    observed: &StageEvidence,
    names_failure: bool,
) -> VerdictRow {
    let provider_reason = observed.provider_reason;
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
    VerdictRow {
        feedback: names_failure.then(|| failure_feedback(observed)),
        ..synthetic(
            stage,
            check,
            kind,
            provider_reason,
            score_bp,
            "stage_failed",
        )
    }
}

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
    use crate::eval::runner::completion_is_not_evidence;
    use crate::eval::runner::projection::completion;
    use crate::eval::runner::scripted::ScriptedExecutor;
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
                    seed: None,
                    deadline_secs: None,
                    settle: false,
                    continuation: None,
                    review_previous: false,
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
            goal: Default::default(),
        }
    }

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
            (OutcomeKind::Passed, Some(10000), 2, "2")
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
    fn a_failed_stage_row_carries_feedback_naming_the_failure() {
        let ev = ScriptedExecutor::failed_evidence("did:x", "s1", OutcomeKind::Deadline, None);
        let rows = grade(
            &case(&[(
                "s1",
                &[("captured_rows_count", json!({"name":"items","min":1}))],
            )]),
            &ev,
            &CheckRegistry::builtin(),
        );
        assert_eq!(
            (rows[0].kind, rows[0].score_bp, rows[0].tier),
            (OutcomeKind::Deadline, Some(0), EvalTier::Acceptance)
        );
        assert_eq!(rows[0].raw["reason_code"], "stage_failed");
        let feedback = rows[0].feedback.as_deref().unwrap_or_default();
        assert!(feedback.contains("failure_kind deadline"), "{feedback}");
    }

    #[test]
    fn a_failed_stage_names_its_failure_on_one_acceptance_row_only() {
        let ev = ScriptedExecutor::failed_evidence("did:x", "s1", OutcomeKind::Deadline, None);
        let mut case = case(&[(
            "s1",
            &[
                ("captured_rows_count", json!({"name":"items","min":1})),
                ("captured_rows_count", json!({"name":"other","min":1})),
            ],
        )]);
        let with_feedback = |case: &EvalCase| -> Vec<bool> {
            grade(case, &ev, &CheckRegistry::builtin())
                .iter()
                .map(|row| {
                    assert_eq!(row.raw["reason_code"], "stage_failed");
                    row.feedback.is_some()
                })
                .collect()
        };
        assert_eq!(with_feedback(&case), [true, false]);
        case.stages[0].checks[0].tier = EvalTier::Development;
        assert_eq!(with_feedback(&case), [false, true]);
        case.stages[0].checks[1].tier = EvalTier::Development;
        assert_eq!(with_feedback(&case), [false, false]);
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
        assert!(completion_is_not_evidence(&completion(&case, &evidence)));
    }

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
