//! Turning verdicts into scores: the reducers, the case class, the paired
//! evidence between two cells, and split exposure.
//!
//! Every function here is pure and every number is an integer basis point in
//! `0..=`[`SCORE_BP_MAX`]; no float ever enters a score. Only acceptance-tier
//! verdicts count, the case is the clustering unit, and a trial that is not
//! evidence of the subject's behavior is excluded rather than counted as a
//! failure. [`super::outcome::classify`] owns which outcome is which.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use super::outcome::{classify, EvidenceClass, OutcomeKind, ProviderReason};
use crate::document_config::{EvalReducer, EvalSplit, EvalTier};

/// A perfect score. Basis points, so a score is an exact integer.
pub const SCORE_BP_MAX: u32 = 10_000;

/// One verdict row as scoring sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerdictView {
    pub verdict_id: String,
    /// Position of the verdict's stage in the case, for `LastStage`.
    pub stage_index: usize,
    pub check: String,
    pub tier: EvalTier,
    pub kind: OutcomeKind,
    pub provider_reason: Option<ProviderReason>,
    pub score_bp: Option<u32>,
    /// Zero disables the check: it enters neither a reducer nor the case class.
    pub weight: u32,
    pub regrade_of: Option<String>,
}

/// The one verdict per `(stage_index, check)` that consumers should read.
///
/// Drops every verdict that another verdict names in `regrade_of`. The rows stay
/// in the database; this only selects what consumers read.
///
/// Supersession alone is not total: two regrades of the same verdict, or two
/// unrelated rows for one check, both survive it. The tie-break is the
/// lexicographic maximum `verdict_id`, the strongest total order this view
/// offers, so the survivor never depends on the order rows arrived in. (If a
/// creation timestamp is ever added to this view it takes precedence, with
/// `verdict_id` breaking ties within one instant.) The result is ordered by
/// `(stage_index, check)`.
///
/// Precondition: every verdict passed in belongs to one case-trial. [`VerdictView`]
/// carries no trial id, so the key is `(stage_index, check)` alone and mixing two
/// trials' verdicts merges them into one row per key instead of failing. Callers
/// select by trial before they get here.
pub fn latest_verdicts(verdicts: Vec<VerdictView>) -> Vec<VerdictView> {
    let superseded: BTreeSet<String> = verdicts
        .iter()
        .filter_map(|verdict| verdict.regrade_of.clone())
        .collect();
    let mut latest: BTreeMap<(usize, String), VerdictView> = BTreeMap::new();
    for verdict in verdicts
        .into_iter()
        .filter(|verdict| !superseded.contains(&verdict.verdict_id))
    {
        match latest.entry((verdict.stage_index, verdict.check.clone())) {
            Entry::Vacant(slot) => {
                slot.insert(verdict);
            }
            Entry::Occupied(mut slot) => {
                if verdict.verdict_id > slot.get().verdict_id {
                    slot.insert(verdict);
                }
            }
        }
    }
    latest.into_values().collect()
}

/// What one case-trial is worth: a score, or nothing to score.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseTrialScore {
    /// Something outside the subject went wrong; the trial is excluded.
    NotEvidence,
    /// The trial ran but its outcome is not attributable either way.
    Unknown,
    /// Basis points in `0..=SCORE_BP_MAX`.
    Scored(u32),
}

/// What one verdict contributes to a mean, clamped into the basis-point range.
///
/// An acceptance check is expected to emit a `score_bp`; the defaults here exist
/// only for a binary check that reports nothing beyond its outcome. A pass with
/// no score is that check's full credit, and a fail kind with no score (the
/// stage never ran) is zero. They are a floor under malformed rows, not a
/// licence to omit the number: a graded check that forgets `score_bp` would
/// otherwise silently read as perfect.
fn verdict_bp(verdict: &VerdictView) -> u32 {
    match classify(verdict.kind, verdict.provider_reason) {
        EvidenceClass::Pass => verdict.score_bp.unwrap_or(SCORE_BP_MAX).min(SCORE_BP_MAX),
        _ => verdict.score_bp.unwrap_or(0).min(SCORE_BP_MAX),
    }
}

