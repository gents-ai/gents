use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::config_client::ConfigAccess;
use crate::eval::optimization::job::{
    derive_state, fold_jobs_for_owner, job_ids_in_txn, JobState, JournalEntry,
};

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

pub async fn job_ids(access: &ConfigAccess, owner: &str) -> Result<Vec<String>> {
    let owner = owner.to_owned();
    let response = access
        .transact_readonly("optimization.job_ids", |txn| {
            let owner = &owner;
            Box::pin(async move { job_ids_in_txn(txn, owner).await })
        })
        .await?;
    Ok(response)
}

pub async fn referenced_run_ids(access: &ConfigAccess, owner: &str) -> Result<BTreeSet<String>> {
    fold_jobs_for_owner(access, owner, |ids: &mut BTreeSet<String>, job| {
        ids.extend(journal_run_ids(&job.journal));
        Ok(())
    })
    .await
}

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

pub async fn held_runs(
    access: &ConfigAccess,
    owner: &str,
) -> Result<BTreeMap<String, (String, JobState)>> {
    fold_jobs_for_owner(
        access,
        owner,
        |held: &mut BTreeMap<String, (String, JobState)>, job| {
            let state = derive_state(&job.journal);
            if removable(&state) {
                return Ok(());
            }
            for run_id in journal_run_ids(&job.journal) {
                held.entry(run_id)
                    .or_insert_with(|| (job.job_id.clone(), state.clone()));
            }
            Ok(())
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::optimization::driver::matrix::accepting_harness;
    use crate::eval::optimization::job::{load_job, JobState};
    use crate::eval::runner::freeze::tests::OWNER;

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

    #[tokio::test]
    async fn owner_jobs_are_folded_in_deterministic_order() {
        let (harness, request) = accepting_harness("bulk-refs").await;
        let jobs = crate::eval::optimization::references::job_ids(harness.access(), OWNER)
            .await
            .unwrap();
        let source = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        for index in 0..128 {
            crate::eval::optimization::job::create_job(
                harness.access(),
                &format!("bulk-{index:03}"),
                OWNER,
                &source.origin,
            )
            .await
            .unwrap();
        }

        let all_job_ids = jobs
            .into_iter()
            .chain((0..128).map(|index| format!("bulk-{index:03}")))
            .collect::<Vec<_>>();
        let mut expected = BTreeSet::new();
        for job_id in &all_job_ids {
            if let Some(job) = load_job(harness.access(), OWNER, job_id).await.unwrap() {
                expected.extend(journal_run_ids(&job.journal));
            }
        }
        let (referenced, batches) = crate::eval::optimization::job::count_owner_job_batches(
            referenced_run_ids(harness.access(), OWNER),
        )
        .await;
        assert_eq!(referenced.unwrap(), expected);
        assert_eq!(all_job_ids.len(), 129);
        assert_eq!(batches, 3);
        assert_eq!(
            held_runs(harness.access(), OWNER).await.unwrap().len(),
            expected.len()
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
