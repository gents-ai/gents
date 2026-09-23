//! Which trials a run still owes.
//!
//! Planning is a pure function of the run's origin and the trials already
//! written, so a resumed run plans exactly the slots that never completed. The
//! order pairs the cells of one case adjacently, so a paired comparison sees
//! both arms close together in time.

use sha2::{Digest, Sha256};

use crate::eval::{RunOrigin, TrialRecord};

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

/// The trials `run_id` still owes, given what it has already written.
///
/// A slot with any completed attempt is done. A slot whose attempts were all
/// abandoned is planned again at one past its highest attempt, so the new row
/// never collides with the old one.
pub fn plan(origin: &RunOrigin, run_id: &str, existing: &[TrialRecord]) -> Vec<PlannedTrial> {
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
                let mut completed = false;
                for record in attempts {
                    completed |= record.completion.is_some();
                    highest = highest.max(Some(record.identity.attempt));
                }
                if completed {
                    continue;
                }
                let attempt = highest.map_or(1, |attempt| attempt + 1);
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
        Anchor, CellSpec, DefinitionRef, RunOrigin, SubjectRef, TrialCompletion, TrialIdentity,
        TrialRecord, TrialUsage, DENOMINATOR_POLICY_V1, TAXONOMY_VERSION,
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

    fn record(cell: &str, case: &str, index: u32, attempt: u32, completed: bool) -> TrialRecord {
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
            completion: completed.then(|| TrialCompletion {
                ended_at: "t".into(),
                stages: vec![],
                usage: TrialUsage::default(),
                anchor: Anchor {
                    terminal_states: vec![],
                    requests: 0,
                    inference_calls: 0,
                },
                evidence_digest: None,
            }),
        }
    }

    #[test]
    fn a_fresh_run_plans_the_full_matrix_pairs_adjacent() {
        let planned = plan(
            &origin(&["base", "cand"], &["b-case", "a-case"], 2),
            "r",
            &[],
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
        let planned = plan(&origin(&["base", "cand"], &["a-case"], 1), "r", &existing);
        assert_eq!(planned.len(), 1);
        assert_eq!(
            (planned[0].cell_id.as_str(), planned[0].attempt),
            ("cand", 3)
        );
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
        assert!(plan(&origin(&["base"], &["a-case"], 1), "r", &existing).is_empty());
    }
}
