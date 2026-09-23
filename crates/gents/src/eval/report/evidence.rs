//! One run's rows, projected into paired evidence.
//!
//! Three judgements live here and nowhere else. A slot is
//! `(run_id, cell_id, case_id, trial_index)` and is read at its latest
//! *completed* attempt, because a resumed run writes a new row per attempt.
//! A verdict's `stage_id` becomes the `stage_index` its case declares, which
//! is what `case_trial_score` reduces over. And several runs are paired one at
//! a time and then concatenated, so a re-run adds pairs.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::config_client::ConfigAccess;
use crate::document_config::{EvalCase, EvalDefinition};
use crate::eval::{
    case_trial_score, latest_verdicts, load_run, load_trials, load_verdicts, pair_trials,
    PairedEvidence, TrialRecord, TrialScore, TrialUsage, VerdictRecord, VerdictView,
};

/// Everything one run wrote, read once.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRows {
    pub run_id: String,
    /// The run's frozen case list: the cases a decision over it expects.
    pub case_ids: Vec<String>,
    pub invalidated: bool,
    pub trials: Vec<TrialRecord>,
    pub verdicts: Vec<VerdictRecord>,
}

pub async fn load_run_rows(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<RunRows> {
    let record = load_run(access, owner, run_id)
        .await?
        .with_context(|| format!("no eval run {run_id:?} for {owner}"))?;
    Ok(RunRows {
        run_id: run_id.to_owned(),
        case_ids: record.origin.case_ids.clone(),
        invalidated: record.invalidated.is_some(),
        trials: load_trials(access, owner, run_id).await?,
        verdicts: load_verdicts(access, owner, run_id).await?,
    })
}

/// One trial per `(run_id, case_id, trial_index)` slot of `cell_id`: the
/// completed attempt with the highest number. A slot whose every attempt is
/// still open contributes nothing, and pairing counts it as a dropped key.
pub fn latest_attempts<'a>(trials: &'a [TrialRecord], cell_id: &str) -> Vec<&'a TrialRecord> {
    let mut latest: BTreeMap<(&str, &str, u32), &TrialRecord> = BTreeMap::new();
    for record in trials
        .iter()
        .filter(|record| record.identity.cell_id == cell_id && record.completion.is_some())
    {
        let key = (
            record.identity.run_id.as_str(),
            record.identity.case_id.as_str(),
            record.identity.trial_index,
        );
        latest
            .entry(key)
            .and_modify(|held| {
                if record.identity.attempt > held.identity.attempt {
                    *held = record;
                }
            })
            .or_insert(record);
    }
    latest.into_values().collect()
}

/// Each completed slot of `cell_id` in one run, with its case and the
/// verdicts that count for it: the latest attempt's verdicts, stage ids mapped
/// to the case's stage indices, after `latest_verdicts` supersession.
pub(crate) fn counted_slots<'a>(
    definition: &'a EvalDefinition,
    rows: &'a RunRows,
    cell_id: &str,
) -> Vec<(&'a TrialRecord, &'a EvalCase, Vec<VerdictView>)> {
    let mut by_trial: BTreeMap<&str, Vec<&VerdictRecord>> = BTreeMap::new();
    for verdict in &rows.verdicts {
        by_trial
            .entry(verdict.trial_id.as_str())
            .or_default()
            .push(verdict);
    }
    let mut slots = Vec::new();
    for record in latest_attempts(&rows.trials, cell_id) {
        let case_id = record.identity.case_id.as_str();
        let Some(case) = definition.cases.iter().find(|case| case.case_id == case_id) else {
            continue;
        };
        let indices: BTreeMap<&str, usize> = case
            .stages
            .iter()
            .enumerate()
            .map(|(index, stage)| (stage.stage_id.as_str(), index))
            .collect();
        let views: Vec<VerdictView> = by_trial
            .get(record.identity.trial_id.as_str())
            .into_iter()
            .flatten()
            .filter_map(|verdict| {
                Some(VerdictView {
                    verdict_id: verdict.verdict_id.clone(),
                    stage_index: *indices.get(verdict.stage_id.as_str())?,
                    check: verdict.check.clone(),
                    tier: verdict.tier,
                    kind: verdict.kind,
                    provider_reason: verdict.provider_reason,
                    score_bp: verdict.score_bp,
                    weight: verdict.weight,
                    regrade_of: verdict.regrade_of.clone(),
                })
            })
            .collect();
        slots.push((record, case, latest_verdicts(views)));
    }
    slots
}

