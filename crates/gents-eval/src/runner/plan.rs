use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::{
    classify, EvidenceClass, OutcomeKind, ProviderReason, RunOrigin, TrialCompletion, TrialRecord,
};

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

pub fn attempt_retryable(completion: &TrialCompletion) -> bool {
    stages_retryable(
        completion
            .stages
            .iter()
            .map(|stage| (stage.failure_kind, stage.provider_reason)),
    )
}

pub(crate) fn stages_retryable(
    stages: impl Iterator<Item = (Option<OutcomeKind>, Option<ProviderReason>)>,
) -> bool {
    let mut accepted = false;
    let mut excluded = false;
    for (failure_kind, provider_reason) in stages {
        match failure_kind {
            None => accepted = true,
            Some(OutcomeKind::SkippedPrerequisite) => {}
            Some(kind) if classify(kind, provider_reason) == EvidenceClass::NotEvidence => {
                excluded = true;
            }
            Some(_) => return false,
        }
    }
    excluded || !accepted
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SlotAttempts {
    pub(crate) latest_retryable: Option<bool>,
    pub(crate) retryable: u32,
    pub(crate) abandoned: u32,
}

pub(crate) fn slot_owes_attempt(
    max_infra_retries: u32,
    max_abandoned: u32,
    slot: SlotAttempts,
) -> bool {
    slot.latest_retryable != Some(false)
        && slot.retryable <= max_infra_retries
        && slot.abandoned < max_abandoned
}

type SlotKey<'a> = (&'a str, &'a str, u32);

#[derive(Clone, Copy, Default)]
struct SlotHistory {
    highest: Option<u32>,
    latest_completed: Option<u32>,
    attempts: SlotAttempts,
}

fn slot_histories(existing: &[TrialRecord]) -> BTreeMap<SlotKey<'_>, SlotHistory> {
    let mut slots: BTreeMap<SlotKey<'_>, SlotHistory> = BTreeMap::new();
    for record in existing {
        let key = (
            record.identity.cell_id.as_str(),
            record.identity.case_id.as_str(),
            record.identity.trial_index,
        );
        let slot = slots.entry(key).or_default();
        let attempt = record.identity.attempt;
        slot.highest = slot.highest.max(Some(attempt));
        let Some(completion) = &record.completion else {
            slot.attempts.abandoned += 1;
            continue;
        };
        let retryable = attempt_retryable(completion);
        slot.attempts.retryable += u32::from(retryable);
        if slot.latest_completed.is_none_or(|latest| attempt >= latest) {
            slot.latest_completed = Some(attempt);
            slot.attempts.latest_retryable = Some(retryable);
        }
    }
    slots
}

pub fn not_evidence_slots(existing: &[TrialRecord]) -> u32 {
    slot_histories(existing)
        .values()
        .filter(|slot| slot.attempts.latest_retryable == Some(true))
        .count() as u32
}

pub const MAX_ABANDONED_ATTEMPTS: u32 = 10;

pub fn abandonment_bounded_slots(existing: &[TrialRecord]) -> u32 {
    slot_histories(existing)
        .values()
        .filter(|slot| {
            slot.attempts.abandoned >= MAX_ABANDONED_ATTEMPTS
                && slot.attempts.latest_retryable != Some(false)
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

    let slots = slot_histories(existing);
    let mut planned = Vec::new();
    for trial_index in 0..origin.trials_per_case {
        for case_id in &case_ids {
            for cell in &origin.cells {
                let slot = slots
                    .get(&(cell.cell_id.as_str(), *case_id, trial_index))
                    .copied()
                    .unwrap_or_default();
                if !slot_owes_attempt(max_infra_retries, MAX_ABANDONED_ATTEMPTS, slot.attempts) {
                    continue;
                }
                let highest = slot.highest;
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
