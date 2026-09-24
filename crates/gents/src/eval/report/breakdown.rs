//! Compare breakdowns (spec 4b §8): which check or stage a candidate moved,
//! and one case's trials side by side. Pure functions over a `Comparison`;
//! no new serialization.
//!
//! A trial key enters a breakdown only when `pair_trials` paired it, read
//! from the comparison's own pairs. Each side is then scored per check or
//! stage from its acceptance verdicts that carry weight, each read by
//! scoring's verdict rule: a pass counts its score (full credit when it has
//! none), a fail its score (zero when it has none), and an unknown or
//! not-evidence verdict contributes nothing. A check or stage present on one
//! side only is left out.
//!
//! The breakdowns therefore do not restate the headline comparison. Pairing
//! imputes an unknown slot worst-case as a whole; a breakdown reads that
//! slot's verdicts one by one and skips the unknown ones, even those that
//! carry a score. A check can show as improved while the comparison worsens,
//! and a check's or stage's `pairs` can be fewer than the comparison's.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::Serialize;

use crate::document_config::EvalTier;
use crate::eval::outcome::{classify, EvidenceClass};
use crate::eval::report::build::SlotVerdict;
use crate::eval::report::compare::{CaseComparison, Comparison, PairedTrial};
use crate::eval::report::refused;
use crate::eval::scoring::SCORE_BP_MAX;

/// One check's paired difference, candidate minus baseline, across cases.
///
/// Read from each side's pass and fail verdicts for the check, not from the
/// slot scores the comparison pairs: an unknown slot, which pairing imputes
/// worst-case, contributes only its pass and fail verdicts. So this row can
/// improve while the comparison worsens, and its `pairs` can be fewer than
/// the comparison's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckDiff {
    pub check: String,
    /// Cases with at least one pair scoring this check on both sides.
    pub cases: u32,
    pub pairs: u64,
    /// Mean over cases of each case's mean paired difference, truncated.
    pub mean_diff_bp: Option<i64>,
    /// Cases by the sign of their mean difference.
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
}

/// One stage's paired difference, candidate minus baseline, across cases.
///
/// Read from each side's pass and fail verdicts for the stage, not from the
/// slot scores the comparison pairs: an unknown slot, which pairing imputes
/// worst-case, contributes only its pass and fail verdicts. So this row can
/// improve while the comparison worsens, and its `pairs` can be fewer than
/// the comparison's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StageDiff {
    pub stage_id: String,
    /// Cases with at least one pair scoring this stage on both sides.
    pub cases: u32,
    pub pairs: u64,
    /// Mean over cases of each case's mean paired difference, truncated.
    pub mean_diff_bp: Option<i64>,
    /// Cases by the sign of their mean difference.
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
}

/// One case of a comparison: its summary and every trial key either cell has.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseView {
    pub case_id: String,
    pub summary: CaseComparison,
    pub trials: Vec<PairedTrial>,
}

/// Per check, sorted by check.
pub fn by_check(comparison: &Comparison) -> Vec<CheckDiff> {
    tally(comparison, |verdict| verdict.check.clone())
        .into_iter()
        .map(|(check, tally)| CheckDiff {
            check,
            cases: tally.cases,
            pairs: tally.pairs,
            mean_diff_bp: tally.mean_diff_bp,
            improved: tally.improved,
            tied: tally.tied,
            worsened: tally.worsened,
        })
        .collect()
}

/// Per stage, sorted by stage id.
pub fn by_stage(comparison: &Comparison) -> Vec<StageDiff> {
    tally(comparison, |verdict| verdict.stage_id.clone())
        .into_iter()
        .map(|(stage_id, tally)| StageDiff {
            stage_id,
            cases: tally.cases,
            pairs: tally.pairs,
            mean_diff_bp: tally.mean_diff_bp,
            improved: tally.improved,
            tied: tally.tied,
            worsened: tally.worsened,
        })
        .collect()
}

/// One case side by side; refused for a case the comparison does not have.
pub fn case_view(comparison: &Comparison, case_id: &str) -> Result<CaseView> {
    let summary = comparison
        .cases
        .iter()
        .find(|case| case.case_id == case_id)
        .cloned()
        .ok_or_else(|| refused(format!("the comparison has no case {case_id:?}")))?;
    Ok(CaseView {
        case_id: case_id.to_owned(),
        summary,
        trials: comparison
            .trials
            .iter()
            .filter(|trial| trial.case_id == case_id)
            .cloned()
            .collect(),
    })
}

struct Tally {
    cases: u32,
    pairs: u64,
    mean_diff_bp: Option<i64>,
    improved: u32,
    tied: u32,
    worsened: u32,
}

