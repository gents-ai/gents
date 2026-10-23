use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::eval::{classify, EvidenceClass, OutcomeKind, RunOrigin, TrialCompletion, TrialRecord};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedTrial {
    pub trial_id: String,
    pub cell_id: String,
    pub cell_label: String,
    pub case_id: String,
    pub trial_index: u32,
    pub attempt: u32,
    pub seed: i64,
}

pub fn trial_id_for(
    run_id: &str,
    cell_id: &str,
    case_id: &str,
    trial_index: u32,
    attempt: u32,
) -> String {
    let material = format!("{run_id}\n{cell_id}\n{case_id}\n{trial_index}\n{attempt}");
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

pub fn completion_is_not_evidence(completion: &TrialCompletion) -> bool {
    completion.stages.is_empty()
        || completion
            .stages
            .iter()
            .filter(|stage| stage.failure_kind != Some(OutcomeKind::SkippedPrerequisite))
            .all(|stage| match stage.failure_kind {
                None => false,
                Some(kind) => classify(kind, stage.provider_reason) == EvidenceClass::NotEvidence,
            })
}

pub fn not_evidence_slots(existing: &[TrialRecord]) -> u32 {
    let mut latest: BTreeMap<(&str, &str, u32), (u32, bool)> = BTreeMap::new();
    for record in existing {
        let Some(completion) = &record.completion else {
            continue;
        };
        let key = (
            record.identity.cell_id.as_str(),
            record.identity.case_id.as_str(),
            record.identity.trial_index,
        );
        let attempt = record.identity.attempt;
        let entry = latest
            .entry(key)
            .or_insert((attempt, completion_is_not_evidence(completion)));
        if attempt >= entry.0 {
            *entry = (attempt, completion_is_not_evidence(completion));
        }
    }
    latest
        .values()
        .filter(|(_, not_evidence)| *not_evidence)
        .count() as u32
}

pub const MAX_ABANDONED_ATTEMPTS: u32 = 10;

type SlotAttempts = (u32, Option<(u32, bool)>);

pub fn abandonment_bounded_slots(existing: &[TrialRecord]) -> u32 {
    let mut slots: BTreeMap<(&str, &str, u32), SlotAttempts> = BTreeMap::new();
    for record in existing {
        let key = (
            record.identity.cell_id.as_str(),
            record.identity.case_id.as_str(),
            record.identity.trial_index,
        );
        let (abandoned, latest) = slots.entry(key).or_default();
        let attempt = record.identity.attempt;
        match &record.completion {
            None => *abandoned += 1,
            Some(completion) => {
                if latest.is_none_or(|(highest, _)| attempt >= highest) {
                    *latest = Some((attempt, !completion_is_not_evidence(completion)));
                }
            }
        }
    }
    slots
        .values()
        .filter(|(abandoned, latest)| {
            *abandoned >= MAX_ABANDONED_ATTEMPTS && !latest.is_some_and(|(_, evidence)| evidence)
        })
        .count() as u32
}

pub fn plan(
    origin: &RunOrigin,
    run_id: &str,
    existing: &[TrialRecord],
    max_infra_retries: u32,
) -> Vec<PlannedTrial> {
    let mut case_ids: Vec<&str> = origin.case_ids.iter().map(String::as_str).collect();
    case_ids.sort_unstable();

    let mut planned = Vec::new();
    for trial_index in 0..origin.trials_per_case {
        for case_id in &case_ids {
            for cell in &origin.cells {
                let attempts = existing.iter().filter(|record| {
                    record.identity.cell_id == cell.cell_id
                        && record.identity.case_id == *case_id
                        && record.identity.trial_index == trial_index
                });
                let mut highest = None;
                let mut abandoned = 0u32;
                let mut not_evidence = 0u32;
                let mut latest_completed: Option<(u32, &TrialCompletion)> = None;
                for record in attempts {
                    highest = highest.max(Some(record.identity.attempt));
                    let Some(completion) = &record.completion else {
                        abandoned += 1;
                        continue;
                    };
                    if completion_is_not_evidence(completion) {
                        not_evidence += 1;
                    }
                    if latest_completed
                        .is_none_or(|(attempt, _)| record.identity.attempt >= attempt)
                    {
                        latest_completed = Some((record.identity.attempt, completion));
                    }
                }
                if latest_completed
                    .is_some_and(|(_, completion)| !completion_is_not_evidence(completion))
                {
                    continue;
                }
                if not_evidence > max_infra_retries || abandoned >= MAX_ABANDONED_ATTEMPTS {
                    continue;
                }
                let Some(attempt) = highest.map_or(Some(1), |attempt| attempt.checked_add(1))
                else {
                    tracing::error!(
                        run_id,
                        cell_id = %cell.cell_id,
                        case_id,
                        trial_index,
                        "eval trial attempt number overflows u32; slot not planned"
                    );
                    continue;
                };
                let Some(seed) = origin.seed_base.checked_add(i64::from(trial_index)) else {
                    tracing::error!(
                        run_id,
                        cell_id = %cell.cell_id,
                        case_id,
                        trial_index,
                        seed_base = origin.seed_base,
                        "eval trial seed overflows i64; slot not planned"
                    );
                    continue;
                };
                planned.push(PlannedTrial {
                    trial_id: trial_id_for(run_id, &cell.cell_id, case_id, trial_index, attempt),
                    cell_id: cell.cell_id.clone(),
                    cell_label: cell.label.clone(),
                    case_id: (*case_id).to_string(),
                    trial_index,
                    attempt,
                    seed,
                });
            }
        }
    }
    planned
}
