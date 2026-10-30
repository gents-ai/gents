use sha2::{Digest, Sha256};

use crate::documents::{EvalDefinition, EvalTier};
use crate::eval::report::{
    cell_usage, concat_paired, counted_verdicts, paired_evidence, CellUsage, RunRows,
};
use crate::eval::VerdictRecord;
use crate::optimization::policy::{evidence_from_pairs, Evidence, TokenTotals};
use crate::optimization::proposer::CheckFeedback;

pub const BASELINE_CELL: &str = "baseline";
pub const CANDIDATE_CELL: &str = "candidate";

pub fn token_totals(runs: &[RunRows], max_missing_usage_bp: u64) -> Option<TokenTotals> {
    let (mut baseline, mut candidate) = (CellUsage::default(), CellUsage::default());
    for rows in runs {
        add(&mut baseline, cell_usage(rows, BASELINE_CELL));
        add(&mut candidate, cell_usage(rows, CANDIDATE_CELL));
    }
    totals(baseline, candidate, max_missing_usage_bp)
}

fn add(total: &mut CellUsage, one: CellUsage) {
    total.tokens += one.tokens;
    total.trials += one.trials;
    total.missing += one.missing;
}

pub fn totals(
    baseline: CellUsage,
    candidate: CellUsage,
    max_missing_usage_bp: u64,
) -> Option<TokenTotals> {
    let counted = u128::from(baseline.trials) + u128::from(candidate.trials);
    let missing = u128::from(baseline.missing) + u128::from(candidate.missing);
    if counted == 0 || missing * 10_000 > u128::from(max_missing_usage_bp) * counted {
        tracing::info!(
            counted = counted as u64,
            missing = missing as u64,
            "optimization cost gate skipped: too many trials reported no usage"
        );
        return None;
    }
    let (baseline_reported, candidate_reported) = (reported(&baseline), reported(&candidate));
    if baseline_reported == 0 || candidate_reported == 0 {
        tracing::info!(
            baseline_reported,
            candidate_reported,
            "optimization cost gate skipped: a cell has no trial that reported usage"
        );
        return None;
    }
    Some(TokenTotals {
        baseline_tokens: baseline.tokens,
        baseline_trials: baseline_reported,
        candidate_tokens: candidate.tokens,
        candidate_trials: candidate_reported,
    })
}

fn reported(usage: &CellUsage) -> u64 {
    usage.trials.saturating_sub(usage.missing)
}

pub fn decision_evidence(
    definition: &EvalDefinition,
    runs: &[RunRows],
    max_missing_usage_bp: u64,
) -> Evidence {
    let parts: Vec<_> = runs
        .iter()
        .map(|rows| paired_evidence(definition, rows, BASELINE_CELL, CANDIDATE_CELL))
        .collect();
    let expected = runs
        .first()
        .map(|rows| rows.case_ids.clone())
        .unwrap_or_default();
    evidence_from_pairs(
        &concat_paired(&parts),
        &expected,
        token_totals(runs, max_missing_usage_bp),
    )
}

pub fn train_feedback(definition: &EvalDefinition, train: &RunRows) -> Vec<CheckFeedback> {
    let mut counted: Vec<&VerdictRecord> = counted_verdicts(definition, train, BASELINE_CELL)
        .into_iter()
        .filter(|verdict| verdict.tier == EvalTier::Acceptance)
        .collect();
    counted.sort_by(|left, right| {
        (&left.check, &left.verdict_id).cmp(&(&right.check, &right.verdict_id))
    });
    counted
        .into_iter()
        .map(|verdict| CheckFeedback {
            check: verdict.check.clone(),
            score_bp: verdict.score_bp,
            feedback: verdict.feedback.clone(),
        })
        .collect()
}