/// Per group: the per-case mean paired difference, then the mean over cases.
fn tally(
    comparison: &Comparison,
    group: impl Fn(&SlotVerdict) -> String,
) -> BTreeMap<String, Tally> {
    let paired: BTreeSet<(&str, u32)> = comparison
        .pairs()
        .iter()
        .map(|pair| (pair.case_id.as_str(), pair.trial_index))
        .collect();
    // (group, case) -> (sum of candidate minus baseline, pairs)
    let mut per_case: BTreeMap<(String, String), (i64, u64)> = BTreeMap::new();
    for trial in &comparison.trials {
        if !paired.contains(&(trial.case_id.as_str(), trial.trial_index)) {
            continue;
        }
        let (Some(baseline), Some(candidate)) = (&trial.baseline, &trial.candidate) else {
            continue;
        };
        let candidate_groups = group_scores(&candidate.verdicts, &group);
        for (name, baseline_bp) in group_scores(&baseline.verdicts, &group) {
            if let Some(candidate_bp) = candidate_groups.get(&name) {
                let entry = per_case.entry((name, trial.case_id.clone())).or_default();
                entry.0 += i64::from(*candidate_bp) - i64::from(baseline_bp);
                entry.1 += 1;
            }
        }
    }
    let mut groups: BTreeMap<String, (Vec<i64>, u64)> = BTreeMap::new();
    for ((name, _case), (sum, pairs)) in per_case {
        let entry = groups.entry(name).or_default();
        // `pairs` is at least one: an entry exists only once a pair added to it.
        entry.0.push(sum / pairs as i64);
        entry.1 += pairs;
    }
    groups
        .into_iter()
        .map(|(name, (means, pairs))| {
            let count =
                |keep: fn(i64) -> bool| means.iter().filter(|mean| keep(**mean)).count() as u32;
            let cases = means.len() as i64;
            let tally = Tally {
                cases: means.len() as u32,
                pairs,
                mean_diff_bp: (cases > 0).then(|| means.iter().sum::<i64>() / cases),
                improved: count(|mean| mean > 0),
                tied: count(|mean| mean == 0),
                worsened: count(|mean| mean < 0),
            };
            (name, tally)
        })
        .collect()
}

/// What one verdict contributes, by scoring's verdict rule: a pass its score
/// (full credit when it has none), a fail its score (zero when it has none),
/// clamped into the basis-point range. `None` for an unknown or not-evidence
/// verdict, which a breakdown skips. Mirrors the frozen scoring's private
/// per-verdict helper; a test pins the two together.
fn verdict_bp(verdict: &SlotVerdict) -> Option<u32> {
    match classify(verdict.kind, verdict.provider_reason) {
        EvidenceClass::Pass => Some(verdict.score_bp.unwrap_or(SCORE_BP_MAX).min(SCORE_BP_MAX)),
        EvidenceClass::Fail => Some(verdict.score_bp.unwrap_or(0).min(SCORE_BP_MAX)),
        EvidenceClass::Unknown | EvidenceClass::NotEvidence => None,
    }
}