/// The weight-weighted mean of `items`, in basis points, or `None` when the
/// items carry no weight at all.
///
/// Rounding: integer division, truncating toward zero. With non-negative basis
/// points that is a floor, so the mean never rounds up and two runs of the same
/// verdicts always produce the same number. A zero-weight verdict contributes
/// nothing to either side of the division; if that leaves no weight there is
/// nothing to average, and the caller turns the `None` into `Unknown` rather
/// than into a score of zero.
fn weighted_mean_bp(items: &[&VerdictView]) -> Option<u32> {
    let weight: u64 = items.iter().map(|verdict| u64::from(verdict.weight)).sum();
    if weight == 0 {
        return None;
    }
    let total: u64 = items
        .iter()
        .map(|verdict| u64::from(verdict_bp(verdict)) * u64::from(verdict.weight))
        .sum();
    Some((total / weight) as u32)
}

/// `Eval.caseClass`, then the declared reducer over the acceptance-tier verdicts
/// that carry weight.
///
/// A zero weight disables a check completely: it enters no reducer and it does
/// not contribute its evidence class, so a disabled infrastructure check cannot
/// make the case-trial `NotEvidence`. Development-tier verdicts are excluded the
/// same way. A case-trial left with no weighted acceptance verdict — none were
/// emitted, or every one is disabled — is `Unknown`: totality was violated, and
/// that is never a score of zero.
pub fn case_trial_score(reducer: EvalReducer, verdicts: &[VerdictView]) -> CaseTrialScore {
    let acceptance: Vec<&VerdictView> = verdicts
        .iter()
        .filter(|verdict| verdict.tier == EvalTier::Acceptance && verdict.weight > 0)
        .collect();
    if acceptance.is_empty() {
        return CaseTrialScore::Unknown;
    }
    let classes: Vec<EvidenceClass> = acceptance
        .iter()
        .map(|verdict| classify(verdict.kind, verdict.provider_reason))
        .collect();
    if classes.contains(&EvidenceClass::NotEvidence) {
        return CaseTrialScore::NotEvidence;
    }
    if classes.contains(&EvidenceClass::Unknown) {
        return CaseTrialScore::Unknown;
    }
    // Only `Pass` and `Fail` remain, so the reducers decide on classes, never on
    // a check's own number: `all` asks whether every check passed.
    let reduced = match reducer {
        EvalReducer::WeightedMean => weighted_mean_bp(&acceptance),
        EvalReducer::All => Some(
            if classes.iter().all(|class| *class == EvidenceClass::Pass) {
                SCORE_BP_MAX
            } else {
                0
            },
        ),
        // `LastStage` takes the maximum `stage_index` among the weighted
        // acceptance verdicts, not the case's declared final stage. Zero-weighting
        // every check of the final stage removes it from this set and promotes an
        // earlier stage to "last".
        EvalReducer::LastStage => {
            let last = acceptance
                .iter()
                .map(|verdict| verdict.stage_index)
                .max()
                .unwrap_or(0);
            let tail: Vec<&VerdictView> = acceptance
                .iter()
                .copied()
                .filter(|verdict| verdict.stage_index == last)
                .collect();
            weighted_mean_bp(&tail)
        }
    };
    match reduced {
        Some(bp) => CaseTrialScore::Scored(bp),
        None => CaseTrialScore::Unknown,
    }
}

/// One case-trial's score, keyed by the pairing key `(case_id, trial_index)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialScore {
    pub case_id: String,
    pub trial_index: u32,
    pub score: CaseTrialScore,
}

/// One `(case, trial_index)` key that both cells produced evidence for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub case_id: String,
    pub trial_index: u32,
    pub baseline_bp: u32,
    pub candidate_bp: u32,
}