/// One [`TrialScore`] per completed slot of `cell_id` in one run.
pub fn cell_trial_scores(
    definition: &EvalDefinition,
    rows: &RunRows,
    cell_id: &str,
) -> Vec<TrialScore> {
    counted_slots(definition, rows, cell_id)
        .into_iter()
        .map(|(record, case, views)| TrialScore {
            case_id: record.identity.case_id.clone(),
            trial_index: record.identity.trial_index,
            score: case_trial_score(case.reducer, &views),
        })
        .collect()
}

/// The verdict rows that count for `cell_id` in one run: those of each slot's
/// latest completed attempt that survive regrade supersession, in slot order
/// and then `(stage_index, check)` order. Rows of superseded or unfinished
/// attempts, regraded rows, and rows naming no declared stage are left out.
pub fn counted_verdicts<'a>(
    definition: &EvalDefinition,
    rows: &'a RunRows,
    cell_id: &str,
) -> Vec<&'a VerdictRecord> {
    let by_id: BTreeMap<&str, &VerdictRecord> = rows
        .verdicts
        .iter()
        .map(|verdict| (verdict.verdict_id.as_str(), verdict))
        .collect();
    counted_slots(definition, rows, cell_id)
        .into_iter()
        .flat_map(|(_, _, views)| views)
        .filter_map(|view| by_id.get(view.verdict_id.as_str()).copied())
        .collect()
}

/// One run's two cells, paired on their shared seed.
pub fn paired_evidence(
    definition: &EvalDefinition,
    rows: &RunRows,
    baseline_cell: &str,
    candidate_cell: &str,
) -> PairedEvidence {
    pair_trials(
        &cell_trial_scores(definition, rows, baseline_cell),
        &cell_trial_scores(definition, rows, candidate_cell),
    )
}

/// Several runs' pairs as one body of evidence. Pairs are appended and the
/// counters summed: a re-run adds pairs and never replaces them.
pub fn concat_paired(parts: &[PairedEvidence]) -> PairedEvidence {
    let mut total = PairedEvidence::default();
    for part in parts {
        total.pairs.extend(part.pairs.iter().cloned());
        total.keys += part.keys;
        total.dropped_baseline += part.dropped_baseline;
        total.dropped_candidate += part.dropped_candidate;
    }
    total
}

/// Tokens reported by one cell's counted trials, and how many reported none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct CellUsage {
    pub tokens: u64,
    pub trials: u64,
    pub missing: u64,
}

fn trial_tokens(usage: &TrialUsage) -> Option<u64> {
    match (usage.input_tokens, usage.output_tokens) {
        (None, None) => None,
        (input, output) => Some(input.unwrap_or(0) + output.unwrap_or(0)),
    }
}