/// Per group, the weight-weighted mean of the pass and fail acceptance
/// verdicts that carry weight, truncated.
fn group_scores(
    verdicts: &[SlotVerdict],
    group: &impl Fn(&SlotVerdict) -> String,
) -> BTreeMap<String, u32> {
    let mut sums: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for verdict in verdicts
        .iter()
        .filter(|verdict| verdict.tier == EvalTier::Acceptance && verdict.weight > 0)
    {
        let Some(score) = verdict_bp(verdict) else {
            continue;
        };
        let entry = sums.entry(group(verdict)).or_default();
        entry.0 += u64::from(score) * u64::from(verdict.weight);
        entry.1 += u64::from(verdict.weight);
    }
    sums.into_iter()
        .map(|(name, (total, weight))| (name, (total / weight) as u32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::report::build::{build, SlotClass};
    use crate::eval::report::compare::{compare, CompareOptions};
    use crate::eval::report::fixtures::{
        at, definition, fail, pass, record, trial_id, Outcome, Rows, RUN,
    };
    use crate::eval::report::report_refused;
    use crate::eval::{OutcomeKind, VerdictRecord};

    fn comparison_of(cases: &[&str], rows: &Rows) -> Comparison {
        let definition = definition(cases);
        let run = record(RUN, &definition, &["baseline", "candidate"], 1);
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        compare(
            &report,
            &report,
            "baseline",
            "candidate",
            &CompareOptions::default(),
        )
        .unwrap()
    }

    /// Case a: `captured_rows_count` improves and `second` ties at 10000.
    /// Case b: `captured_rows_count` ties.
    fn two_checks() -> Comparison {
        let rows = with_a_scored_second_check(
            Rows::default()
                .add(RUN, at("baseline", "a", 0, 1), fail())
                .add(RUN, at("candidate", "a", 0, 1), pass())
                .add(RUN, at("baseline", "b", 0, 1), pass())
                .add(RUN, at("candidate", "b", 0, 1), pass()),
        );
        comparison_of(&["a", "b"], &rows)
    }

    #[test]
    fn by_check_aggregates_the_paired_difference_per_check_across_cases() {
        assert_eq!(
            by_check(&two_checks()),
            vec![
                CheckDiff {
                    check: "captured_rows_count".into(),
                    cases: 2,
                    pairs: 2,
                    mean_diff_bp: Some(5_000),
                    improved: 1,
                    tied: 1,
                    worsened: 0,
                },
                CheckDiff {
                    check: "second".into(),
                    cases: 1,
                    pairs: 1,
                    mean_diff_bp: Some(0),
                    improved: 0,
                    tied: 1,
                    worsened: 0,
                },
            ]
        );
    }

    #[test]
    fn by_stage_compares_each_stages_weighted_mean() {
        // Case a: baseline (0 + 10000) / 2 against candidate 10000; case b ties.
        assert_eq!(
            by_stage(&two_checks()),
            vec![StageDiff {
                stage_id: "check".into(),
                cases: 2,
                pairs: 2,
                mean_diff_bp: Some(2_500),
                improved: 1,
                tied: 1,
                worsened: 0,
            }]
        );
    }

    /// Adds a scored `second` pass to both sides of case `a`, trial 0.
    fn with_a_scored_second_check(mut rows: Rows) -> Rows {
        for cell in ["baseline", "candidate"] {
            let trial = trial_id(RUN, at(cell, "a", 0, 1));
            let template = rows
                .verdicts
                .iter()
                .find(|verdict| verdict.trial_id == trial)
                .unwrap()
                .clone();
            rows.verdicts.push(VerdictRecord {
                verdict_id: format!("{trial}-second"),
                check: "second".into(),
                kind: OutcomeKind::Passed,
                score_bp: Some(10_000),
                ..template
            });
        }
        rows
    }

    #[test]
    fn a_side_that_is_not_evidence_is_left_out_as_pair_trials_leaves_it_out() {
        // The not-evidence side also passes `second` with a score, so only
        // the pairing gate keeps it out.
        let rows = with_a_scored_second_check(
            Rows::default()
                .add(
                    RUN,
                    at("baseline", "a", 0, 1),
                    Outcome::Verdict(OutcomeKind::Infrastructure, Some(10_000)),
                )
                .add(RUN, at("candidate", "a", 0, 1), pass()),
        );
        let comparison = comparison_of(&["a"], &rows);
        assert_eq!(comparison.pairs, 0);
        assert!(by_check(&comparison).is_empty());
        assert!(by_stage(&comparison).is_empty());
    }

    #[test]
    fn an_unknown_verdict_contributes_nothing_to_a_pair_pair_trials_keeps() {
        // The baseline's grader could not decide: an unknown slot that
        // `pair_trials` pairs (imputed). Its one verdict carries a score,
        // and scoring's verdict rule still skips it.
        for score_bp in [None, Some(10_000)] {
            let rows = Rows::default()
                .add(
                    RUN,
                    at("baseline", "a", 0, 1),
                    Outcome::Verdict(OutcomeKind::Grader, score_bp),
                )
                .add(RUN, at("candidate", "a", 0, 1), pass());
            let comparison = comparison_of(&["a"], &rows);
            assert_eq!((comparison.pairs, comparison.imputed), (1, 1));
            assert!(by_check(&comparison).is_empty(), "{score_bp:?}");
            assert!(by_stage(&comparison).is_empty(), "{score_bp:?}");
        }
    }

    /// A one-verdict case reads as scoring reads it: `by_check` against a
    /// full-credit candidate is `10000 - case_trial_score(WeightedMean)` for
    /// the baseline's verdict, and no row when scoring gives no score.
    #[test]
    fn a_one_verdict_case_scores_as_case_trial_score_scores_it() {
        use crate::document_config::EvalReducer;
        use crate::eval::scoring::{case_trial_score, CaseTrialScore, VerdictView};

        for (kind, score_bp) in [
            (OutcomeKind::Passed, None),
            (OutcomeKind::Passed, Some(6_000)),
            (OutcomeKind::ModelAcceptance, None),
            (OutcomeKind::ModelAcceptance, Some(3_000)),
            (OutcomeKind::Grader, Some(7_000)),
        ] {
            let rows = Rows::default()
                .add(
                    RUN,
                    at("baseline", "a", 0, 1),
                    Outcome::Verdict(kind, score_bp),
                )
                .add(RUN, at("candidate", "a", 0, 1), pass());
            let trial = trial_id(RUN, at("baseline", "a", 0, 1));
            let record = rows
                .verdicts
                .iter()
                .find(|verdict| verdict.trial_id == trial)
                .unwrap();
            let view = VerdictView {
                verdict_id: record.verdict_id.clone(),
                stage_index: 0,
                check: record.check.clone(),
                tier: record.tier,
                kind: record.kind,
                provider_reason: record.provider_reason,
                score_bp: record.score_bp,
                weight: record.weight,
                regrade_of: None,
            };
            let expected = match case_trial_score(EvalReducer::WeightedMean, &[view]) {
                CaseTrialScore::Scored(bp) => Some(10_000 - i64::from(bp)),
                CaseTrialScore::Unknown | CaseTrialScore::NotEvidence => None,
            };
            let comparison = comparison_of(&["a"], &rows);
            assert_eq!(
                by_check(&comparison)
                    .first()
                    .and_then(|row| row.mean_diff_bp),
                expected,
                "{kind:?} {score_bp:?}"
            );
        }
    }

    /// Case a: the baseline's `captured_rows_count` is undecided (no score),
    /// its `second` scores 0; the candidate fails `captured_rows_count` and
    /// passes `second`. Case b: the baseline is undecided, the candidate
    /// passes.
    #[test]
    fn a_check_can_improve_while_the_imputed_comparison_worsens() {
        let mut rows = Rows::default()
            .add(
                RUN,
                at("baseline", "a", 0, 1),
                Outcome::Verdict(OutcomeKind::Grader, None),
            )
            .add(RUN, at("candidate", "a", 0, 1), fail())
            .add(
                RUN,
                at("baseline", "b", 0, 1),
                Outcome::Verdict(OutcomeKind::Grader, None),
            )
            .add(RUN, at("candidate", "b", 0, 1), pass());
        for (cell, kind, score_bp) in [
            ("baseline", OutcomeKind::ModelAcceptance, 0),
            ("candidate", OutcomeKind::Passed, 10_000),
        ] {
            let trial = trial_id(RUN, at(cell, "a", 0, 1));
            let template = rows
                .verdicts
                .iter()
                .find(|verdict| verdict.trial_id == trial)
                .unwrap()
                .clone();
            rows.verdicts.push(VerdictRecord {
                verdict_id: format!("{trial}-second"),
                check: "second".into(),
                kind,
                score_bp: Some(score_bp),
                ..template
            });
        }
        let comparison = comparison_of(&["a", "b"], &rows);
        // Pairing imputes each undecided baseline at 10000: case a is
        // 10000 against (0 + 10000) / 2 and worsens; case b ties.
        assert_eq!(
            (
                comparison.pairs,
                comparison.imputed,
                comparison.improved,
                comparison.tied,
                comparison.worsened,
            ),
            (2, 2, 0, 1, 1)
        );
        // The breakdowns read verdict scores: `captured_rows_count` has no
        // baseline score in either pair, and `second` improved in case a.
        assert_eq!(
            by_check(&comparison),
            vec![CheckDiff {
                check: "second".into(),
                cases: 1,
                pairs: 1,
                mean_diff_bp: Some(10_000),
                improved: 1,
                tied: 0,
                worsened: 0,
            }]
        );
        // The stage compares the baseline's `second` (0) against the
        // candidate's (0 + 10000) / 2.
        assert_eq!(
            by_stage(&comparison),
            vec![StageDiff {
                stage_id: "check".into(),
                cases: 1,
                pairs: 1,
                mean_diff_bp: Some(5_000),
                improved: 1,
                tied: 0,
                worsened: 0,
            }]
        );
    }

    #[test]
    fn case_view_shows_both_sides_and_refuses_an_unknown_case() {
        let comparison = two_checks();
        let view = case_view(&comparison, "a").unwrap();
        assert_eq!(view.summary.diff_bp, Some(5_000));
        assert_eq!(view.trials.len(), 1);
        let baseline = view.trials[0].baseline.as_ref().unwrap();
        let candidate = view.trials[0].candidate.as_ref().unwrap();
        assert_eq!(
            (baseline.class, candidate.class),
            (SlotClass::Fail, SlotClass::Pass)
        );
        assert_eq!(
            baseline
                .verdicts
                .iter()
                .map(|verdict| (verdict.check.as_str(), verdict.score_bp))
                .collect::<Vec<_>>(),
            vec![("captured_rows_count", Some(0)), ("second", Some(10_000))]
        );
        let error = case_view(&comparison, "zzz").unwrap_err();
        assert_eq!(
            report_refused(&error).expect("a ReportRefused").0,
            "the comparison has no case \"zzz\""
        );
    }
}