/// The kept pairs and the audit trail of what the denominator policy removed.
///
/// The denominator is `pairs.len()`, and the keys it was drawn from are `keys`;
/// every kept pair carries its own `(case_id, trial_index)` so a consumer can
/// audit exactly which pairs counted. The two counters below are per-cell rates
/// for a health gate, not parts of a partition: when neither side is evidence a
/// key increments both. Never sum them, and never subtract either from `keys` —
/// use [`PairedEvidence::dropped_keys`] for how many keys were lost.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairedEvidence {
    pub pairs: Vec<Pair>,
    /// Distinct `(case, trial_index)` keys seen in either cell.
    pub keys: usize,
    /// Keys lost because the baseline side was not evidence or was missing.
    pub dropped_baseline: usize,
    /// Keys lost because the candidate side was not evidence or was missing.
    pub dropped_candidate: usize,
}

impl PairedEvidence {
    /// How many keys produced no pair. Exactly `keys - pairs.len()`, which the
    /// overlapping per-cell counters cannot tell you.
    pub fn dropped_keys(&self) -> usize {
        self.keys.saturating_sub(self.pairs.len())
    }
}

/// `exclude_not_evidence_v1`. A pair is `(case, trial_index)`, joined by the
/// shared seed. It counts only when both sides are evidence. An unknown is
/// imputed worst-case: it fails the candidate and passes the baseline.
///
/// Precondition: each cell holds at most one score per `(case_id, trial_index)`,
/// the latest completed attempt. A cell that nevertheless repeats a key has not
/// told us which attempt counts, so that key's score in that cell is read as
/// `Unknown` and the worst-case imputation applies. The result depends only on
/// the set of rows, never on the order they arrived in.
pub fn pair_trials(baseline: &[TrialScore], candidate: &[TrialScore]) -> PairedEvidence {
    let index = |trials: &[TrialScore]| -> BTreeMap<(String, u32), CaseTrialScore> {
        let mut scores: BTreeMap<(String, u32), CaseTrialScore> = BTreeMap::new();
        for trial in trials {
            match scores.entry((trial.case_id.clone(), trial.trial_index)) {
                Entry::Vacant(slot) => {
                    slot.insert(trial.score);
                }
                Entry::Occupied(mut slot) => {
                    slot.insert(CaseTrialScore::Unknown);
                }
            }
        }
        scores
    };
    let (baseline, candidate) = (index(baseline), index(candidate));
    let keys: BTreeSet<&(String, u32)> = baseline.keys().chain(candidate.keys()).collect();
    let mut paired = PairedEvidence {
        keys: keys.len(),
        ..PairedEvidence::default()
    };
    for key in keys {
        let side = |score: Option<&CaseTrialScore>, unknown_bp: u32| match score {
            Some(CaseTrialScore::Scored(bp)) => Some((*bp).min(SCORE_BP_MAX)),
            Some(CaseTrialScore::Unknown) => Some(unknown_bp),
            Some(CaseTrialScore::NotEvidence) | None => None,
        };
        let baseline_side = side(baseline.get(key), SCORE_BP_MAX);
        let candidate_side = side(candidate.get(key), 0);
        match (baseline_side, candidate_side) {
            (Some(baseline_bp), Some(candidate_bp)) => paired.pairs.push(Pair {
                case_id: key.0.clone(),
                trial_index: key.1,
                baseline_bp,
                candidate_bp,
            }),
            (baseline_side, candidate_side) => {
                if baseline_side.is_none() {
                    paired.dropped_baseline += 1;
                }
                if candidate_side.is_none() {
                    paired.dropped_candidate += 1;
                }
            }
        }
    }
    paired
}

