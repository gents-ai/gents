//! Which trials a run still owes.
//!
//! Planning is a pure function of the run's origin and the trials already
//! written, so a resumed run plans exactly the slots that still owe an answer.
//! The order pairs the cells of one case adjacently, so a paired comparison
//! sees both arms close together in time.
//!
//! A slot owes an answer while it has none: no attempt at all, an attempt the
//! runner never finished, or a finished attempt that turned out not to be
//! evidence about the subject. The last of those is why the rule lives here
//! rather than in the loop: what a finished attempt came to is readable from
//! its own completion, so a resumed run reads it the same way the run that
//! wrote it did.

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
///
/// An attempt that observed nothing is not. Neither is one whose every stage
/// that actually ran ended in an outcome that is not evidence: a stage the
/// trial never reached says nothing about the subject, and a stage that ran to
/// completion says everything.
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
///
/// Meaningful once [`plan`] has nothing left to plan: a slot whose latest
/// completed attempt is not evidence has by then been attempted as often as
/// the run permits.
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

/// The trials `run_id` still owes, given what it has already written.
///
/// A slot is done once an attempt finished with evidence about the subject.
/// A slot whose latest completed attempt is not evidence, and a slot whose
/// attempts were all abandoned, are planned again at one past their highest
/// attempt, so the new row never collides with an old one — until the slot has
/// used `max_infra_retries + 1` attempts, after which the run stops paying for
/// a question it keeps failing to ask.
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
                let mut latest_completed: Option<(u32, &TrialCompletion)> = None;
                for record in attempts {
                    highest = highest.max(Some(record.identity.attempt));
                    let Some(completion) = &record.completion else {
                        continue;
                    };
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
                let attempt = highest.map_or(1, |attempt| attempt + 1);
                if attempt > max_infra_retries.saturating_add(1) {
                    continue;
                }
                planned.push(PlannedTrial {
                    trial_id: trial_id_for(run_id, &cell.cell_id, case_id, trial_index, attempt),
                    cell_id: cell.cell_id.clone(),
                    cell_label: cell.label.clone(),
                    case_id: (*case_id).to_string(),
                    trial_index,
                    attempt,
                    seed: origin.seed_base + trial_index as i64,
                });
            }
        }
    }
    planned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::EvalSplit;
    use crate::eval::{
        Anchor, CellSpec, DefinitionRef, ProviderReason, RunOrigin, StageCompletion, SubjectRef,
        TrialCompletion, TrialIdentity, TrialRecord, TrialUsage, DENOMINATOR_POLICY_V1,
        TAXONOMY_VERSION,
    };

    fn origin(cells: &[&str], cases: &[&str], trials_per_case: u32) -> RunOrigin {
        RunOrigin {
            definition: DefinitionRef {
                definition_id: "d".into(),
                comparability_version: 1,
                digest: "x".into(),
            },
            split: EvalSplit::Validation,
            case_ids: cases.iter().map(|c| c.to_string()).collect(),
            cells: cells
                .iter()
                .map(|c| CellSpec {
                    cell_id: c.to_string(),
                    label: c.to_string(),
                    subject: SubjectRef {
                        pack_digest: "p".into(),
                        behavior_id: "b".into(),
                    },
                    inference_profile_id: "prof".into(),
                })
                .collect(),
            trials_per_case,
            seed_base: 100,
            deadline_secs: None,
            concurrency: 1,
            denominator_policy: DENOMINATOR_POLICY_V1.into(),
            taxonomy_version: TAXONOMY_VERSION.into(),
            max_infra_retries: 3,
            breaker_threshold: 5,
            check_registry_version: "0".into(),
            source_commit: "c".into(),
            source_dirty: false,
            purpose: "eval".into(),
        }
    }

    /// One stage of a completion: `None` ran to the end, anything else names
    /// how it failed.
    fn stage(failure_kind: Option<OutcomeKind>, reason: Option<ProviderReason>) -> StageCompletion {
        StageCompletion {
            stage_id: "s1".into(),
            request_id: failure_kind
                .is_none_or(|kind| kind != OutcomeKind::SkippedPrerequisite)
                .then(|| "req".to_string()),
            terminal_state: None,
            failure_kind,
            provider_reason: reason,
        }
    }

    fn completion(stages: Vec<StageCompletion>) -> TrialCompletion {
        TrialCompletion {
            ended_at: "t".into(),
            stages,
            usage: TrialUsage::default(),
            anchor: Anchor {
                terminal_states: vec![],
                requests: 0,
                inference_calls: 0,
            },
            evidence_digest: None,
        }
    }

    /// A row whose completion, when it has one, is evidence about the subject.
    fn record(cell: &str, case: &str, index: u32, attempt: u32, completed: bool) -> TrialRecord {
        let finished = completed.then(|| completion(vec![stage(None, None)]));
        finished_record(cell, case, index, attempt, finished)
    }

    fn finished_record(
        cell: &str,
        case: &str,
        index: u32,
        attempt: u32,
        completion: Option<TrialCompletion>,
    ) -> TrialRecord {
        TrialRecord {
            identity: TrialIdentity {
                trial_id: trial_id_for("r", cell, case, index, attempt),
                run_id: "r".into(),
                cell_id: cell.into(),
                case_id: case.into(),
                trial_index: index,
                attempt,
                trial_agent_did: "did:x".into(),
                session_id: "s".into(),
                seed: 100 + index as i64,
                home_hint: None,
            },
            created_at: "t".into(),
            completion,
        }
    }

    /// A finished attempt that learned nothing: its one stage was lost to a
    /// provider outage.
    fn no_evidence(cell: &str, case: &str, attempt: u32) -> TrialRecord {
        finished_record(
            cell,
            case,
            0,
            attempt,
            Some(completion(vec![stage(
                Some(OutcomeKind::Provider),
                Some(ProviderReason::Unavailable),
            )])),
        )
    }

    #[test]
    fn a_fresh_run_plans_the_full_matrix_pairs_adjacent() {
        let planned = plan(
            &origin(&["base", "cand"], &["b-case", "a-case"], 2),
            "r",
            &[],
            3,
        );
        let keys: Vec<(u32, &str, &str)> = planned
            .iter()
            .map(|p| (p.trial_index, p.case_id.as_str(), p.cell_id.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                (0, "a-case", "base"),
                (0, "a-case", "cand"),
                (0, "b-case", "base"),
                (0, "b-case", "cand"),
                (1, "a-case", "base"),
                (1, "a-case", "cand"),
                (1, "b-case", "base"),
                (1, "b-case", "cand"),
            ]
        );
        assert!(planned.iter().all(|p| p.attempt == 1));
        assert_eq!(planned[0].seed, 100);
        assert_eq!(planned[4].seed, 101);
    }

    #[test]
    fn completed_slots_are_skipped_and_abandoned_slots_get_the_next_attempt() {
        let existing = vec![
            record("base", "a-case", 0, 1, true),
            record("cand", "a-case", 0, 1, false),
            record("cand", "a-case", 0, 2, false),
        ];
        let planned = plan(
            &origin(&["base", "cand"], &["a-case"], 1),
            "r",
            &existing,
            3,
        );
        assert_eq!(planned.len(), 1);
        assert_eq!(
            (planned[0].cell_id.as_str(), planned[0].attempt),
            ("cand", 3)
        );
    }

    /// A finished attempt that learned nothing leaves the slot owing an
    /// answer, up to the retry cap; anything the subject caused ends it.
    #[test]
    fn a_slot_is_replanned_while_its_latest_completed_attempt_is_not_evidence() {
        let origin = origin(&["base"], &["a-case"], 1);
        let next = |existing: &[TrialRecord], max_infra_retries: u32| {
            plan(&origin, "r", existing, max_infra_retries)
                .first()
                .map(|planned| planned.attempt)
        };

        assert_eq!(
            next(&[no_evidence("base", "a-case", 1)], 3),
            Some(2),
            "one attempt that learned nothing is retried"
        );
        assert_eq!(
            next(
                &[
                    no_evidence("base", "a-case", 1),
                    no_evidence("base", "a-case", 2),
                    no_evidence("base", "a-case", 3),
                ],
                2,
            ),
            None,
            "the slot stops at max_infra_retries + 1 attempts"
        );
        assert_eq!(
            next(
                &[
                    no_evidence("base", "a-case", 1),
                    record("base", "a-case", 0, 2, true),
                ],
                3,
            ),
            None,
            "an attempt that learned something ends the slot"
        );
        assert_eq!(
            next(
                &[
                    record("base", "a-case", 0, 1, false),
                    no_evidence("base", "a-case", 2),
                ],
                3,
            ),
            Some(3),
            "an abandoned row and a finished one that learned nothing both count as attempts"
        );
        assert_eq!(
            not_evidence_slots(&[
                no_evidence("base", "a-case", 1),
                no_evidence("base", "a-case", 2),
            ]),
            1,
            "the slot is counted once, not once per attempt"
        );
        assert_eq!(
            not_evidence_slots(&[
                no_evidence("base", "a-case", 1),
                record("base", "a-case", 0, 2, true),
            ]),
            0
        );
    }

    /// A trial that reached no stage at all, and one whose only stages were
    /// skipped because it never got that far, are both silence.
    #[test]
    fn a_completion_with_nothing_that_ran_is_not_evidence() {
        assert!(completion_is_not_evidence(&completion(vec![])));
        assert!(completion_is_not_evidence(&completion(vec![stage(
            Some(OutcomeKind::SkippedPrerequisite),
            None
        )])));
        assert!(!completion_is_not_evidence(&completion(vec![stage(
            Some(OutcomeKind::Deadline),
            None
        )])));
        assert!(!completion_is_not_evidence(&completion(vec![
            stage(
                Some(OutcomeKind::Provider),
                Some(ProviderReason::Unavailable)
            ),
            stage(None, None),
        ])));
    }

    #[test]
    fn trial_ids_are_deterministic_and_distinct_per_attempt() {
        assert_eq!(
            trial_id_for("r", "c", "k", 0, 1),
            trial_id_for("r", "c", "k", 0, 1)
        );
        assert_ne!(
            trial_id_for("r", "c", "k", 0, 1),
            trial_id_for("r", "c", "k", 0, 2)
        );
        assert_eq!(trial_id_for("r", "c", "k", 0, 1).len(), 64);
    }

    #[test]
    fn a_completed_later_attempt_wins_over_an_abandoned_earlier_one() {
        let existing = vec![
            record("base", "a-case", 0, 1, false),
            record("base", "a-case", 0, 2, true),
        ];
        assert!(plan(&origin(&["base"], &["a-case"], 1), "r", &existing, 3).is_empty());
    }
}
