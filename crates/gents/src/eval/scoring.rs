use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use super::outcome::{classify, EvidenceClass, OutcomeKind, ProviderReason};
use gents_documents::{EvalReducer, EvalSplit, EvalTier};

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

    #[test]
    fn generated_latest_verdicts_are_independent_of_database_row_order() {
        use gents_lean_vocab::families::runtime_eval_selection_latest_verdicts as family;
        family::replay(|input| {
            let rows = input
                .rows
                .iter()
                .map(|row| VerdictView {
                    verdict_id: row.verdict_id.clone(),
                    stage_index: usize::try_from(row.stage_index).unwrap(),
                    check: row.check.clone(),
                    tier: serde_json::from_value(serde_json::json!(row.tier)).unwrap(),
                    kind: OutcomeKind::parse(&row.kind).unwrap(),
                    provider_reason: row
                        .provider_reason
                        .as_deref()
                        .map(|value| ProviderReason::parse(value).unwrap()),
                    score_bp: row.score_bp.map(|value| u32::try_from(value).unwrap()),
                    weight: u32::try_from(row.weight).unwrap(),
                    regrade_of: row.regrade_of.clone(),
                })
                .collect();
            latest_verdicts(rows)
                .into_iter()
                .map(|row| family::Verdict {
                    verdict_id: row.verdict_id,
                    stage_index: row.stage_index as u64,
                    check: row.check,
                    tier: serde_json::to_value(row.tier)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    kind: row.kind.as_str().to_owned(),
                    provider_reason: row.provider_reason.map(|reason| reason.as_str().to_owned()),
                    score_bp: row.score_bp.map(u64::from),
                    weight: u64::from(row.weight),
                    regrade_of: row.regrade_of,
                })
                .collect()
        });
    }

    #[test]
    fn generated_pair_trials_impute_ambiguous_cells_against_the_candidate() {
        use gents_lean_vocab::families::runtime_eval_selection_pair_trials as family;
        fn rows(rows: &[family::Trial]) -> Vec<TrialScore> {
            rows.iter()
                .map(|row| TrialScore {
                    case_id: row.case_id.clone(),
                    trial_index: u32::try_from(row.trial_index).unwrap(),
                    score: match row.classification.as_str() {
                        "scored" => CaseTrialScore::Scored(u32::try_from(row.score).unwrap()),
                        "unknown" => CaseTrialScore::Unknown,
                        "not_evidence" => CaseTrialScore::NotEvidence,
                        other => panic!("unknown modeled score classification {other}"),
                    },
                })
                .collect()
        }
        family::replay(|input| {
            let paired = pair_trials(&rows(&input.baseline), &rows(&input.candidate));
            family::Paired {
                pairs: paired
                    .pairs
                    .into_iter()
                    .map(|pair| family::Pair {
                        case_id: pair.case_id,
                        trial_index: u64::from(pair.trial_index),
                        baseline_bp: u64::from(pair.baseline_bp),
                        candidate_bp: u64::from(pair.candidate_bp),
                    })
                    .collect(),
                keys: paired.keys as u64,
                dropped_baseline: paired.dropped_baseline as u64,
                dropped_candidate: paired.dropped_candidate as u64,
            }
        });
    }
}