/// Per-case mean over evidence trials, for a single-run report: an unknown
/// counts as failure, a not-evidence trial is excluded. A case with no evidence
/// trial at all has no mean, so it never enters the headline.
pub fn case_means_bp(trials: &[TrialScore]) -> BTreeMap<String, u32> {
    let mut sums: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for trial in trials {
        let bp = match trial.score {
            CaseTrialScore::Scored(bp) => bp.min(SCORE_BP_MAX),
            CaseTrialScore::Unknown => 0,
            CaseTrialScore::NotEvidence => continue,
        };
        let entry = sums.entry(trial.case_id.clone()).or_default();
        entry.0 += u64::from(bp);
        entry.1 += 1;
    }
    sums.into_iter()
        .filter(|(_, (_, count))| *count > 0)
        .map(|(case, (sum, count))| (case, (sum / count) as u32))
        .collect()
}

/// Mean over cases, equally weighted. The case is the clustering unit, so more
/// trials of one case never outvote another. Truncating division, as in
/// [`weighted_mean_bp`]; `None` when no case has evidence.
pub fn headline_bp(trials: &[TrialScore]) -> Option<u32> {
    let means = case_means_bp(trials);
    if means.is_empty() {
        return None;
    }
    let total: u64 = means.values().map(|bp| u64::from(*bp)).sum();
    Some((total / means.len() as u64) as u32)
}

/// The `purpose` of a run an author launches to try a draft definition. A
/// pilot is not evidence: it never counts as exposure, and `compare` refuses
/// it unless asked.
pub const PILOT_PURPOSE: &str = "pilot";

/// The comparability-relevant header of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunHeader {
    pub definition_id: String,
    pub comparability_version: i64,
    pub split: EvalSplit,
    pub invalidated: bool,
    pub purpose: String,
}

