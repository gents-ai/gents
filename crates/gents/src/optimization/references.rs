//! Which eval runs an optimization job's journal names: its train,
//! validation, re-run and held-out runs. `gents eval gc` keeps them (spec 4b
//! §7), so a job's evidence and a later A/A calibration over it never lose
//! their homes to a side effect.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::optimization::job::{derive_state, load_job, JobState, JournalEntry};

/// Every run `journal` started and every run one of its decisions read.
pub fn journal_run_ids(journal: &[JournalEntry]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for entry in journal {
        match entry {
            JournalEntry::RunStarted { run_id, .. } => {
                ids.insert(run_id.clone());
            }
            JournalEntry::Decided { run_ids, .. } => ids.extend(run_ids.iter().cloned()),
            // Listed, not wildcarded: a new variant that names a run must be
            // added above, or gc and rm would no longer protect its run.
            JournalEntry::Frozen
            | JournalEntry::Proposed { .. }
            | JournalEntry::StructuralReject { .. }
            | JournalEntry::BudgetExhausted { .. }
            | JournalEntry::Finalized { .. }
            | JournalEntry::Promoted { .. }
            | JournalEntry::PromotionRefused { .. }
            | JournalEntry::Reverted { .. } => {}
        }
    }
    ids
}

/// The ids of every job `owner` has, sorted.
pub async fn job_ids(access: &ConfigAccess, owner: &str) -> Result<Vec<String>> {
    let query = format!(
        r#"{{ OptimizationJob(filter: {{ owner_agent_did: {{ _eq: "{owner}" }} }}) {{ job_id }} }}"#,
        owner = escape_graphql_string(owner),
    );
    let response = access
        .transact("optimization.job_ids", |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        })
        .await?;
    let mut ids: Vec<String> = response["data"]["OptimizationJob"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["job_id"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Every run any of `owner`'s jobs names.
pub async fn referenced_run_ids(access: &ConfigAccess, owner: &str) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for job_id in job_ids(access, owner).await? {
        if let Some(job) = load_job(access, owner, &job_id).await? {
            ids.extend(journal_run_ids(&job.journal));
        }
    }
    Ok(ids)
}

/// Whether a job's directory, and its runs' directories, may go without
/// `--force`. `ReadyToPromote` is not: `promote` verifies the
/// retained checkpoint pack in the job's directory.
pub fn removable(state: &JobState) -> bool {
    matches!(
        state,
        JobState::NothingToPromote
            | JobState::Exhausted
            | JobState::Failed { .. }
            | JobState::Stale
            | JobState::Promoted
            | JobState::Reverted
    )
}

/// Every run a job that is not [`removable`] still names, with
/// that job's id and state. `gents eval rm` refuses these without `--force`.
pub async fn held_runs(
    access: &ConfigAccess,
    owner: &str,
) -> Result<BTreeMap<String, (String, JobState)>> {
    let mut held = BTreeMap::new();
    for job_id in job_ids(access, owner).await? {
        let Some(job) = load_job(access, owner, &job_id).await? else {
            continue;
        };
        let state = derive_state(&job.journal);
        if removable(&state) {
            continue;
        }
        for run_id in journal_run_ids(&job.journal) {
            held.entry(run_id)
                .or_insert_with(|| (job.job_id.clone(), state.clone()));
        }
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::EvalSplit;
    use crate::eval::runner::freeze::tests::OWNER;
    use crate::optimization::driver::matrix::accepting_harness;
    use crate::optimization::job::{load_job, DecisionSummary, JobState};
    use crate::optimization::policy::{Decision, Mode};

    #[test]
    fn a_journal_names_every_run_it_started_or_decided_on() {
        let journal = vec![
            JournalEntry::Frozen,
            JournalEntry::RunStarted {
                run_id: "train".into(),
                round: Some(1),
                split: EvalSplit::Train,
            },
            JournalEntry::Decided {
                round: Some(1),
                attempt: 0,
                run_ids: vec!["val-a".into(), "val-b".into()],
                mode: Mode::Improve,
                decision: Decision::Accept,
                policy_version: "v2".into(),
                summary: DecisionSummary {
                    improved: 0,
                    tied: 0,
                    worsened: 0,
                    mean_diff_bp: None,
                    p_ppm: None,
                    alpha_effective_ppm: 0,
                    cost_skipped: true,
                },
            },
            JournalEntry::Reverted {
                by: "did:key:owner".into(),
            },
        ];
        assert_eq!(
            journal_run_ids(&journal),
            BTreeSet::from(["train".to_owned(), "val-a".to_owned(), "val-b".to_owned()])
        );
    }

    #[tokio::test]
    async fn every_run_of_an_owners_jobs_is_referenced() {
        let (harness, request) = accepting_harness("refs").await;
        let referenced = referenced_run_ids(harness.access(), OWNER).await.unwrap();
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert!(!referenced.is_empty());
        assert_eq!(referenced, journal_run_ids(&job.journal));
        assert_eq!(
            job_ids(harness.access(), OWNER).await.unwrap(),
            vec!["refs".to_owned()]
        );
        assert!(referenced_run_ids(harness.access(), "did:key:someone-else")
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn removable_states_are_the_settled_ones_that_need_nothing_more() {
        let failed = JobState::Failed {
            reason: "held_out_regression".into(),
        };
        for state in [
            JobState::NothingToPromote,
            JobState::Exhausted,
            failed,
            JobState::Stale,
            JobState::Promoted,
            JobState::Reverted,
        ] {
            assert!(removable(&state), "{state:?}");
        }
        assert!(!removable(&JobState::Running));
        assert!(
            !removable(&JobState::ReadyToPromote),
            "promote verifies the retained checkpoint pack in the job's directory"
        );
    }

    #[tokio::test]
    async fn a_ready_jobs_runs_are_held_and_a_foreign_owner_holds_none() {
        let (harness, request) = accepting_harness("held").await;
        let held = held_runs(harness.access(), OWNER).await.unwrap();
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            held.keys().cloned().collect::<BTreeSet<_>>(),
            journal_run_ids(&job.journal)
        );
        assert!(held
            .values()
            .all(|(job_id, state)| job_id == "held" && *state == JobState::ReadyToPromote));
        assert!(held_runs(harness.access(), "did:key:someone-else")
            .await
            .unwrap()
            .is_empty());
    }
}
