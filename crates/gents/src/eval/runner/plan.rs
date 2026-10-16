//! Which trials a run still owes: planning is a pure function of the run's
//! origin and the trials already written, so a resumed run plans exactly the
//! slots that still owe an answer.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::eval::{classify, EvidenceClass, OutcomeKind, RunOrigin, TrialCompletion, TrialRecord};

/// One trial slot the run still owes.
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

/// The identity of a slot's attempt: stable across replanning, distinct per
/// attempt.
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

/// Whether a finished attempt is evidence about its subject.
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

/// How many slots finished every attempt they were allowed without learning
/// anything.
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

/// How many abandoned attempts (a null completion: the host crashed or the
/// run was cancelled mid-trial) a slot may accumulate. Abandonment is not a
/// verdict on the subject or the provider, so it spends no
/// `max_infra_retries`; this bound only stops a trial that kills its host
/// every time from looping forever (spec 4b §9).
pub const MAX_ABANDONED_ATTEMPTS: u32 = 10;

/// How many slots [`MAX_ABANDONED_ATTEMPTS`] stopped. Kept apart from
/// [`plan`], which every listing of runs calls; the loop asks once, when its
/// plan comes back empty, to say why those slots stay unanswered.
pub fn abandonment_bounded_slots(existing: &[TrialRecord]) -> u32 {
    let mut slots: BTreeMap<(&str, &str, u32), (u32, Option<(u32, bool)>)> = BTreeMap::new();
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

/// The trials `run_id` still owes, given what it has already written.
///
/// A slot is done once its latest completed attempt is evidence about the
/// subject. A slot whose latest completed attempt is not evidence, and a slot
/// whose attempts were all abandoned, are planned again at one past their
/// highest attempt, so the new row never collides with an old one. Two bounds
/// stop that: `max_infra_retries + 1` attempts that finished without
/// evidence, and [`MAX_ABANDONED_ATTEMPTS`] attempts that never finished. An
/// abandoned attempt counts only toward the second, so a cancel never costs a
/// slot its pair.
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