/// Split exposure is a count of rows, never a counter field. Pilot runs are
/// drafts of a definition and never count.
pub fn exposure(
    runs: &[RunHeader],
    definition_id: &str,
    comparability_version: i64,
    split: EvalSplit,
) -> usize {
    runs.iter()
        .filter(|run| {
            !run.invalidated
                && run.purpose != PILOT_PURPOSE
                && run.definition_id == definition_id
                && run.comparability_version == comparability_version
                && run.split == split
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::{EvalReducer, EvalSplit, EvalTier};
    use crate::eval::{OutcomeKind, ProviderReason};

    fn v(
        id: &str,
        stage: usize,
        kind: OutcomeKind,
        score: Option<u32>,
        weight: u32,
    ) -> VerdictView {
        VerdictView {
            verdict_id: id.into(),
            stage_index: stage,
            check: format!("check-{id}"),
            tier: EvalTier::Acceptance,
            kind,
            provider_reason: None,
            score_bp: score,
            weight,
            regrade_of: None,
        }
    }

    #[test]
    fn reducers() {
        let verdicts = [
            v("a", 0, OutcomeKind::Passed, Some(10_000), 1),
            v("b", 1, OutcomeKind::ModelAcceptance, Some(4_000), 3),
        ];
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &verdicts),
            CaseTrialScore::Scored(5_500)
        );
        assert_eq!(
            case_trial_score(EvalReducer::All, &verdicts),
            CaseTrialScore::Scored(0)
        );
        assert_eq!(
            case_trial_score(EvalReducer::LastStage, &verdicts),
            CaseTrialScore::Scored(4_000)
        );
        let all_pass = [v("a", 0, OutcomeKind::Passed, Some(10_000), 1)];
        assert_eq!(
            case_trial_score(EvalReducer::All, &all_pass),
            CaseTrialScore::Scored(10_000)
        );
    }

    #[test]
    fn weighted_mean_truncates_toward_zero() {
        let truncating = [
            v("a", 0, OutcomeKind::Passed, Some(10_000), 2),
            v("b", 0, OutcomeKind::Passed, Some(0), 1),
        ];
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &truncating),
            CaseTrialScore::Scored(6_666),
            "20000/3 is 6666.67; the mean never rounds up"
        );
    }

    #[test]
    fn all_decides_by_evidence_class_not_by_a_check_score() {
        let judged = [v("a", 0, OutcomeKind::ModelAcceptance, Some(10_000), 1)];
        assert_eq!(
            case_trial_score(EvalReducer::All, &judged),
            CaseTrialScore::Scored(0),
            "a failing class is a failing check whatever number it carries"
        );
        let partial = [
            v("a", 0, OutcomeKind::Passed, Some(7_000), 1),
            v("b", 0, OutcomeKind::Passed, Some(7_000), 1),
        ];
        assert_eq!(
            case_trial_score(EvalReducer::All, &partial),
            CaseTrialScore::Scored(10_000),
            "every check passed, so `all` is satisfied"
        );
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &partial),
            CaseTrialScore::Scored(7_000)
        );
    }

    #[test]
    fn a_zero_weight_check_is_disabled_for_scoring_and_for_the_class() {
        let disabled_failure = [
            v("a", 0, OutcomeKind::Passed, Some(10_000), 2),
            v("b", 0, OutcomeKind::Deadline, Some(0), 0),
        ];
        for reducer in [
            EvalReducer::WeightedMean,
            EvalReducer::All,
            EvalReducer::LastStage,
        ] {
            assert_eq!(
                case_trial_score(reducer, &disabled_failure),
                CaseTrialScore::Scored(10_000),
                "{reducer:?} counted a zero-weight check"
            );
        }
        let all_disabled = [
            v("a", 0, OutcomeKind::Passed, Some(10_000), 0),
            v("b", 0, OutcomeKind::Passed, Some(10_000), 0),
        ];
        for reducer in [
            EvalReducer::WeightedMean,
            EvalReducer::All,
            EvalReducer::LastStage,
        ] {
            assert_eq!(
                case_trial_score(reducer, &all_disabled),
                CaseTrialScore::Unknown,
                "nothing to score is never a score of zero"
            );
        }
        let disabled_outage = [
            v("a", 0, OutcomeKind::Passed, Some(10_000), 1),
            v("b", 0, OutcomeKind::Infrastructure, None, 0),
        ];
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &disabled_outage),
            CaseTrialScore::Scored(10_000),
            "a disabled check cannot make the case-trial not evidence"
        );
    }

    #[test]
    fn development_checks_never_enter_a_score() {
        let mut judge = v("j", 0, OutcomeKind::ModelAcceptance, Some(0), 100);
        judge.tier = EvalTier::Development;
        let verdicts = [v("a", 0, OutcomeKind::Passed, Some(10_000), 1), judge];
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &verdicts),
            CaseTrialScore::Scored(10_000)
        );
        let mut outage = v("i", 0, OutcomeKind::Infrastructure, None, 1);
        outage.tier = EvalTier::Development;
        let with_outage = [v("a", 0, OutcomeKind::Passed, Some(10_000), 1), outage];
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &with_outage),
            CaseTrialScore::Scored(10_000),
            "a development-tier check cannot make the case-trial not evidence either"
        );
    }

    #[test]
    fn a_case_trial_takes_the_class_of_its_worst_verdict() {
        let pass = v("a", 0, OutcomeKind::Passed, Some(10_000), 1);
        let unknown = v("b", 0, OutcomeKind::Inconclusive, None, 1);
        let mut outage = v("c", 0, OutcomeKind::Provider, None, 1);
        outage.provider_reason = Some(ProviderReason::Unavailable);
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &[pass.clone(), unknown.clone()]),
            CaseTrialScore::Unknown
        );
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &[pass, unknown, outage]),
            CaseTrialScore::NotEvidence
        );
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &[]),
            CaseTrialScore::Unknown
        );
    }

    #[test]
    fn a_fail_kind_scores_zero_even_without_a_check_score() {
        let deadline = v("d", 0, OutcomeKind::Deadline, None, 1);
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &[deadline]),
            CaseTrialScore::Scored(0)
        );
        let skipped = v("s", 0, OutcomeKind::SkippedPrerequisite, None, 1);
        assert_eq!(
            case_trial_score(EvalReducer::WeightedMean, &[skipped.clone()]),
            CaseTrialScore::Scored(0)
        );
        assert_eq!(
            case_trial_score(EvalReducer::All, &[skipped]),
            CaseTrialScore::Scored(0),
            "a skipped prerequisite is the subject's own failure"
        );
    }

    #[test]
    fn regrades_supersede_without_destroying() {
        let original = v("v1", 0, OutcomeKind::ModelAcceptance, Some(0), 1);
        let mut regrade = v("v2", 0, OutcomeKind::Passed, Some(10_000), 1);
        regrade.check = original.check.clone();
        regrade.regrade_of = Some("v1".into());
        let latest = latest_verdicts(vec![original, regrade]);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].verdict_id, "v2");
    }

    #[test]
    fn two_regrades_of_one_verdict_leave_one_survivor_whatever_the_row_order() {
        let original = v("v1", 0, OutcomeKind::ModelAcceptance, Some(0), 1);
        let regrade = |id: &str| {
            let mut regrade = v(id, 0, OutcomeKind::Passed, Some(10_000), 1);
            regrade.check = original.check.clone();
            regrade.regrade_of = Some("v1".into());
            regrade
        };
        let (v2, v3) = (regrade("v2"), regrade("v3"));
        let forward = latest_verdicts(vec![original.clone(), v2.clone(), v3.clone()]);
        let reversed = latest_verdicts(vec![v3, v2, original]);
        assert_eq!(forward.len(), 1, "one verdict per (stage, check)");
        assert_eq!(forward, reversed, "the survivor never depends on row order");
        assert_eq!(
            forward[0].verdict_id, "v3",
            "the lexicographic maximum verdict_id wins"
        );
    }

    fn t(case: &str, index: u32, score: CaseTrialScore) -> TrialScore {
        TrialScore {
            case_id: case.into(),
            trial_index: index,
            score,
        }
    }

    #[test]
    fn pairing_drops_not_evidence_and_imputes_unknown_worst_case() {
        let baseline = [
            t("a", 0, CaseTrialScore::Scored(5_000)),
            t("a", 1, CaseTrialScore::NotEvidence),
            t("b", 0, CaseTrialScore::Unknown),
        ];
        let candidate = [
            t("a", 0, CaseTrialScore::Scored(9_000)),
            t("a", 1, CaseTrialScore::Scored(9_000)),
            t("b", 0, CaseTrialScore::Unknown),
        ];
        let paired = pair_trials(&baseline, &candidate);
        assert_eq!(paired.keys, 3);
        assert_eq!(paired.dropped_baseline, 1);
        assert_eq!(paired.dropped_candidate, 0);
        assert_eq!(paired.pairs.len(), 2);
        let b = paired
            .pairs
            .iter()
            .find(|p| p.case_id == "b")
            .expect("case b is a kept pair");
        assert_eq!(
            (b.baseline_bp, b.candidate_bp),
            (10_000, 0),
            "an unknown never helps a candidate"
        );
    }

    #[test]
    fn an_unmatched_trial_is_a_dropped_pair() {
        let baseline = [t("a", 0, CaseTrialScore::Scored(5_000))];
        let paired = pair_trials(&baseline, &[]);
        assert!(paired.pairs.is_empty());
        assert_eq!(paired.dropped_candidate, 1);
        assert_eq!(paired.dropped_keys(), 1);
    }

    #[test]
    fn a_key_both_cells_lost_counts_once_as_a_dropped_key() {
        let baseline = [t("a", 0, CaseTrialScore::NotEvidence)];
        let paired = pair_trials(&baseline, &[]);
        assert_eq!(paired.keys, 1);
        assert_eq!(paired.dropped_baseline, 1);
        assert_eq!(
            paired.dropped_candidate, 1,
            "the per-cell counters overlap by design"
        );
        assert_eq!(
            paired.dropped_keys(),
            1,
            "one key was lost, not two; the counters must not be summed"
        );
    }

    #[test]
    fn disjoint_keys_pair_only_where_both_cells_have_a_trial() {
        let baseline = [
            t("a", 0, CaseTrialScore::Scored(5_000)),
            t("a", 2, CaseTrialScore::Scored(5_000)),
        ];
        let candidate = [
            t("a", 2, CaseTrialScore::Scored(9_000)),
            t("a", 1, CaseTrialScore::Scored(9_000)),
        ];
        let paired = pair_trials(&baseline, &candidate);
        assert_eq!(paired.keys, 3);
        assert_eq!(paired.pairs.len(), 1);
        assert_eq!(paired.pairs[0].trial_index, 2);
        assert_eq!(
            (paired.pairs[0].baseline_bp, paired.pairs[0].candidate_bp),
            (5_000, 9_000)
        );
        assert_eq!(
            paired.dropped_baseline, 1,
            "(a,1) is missing from the baseline"
        );
        assert_eq!(
            paired.dropped_candidate, 1,
            "(a,0) is missing from the candidate"
        );
        assert_eq!(paired.dropped_keys(), 2);
    }

    #[test]
    fn a_repeated_key_in_a_cell_is_read_as_unknown_whatever_the_row_order() {
        let baseline = [t("a", 0, CaseTrialScore::Scored(5_000))];
        let forward = pair_trials(
            &baseline,
            &[
                t("a", 0, CaseTrialScore::Scored(9_000)),
                t("a", 0, CaseTrialScore::Scored(1_000)),
            ],
        );
        let reversed = pair_trials(
            &baseline,
            &[
                t("a", 0, CaseTrialScore::Scored(1_000)),
                t("a", 0, CaseTrialScore::Scored(9_000)),
            ],
        );
        assert_eq!(forward, reversed, "duplicates must not depend on row order");
        assert_eq!(forward.keys, 1);
        assert_eq!(forward.pairs.len(), 1);
        assert_eq!(
            (forward.pairs[0].baseline_bp, forward.pairs[0].candidate_bp),
            (5_000, 0),
            "an ambiguous cell is unknown, imputed worst-case for the candidate"
        );
    }

    #[test]
    fn headline_weights_cases_equally_and_counts_unknown_as_failure() {
        let trials = [
            t("a", 0, CaseTrialScore::Scored(10_000)),
            t("a", 1, CaseTrialScore::Scored(10_000)),
            t("a", 2, CaseTrialScore::Scored(10_000)),
            t("b", 0, CaseTrialScore::Unknown),
            t("c", 0, CaseTrialScore::NotEvidence),
        ];
        assert_eq!(case_means_bp(&trials).get("a"), Some(&10_000));
        assert_eq!(case_means_bp(&trials).get("b"), Some(&0));
        assert_eq!(
            case_means_bp(&trials).get("c"),
            None,
            "no evidence, no mean"
        );
        assert_eq!(headline_bp(&trials), Some(5_000));
        assert_eq!(headline_bp(&[]), None);
    }

    #[test]
    fn exposure_counts_rows_and_skips_invalidated_runs() {
        let run = |split, invalidated| RunHeader {
            definition_id: "d".into(),
            comparability_version: 2,
            split,
            invalidated,
            purpose: "eval".into(),
        };
        let runs = [
            run(EvalSplit::Validation, false),
            run(EvalSplit::Validation, true),
            run(EvalSplit::HeldOut, false),
        ];
        assert_eq!(exposure(&runs, "d", 2, EvalSplit::Validation), 1);
        assert_eq!(exposure(&runs, "d", 1, EvalSplit::Validation), 0);
    }

    #[test]
    fn pilot_runs_are_not_exposure() {
        let run = |purpose: &str| RunHeader {
            definition_id: "d".into(),
            comparability_version: 2,
            split: EvalSplit::Validation,
            invalidated: false,
            purpose: purpose.into(),
        };
        let runs = [run("eval"), run(PILOT_PURPOSE)];
        assert_eq!(exposure(&runs, "d", 2, EvalSplit::Validation), 1);
    }
}