pub fn cell_usage(rows: &RunRows, cell_id: &str) -> CellUsage {
    let mut usage = CellUsage::default();
    for record in latest_attempts(&rows.trials, cell_id) {
        let Some(completion) = &record.completion else {
            continue;
        };
        usage.trials += 1;
        match trial_tokens(&completion.usage) {
            Some(tokens) => usage.tokens += tokens,
            None => usage.missing += 1,
        }
    }
    usage
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::document_config::EvalTier;
    use crate::eval::{
        Anchor, CaseTrialScore, OutcomeKind, StageCompletion, TrialCompletion, TrialIdentity,
    };
    use serde_json::json;

    pub(crate) fn one_case_definition() -> EvalDefinition {
        serde_json::from_value(json!({
            "definition_id": "monitor-findings",
            "agent_did": "did:key:o",
            "comparability_version": 1,
            "subject": {"kind": "behavior", "inference_slots": ["primary"]},
            "cases": [{
                "case_id": "disk-warning",
                "split": "validation",
                "stages": [{
                    "stage_id": "check",
                    "prompt": "Run the monitor.",
                    "deadline_secs": 600,
                    "checks": [{"check": "captured_rows_count", "params": {"name": "findings", "min": 1}, "tier": "acceptance"}],
                }],
            }],
        }))
        .unwrap()
    }

    /// One trial slot of case `disk-warning`. `score_bp: None` leaves the
    /// attempt unfinished (null completion, no verdict).
    pub(crate) struct Slot {
        pub cell: &'static str,
        pub trial_index: u32,
        pub attempt: u32,
        pub score_bp: Option<u32>,
        pub tokens: Option<u64>,
        pub feedback: Option<&'static str>,
    }

    pub(crate) fn slot(cell: &'static str, trial_index: u32, score_bp: u32) -> Slot {
        Slot {
            cell,
            trial_index,
            attempt: 1,
            score_bp: Some(score_bp),
            tokens: Some(10),
            feedback: None,
        }
    }

    pub(crate) fn run_rows(run_id: &str, slots: &[Slot]) -> RunRows {
        let mut rows = RunRows {
            run_id: run_id.into(),
            case_ids: vec!["disk-warning".into()],
            invalidated: false,
            trials: Vec::new(),
            verdicts: Vec::new(),
        };
        for slot in slots {
            let trial_id = format!(
                "{run_id}-{}-{}-{}",
                slot.cell, slot.trial_index, slot.attempt
            );
            rows.trials.push(TrialRecord {
                identity: TrialIdentity {
                    trial_id: trial_id.clone(),
                    run_id: run_id.into(),
                    cell_id: slot.cell.into(),
                    case_id: "disk-warning".into(),
                    trial_index: slot.trial_index,
                    attempt: slot.attempt,
                    trial_agent_did: "did:key:trial".into(),
                    session_id: "session".into(),
                    seed: 1_000 + slot.trial_index as i64,
                    home_hint: None,
                },
                created_at: "2026-09-22T00:00:00Z".into(),
                completion: slot.score_bp.map(|_| TrialCompletion {
                    ended_at: "2026-09-22T00:01:00Z".into(),
                    stages: vec![StageCompletion {
                        stage_id: "check".into(),
                        request_id: None,
                        terminal_state: None,
                        failure_kind: None,
                        provider_reason: None,
                    }],
                    usage: TrialUsage {
                        input_tokens: slot.tokens,
                        output_tokens: slot.tokens,
                    },
                    anchor: Anchor {
                        terminal_states: Vec::new(),
                        requests: 1,
                        inference_calls: 1,
                    },
                    evidence_digest: None,
                }),
            });
            if let Some(score_bp) = slot.score_bp {
                rows.verdicts.push(VerdictRecord {
                    verdict_id: format!("{trial_id}-v"),
                    run_id: run_id.into(),
                    trial_id,
                    stage_id: "check".into(),
                    check: "captured_rows_count".into(),
                    check_version: "1".into(),
                    tier: EvalTier::Acceptance,
                    kind: if score_bp > 0 {
                        OutcomeKind::Passed
                    } else {
                        OutcomeKind::ModelAcceptance
                    },
                    provider_reason: None,
                    score_bp: Some(score_bp),
                    weight: 1,
                    raw: json!({"reason_code": "in_range"}),
                    feedback: slot.feedback.map(str::to_owned),
                    regrade_of: None,
                });
            }
        }
        rows
    }

    #[test]
    fn a_slot_is_read_at_its_latest_completed_attempt() {
        let mut later = slot("base", 0, 10_000);
        later.attempt = 2;
        let rows = run_rows("run", &[slot("base", 0, 0), later]);
        let scores = cell_trial_scores(&one_case_definition(), &rows, "base");
        assert_eq!(scores.len(), 1, "one slot, one score: {scores:?}");
        assert_eq!(scores[0].score, CaseTrialScore::Scored(10_000));
    }

    #[test]
    fn an_unfinished_attempt_contributes_nothing_and_falls_back_to_a_finished_one() {
        let open = Slot {
            attempt: 2,
            score_bp: None,
            ..slot("base", 0, 0)
        };
        let rows = run_rows("run", &[slot("base", 0, 10_000), open]);
        let scores = cell_trial_scores(&one_case_definition(), &rows, "base");
        assert_eq!(
            scores,
            vec![TrialScore {
                case_id: "disk-warning".into(),
                trial_index: 0,
                score: CaseTrialScore::Scored(10_000),
            }]
        );
        let only_open = run_rows(
            "run",
            &[Slot {
                score_bp: None,
                ..slot("base", 0, 0)
            }],
        );
        assert!(cell_trial_scores(&one_case_definition(), &only_open, "base").is_empty());
    }

    /// F1: slots are keyed by run as well, so the same `(case, trial_index)` in
    /// two runs is two slots, never one ambiguous one.
    #[test]
    fn the_same_slot_in_two_runs_is_two_slots() {
        let mut first = run_rows("run-a", &[slot("base", 0, 10_000)]);
        let second = run_rows("run-b", &[slot("base", 0, 0)]);
        first.trials.extend(second.trials);
        assert_eq!(latest_attempts(&first.trials, "base").len(), 2);
    }

    #[test]
    fn a_run_is_paired_on_its_shared_seed() {
        let rows = run_rows(
            "run",
            &[
                slot("base", 0, 0),
                slot("base", 1, 0),
                slot("cand", 0, 10_000),
                slot("cand", 1, 10_000),
            ],
        );
        let paired = paired_evidence(&one_case_definition(), &rows, "base", "cand");
        assert_eq!(paired.keys, 2);
        assert_eq!(paired.pairs.len(), 2);
        assert!(paired
            .pairs
            .iter()
            .all(|pair| (pair.baseline_bp, pair.candidate_bp) == (0, 10_000)));
    }

    /// F1: a re-run adds pairs; it never replaces them and never makes a
    /// shared key ambiguous.
    #[test]
    fn concatenating_two_runs_adds_their_pairs() {
        let definition = one_case_definition();
        let slots = || {
            [
                slot("base", 0, 0),
                slot("base", 1, 0),
                slot("cand", 0, 10_000),
                slot("cand", 1, 10_000),
            ]
        };
        let one = paired_evidence(&definition, &run_rows("run-a", &slots()), "base", "cand");
        let two = paired_evidence(&definition, &run_rows("run-b", &slots()), "base", "cand");
        let both = concat_paired(&[one.clone(), two]);
        assert_eq!(both.pairs.len(), 4);
        assert_eq!(both.keys, 4);
        assert_eq!((both.dropped_baseline, both.dropped_candidate), (0, 0));
        assert!(
            both.pairs.iter().all(|pair| pair.candidate_bp == 10_000),
            "no pair was imputed: {both:?}"
        );
        assert_eq!(concat_paired(&[one.clone()]), one);
    }

    #[test]
    fn counted_verdicts_are_the_latest_attempts_after_regrades() {
        let mut rows = run_rows(
            "run",
            &[
                slot("base", 0, 0),
                Slot {
                    attempt: 2,
                    ..slot("base", 0, 0)
                },
                slot("base", 1, 0),
                slot("cand", 0, 10_000),
            ],
        );
        let original = rows.verdicts[2].clone();
        rows.verdicts.push(VerdictRecord {
            verdict_id: "regrade".into(),
            score_bp: Some(10_000),
            regrade_of: Some(original.verdict_id.clone()),
            ..original
        });
        let ids: Vec<&str> = counted_verdicts(&one_case_definition(), &rows, "base")
            .into_iter()
            .map(|verdict| verdict.verdict_id.as_str())
            .collect();
        assert_eq!(ids, vec!["run-base-0-2-v", "regrade"]);
    }

    #[test]
    fn usage_counts_trials_and_the_ones_that_reported_nothing() {
        let rows = run_rows(
            "run",
            &[
                slot("base", 0, 10_000),
                Slot {
                    tokens: None,
                    ..slot("base", 1, 10_000)
                },
            ],
        );
        assert_eq!(
            cell_usage(&rows, "base"),
            CellUsage {
                tokens: 20,
                trials: 2,
                missing: 1,
            },
            "input and output are summed; a trial with neither is missing"
        );
    }
}