pub fn decision_seed(run_ids: &[String]) -> u64 {
    let digest = Sha256::digest(run_ids.join("\n").as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::report::evidence::tests::{one_case_definition, run_rows, slot, Slot};

    fn improving(run_id: &str) -> RunRows {
        run_rows(
            run_id,
            &[
                slot(BASELINE_CELL, 0, 0),
                slot(BASELINE_CELL, 1, 0),
                slot(CANDIDATE_CELL, 0, 10_000),
                slot(CANDIDATE_CELL, 1, 10_000),
            ],
        )
    }

    #[test]
    fn the_cost_gate_is_skipped_when_too_many_trials_report_no_usage() {
        let runs = [improving("v0")];
        let totals = token_totals(&runs, 2_000).expect("every trial reported usage");
        assert_eq!((totals.baseline_tokens, totals.baseline_trials), (40, 2));
        assert_eq!((totals.candidate_tokens, totals.candidate_trials), (40, 2));

        let half = run_rows(
            "v0",
            &[
                slot(BASELINE_CELL, 0, 0),
                Slot {
                    tokens: None,
                    ..slot(CANDIDATE_CELL, 0, 10_000)
                },
            ],
        );
        assert_eq!(
            token_totals(&[half], 2_000),
            None,
            "one trial in two without usage is past a 20% tolerance"
        );
    }

    #[test]
    fn a_trial_without_usage_is_left_out_of_its_cells_mean() {
        let mut slots: Vec<Slot> = (0..5)
            .flat_map(|index| {
                [
                    slot(BASELINE_CELL, index, 0),
                    slot(CANDIDATE_CELL, index, 10_000),
                ]
            })
            .collect();
        slots[1].tokens = None;
        let rows = run_rows("v0", &slots);
        let totals = token_totals(&[rows], 2_000).expect("one in ten is under 20%");
        assert_eq!((totals.baseline_tokens, totals.baseline_trials), (100, 5));
        assert_eq!(
            (totals.candidate_tokens, totals.candidate_trials),
            (80, 4),
            "the candidate's mean is 20 per reporting trial, not 16 per trial"
        );

        let silent = run_rows(
            "v0",
            &[
                slot(BASELINE_CELL, 0, 0),
                Slot {
                    tokens: None,
                    ..slot(CANDIDATE_CELL, 0, 10_000)
                },
            ],
        );
        assert_eq!(
            token_totals(&[silent], 10_000),
            None,
            "a cell with no reporting trial has no mean"
        );
    }

    #[test]
    fn feedback_is_read_from_counted_acceptance_verdicts_only() {
        let definition = one_case_definition();
        let mut rows = run_rows(
            "train",
            &[
                Slot {
                    feedback: Some("superseded attempt"),
                    ..slot(BASELINE_CELL, 0, 0)
                },
                Slot {
                    attempt: 2,
                    feedback: Some("latest attempt"),
                    ..slot(BASELINE_CELL, 0, 0)
                },
                Slot {
                    feedback: Some("before the regrade"),
                    ..slot(BASELINE_CELL, 1, 0)
                },
            ],
        );
        let original = rows.verdicts[2].clone();
        rows.verdicts.push(VerdictRecord {
            verdict_id: format!("{}-regrade", original.verdict_id),
            score_bp: Some(10_000),
            feedback: Some("after the regrade".into()),
            regrade_of: Some(original.verdict_id.clone()),
            ..original.clone()
        });
        rows.verdicts.push(VerdictRecord {
            verdict_id: format!("{}-dev", original.verdict_id),
            check: "prose_style".into(),
            tier: EvalTier::Development,
            feedback: Some("development advice".into()),
            ..original
        });

        let feedback = train_feedback(&definition, &rows);
        let said: Vec<_> = feedback
            .iter()
            .map(|entry| entry.feedback.as_deref())
            .collect();
        assert_eq!(
            said,
            vec![Some("latest attempt"), Some("after the regrade")]
        );
        assert!(feedback
            .iter()
            .all(|entry| entry.check == "captured_rows_count"));
    }

    #[test]
    fn feedback_reaches_the_proposer_as_check_name_score_and_text_only() {
        let rows = run_rows(
            "train",
            &[
                Slot {
                    feedback: Some("name the collection"),
                    ..slot(BASELINE_CELL, 0, 0)
                },
                slot(BASELINE_CELL, 1, 10_000),
            ],
        );
        let feedback = train_feedback(&one_case_definition(), &rows);
        assert_eq!(feedback.len(), 2);
        assert_eq!(feedback[0].check, "captured_rows_count");
        assert!(feedback
            .iter()
            .any(|entry| entry.feedback.as_deref() == Some("name the collection")));
        let rendered = serde_json::to_string(&feedback).unwrap();
        for forbidden in ["reason_code", "trial", "disk-warning", "stage", "train-"] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden} leaked: {rendered}"
            );
        }
    }

    #[test]
    fn totals_skip_the_cost_gate_past_the_missing_share_and_for_a_silent_cell() {
        let usage = |tokens, trials, missing| CellUsage {
            tokens,
            trials,
            missing,
        };
        assert_eq!(
            totals(usage(100, 10, 0), usage(120, 10, 0), 2000),
            Some(TokenTotals {
                baseline_tokens: 100,
                baseline_trials: 10,
                candidate_tokens: 120,
                candidate_trials: 10,
            })
        );
        assert_eq!(
            totals(usage(100, 10, 3), usage(120, 10, 2), 2000),
            None,
            "5 of 20 trials missing is past 2000 bp"
        );
        assert_eq!(
            totals(usage(0, 2, 2), usage(120, 10, 0), 5000),
            None,
            "a cell whose every trial is unmetered has no mean"
        );
        assert_eq!(totals(usage(0, 0, 0), usage(0, 0, 0), 2000), None);
    }
}
