use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use super::outcome::{classify, EvidenceClass, OutcomeKind, ProviderReason};
use crate::documents::{EvalReducer, EvalSplit, EvalTier};

pub const SCORE_BP_MAX: u32 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerdictView {
    pub verdict_id: String,
    pub stage_index: usize,
    pub check: String,
    pub tier: EvalTier,
    pub kind: OutcomeKind,
    pub provider_reason: Option<ProviderReason>,
    pub score_bp: Option<u32>,
    pub weight: u32,
    pub regrade_of: Option<String>,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseTrialScore {
    NotEvidence,
    Unknown,
    Scored(u32),
}

fn verdict_bp(verdict: &VerdictView) -> u32 {
    match classify(verdict.kind, verdict.provider_reason) {
        EvidenceClass::Pass => verdict.score_bp.unwrap_or(SCORE_BP_MAX).min(SCORE_BP_MAX),
        _ => verdict.score_bp.unwrap_or(0).min(SCORE_BP_MAX),
    }
}

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
    let reduced = match reducer {
        EvalReducer::WeightedMean => weighted_mean_bp(&acceptance),
        EvalReducer::All => Some(
            if classes.iter().all(|class| *class == EvidenceClass::Pass) {
                SCORE_BP_MAX
            } else {
                0
            },
        ),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialScore {
    pub case_id: String,
    pub trial_index: u32,
    pub score: CaseTrialScore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub case_id: String,
    pub trial_index: u32,
    pub baseline_bp: u32,
    pub candidate_bp: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairedEvidence {
    pub pairs: Vec<Pair>,
    pub keys: usize,
    pub dropped_baseline: usize,
    pub dropped_candidate: usize,
}

impl PairedEvidence {
    pub fn dropped_keys(&self) -> usize {
        self.keys.saturating_sub(self.pairs.len())
    }
}

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

pub fn headline_bp(trials: &[TrialScore]) -> Option<u32> {
    let means = case_means_bp(trials);
    if means.is_empty() {
        return None;
    }
    let total: u64 = means.values().map(|bp| u64::from(*bp)).sum();
    Some((total / means.len() as u64) as u32)
}

pub const PILOT_PURPOSE: &str = "pilot";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunHeader {
    pub definition_id: String,
    pub comparability_version: i64,
    pub split: EvalSplit,
    pub invalidated: bool,
    pub purpose: String,
}

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
    use crate::documents::EvalTier;

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

    fn t(case: &str, index: u32, score: CaseTrialScore) -> TrialScore {
        TrialScore {
            case_id: case.into(),
            trial_index: index,
            score,
        }
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
}
