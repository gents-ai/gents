//! `show`: a job as its journal states it, with every decision recomputed
//! from the `EvalVerdict` rows of the runs it names. The journal's summary is
//! a convenience; these rows are the authority (spec section 5, ruling R1).
//!
//! A decision is recomputed through the driver's own decision path, so an
//! invalidated run is left out exactly as the driver leaves it out, and the
//! seed still follows every run the decision names. The view renders through
//! serde: a decision's reason is its snake_case name (ruling C2).

use anyhow::Result;
use serde::Serialize;

use crate::config_client::ConfigAccess;
use crate::optimization::driver::{decide_runs, definition_ref, load_definition};
use crate::optimization::job::{
    checkpoint, derive_state, load_job, rounds_used, Checkpoint, JobRecord, JobState, JournalEntry,
};
use crate::optimization::policy::{Decision, Mode};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DecisionView {
    pub round: Option<u32>,
    pub attempt: u32,
    pub mode: Mode,
    pub run_ids: Vec<String>,
    pub journaled: Decision,
    pub recomputed: Decision,
    /// The rows no longer support what the journal says.
    pub mismatch: bool,
    /// A run this decision read has since been invalidated.
    pub invalidated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct JobView {
    pub job: JobRecord,
    pub state: JobState,
    pub checkpoint: Option<Checkpoint>,
    pub rounds_used: u32,
    /// The installed definition no longer matches the one the job froze, so
    /// the recomputations below read a different instrument.
    pub definition_changed: bool,
    pub decisions: Vec<DecisionView>,
}

/// Project `job_id`'s journal. Only `Decided` entries are recomputed: a
/// structural rejection, including a text refused before any pack was
/// materialized (ruling P-N5), and a `BudgetExhausted` spent no run, so they
/// appear in `job.journal` as written. A decision the rounds ended on (ruling
/// P-N3) is recomputed like any other.
pub async fn show(access: &ConfigAccess, owner: &str, job_id: &str) -> Result<JobView> {
    let job = load_job(access, owner, job_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no optimization job {job_id:?} for {owner}"))?;
    let origin = &job.origin;
    let definition = load_definition(access, owner, &origin.definition.definition_id).await?;
    let definition_changed = definition_ref(&definition)? != origin.definition;

    let mut decisions = Vec::new();
    for entry in &job.journal {
        let JournalEntry::Decided {
            round,
            attempt,
            run_ids,
            mode,
            decision,
            ..
        } = entry
        else {
            continue;
        };
        let (report, invalidated) =
            decide_runs(access, owner, &definition, &origin.policy, *mode, run_ids).await?;
        decisions.push(DecisionView {
            round: *round,
            attempt: *attempt,
            mode: *mode,
            run_ids: run_ids.clone(),
            journaled: *decision,
            recomputed: report.decision,
            mismatch: report.decision != *decision,
            invalidated,
        });
    }
    Ok(JobView {
        state: derive_state(&job.journal),
        checkpoint: checkpoint(&job.journal),
        rounds_used: rounds_used(&job.journal),
        definition_changed,
        decisions,
        job,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::invalidate_run;
    use crate::eval::runner::freeze::tests::OWNER;
    use crate::eval::runner::ScriptedExecutor;
    use crate::optimization::driver::matrix::accepting_harness;
    use crate::optimization::driver::matrix::{
        base_executor, budgets, fail, rejecting_harness, repeating_proposer, script, settle,
        Harness, CANDIDATE_PROMPT, DEFINITION, VALIDATION_CASES,
    };
    use crate::optimization::policy::{InconclusiveReason, RejectReason};
    use crate::optimization::proposer::ScriptedProposer;

    #[tokio::test]
    async fn every_journaled_decision_recomputes_from_its_verdicts() {
        let (harness, request) = accepting_harness("show-clean").await;
        let view = show(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap();
        assert_eq!(view.state, JobState::ReadyToPromote);
        assert!(!view.definition_changed);
        assert_eq!(view.decisions.len(), 2, "{:#?}", view.decisions);
        for decision in &view.decisions {
            assert_eq!(decision.journaled, decision.recomputed, "{decision:#?}");
            assert!(!decision.mismatch && !decision.invalidated, "{decision:#?}");
        }
    }

    #[tokio::test]
    async fn a_decision_on_an_invalidated_run_is_flagged() {
        let (harness, request) = accepting_harness("show-invalidated").await;
        invalidate_run(
            harness.access(),
            OWNER,
            "show-invalidated-r1-v0",
            OWNER,
            "a fixture in this run was broken",
        )
        .await
        .unwrap();
        let view = show(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap();
        let round_one = view
            .decisions
            .iter()
            .find(|decision| decision.round == Some(1))
            .unwrap();
        assert!(round_one.invalidated, "{round_one:#?}");
        assert_eq!(round_one.journaled, Decision::Accept);
        assert!(
            round_one.mismatch,
            "with its only run left out, the rows no longer support the accept: {round_one:#?}"
        );
        let held_out = view
            .decisions
            .iter()
            .find(|decision| decision.round.is_none())
            .unwrap();
        assert!(
            !held_out.invalidated,
            "only the invalidated run's decision is flagged"
        );
    }

    /// Ruling C2: a reason renders as its serde name, never as `Debug`.
    #[tokio::test]
    async fn a_rejection_renders_its_reason_in_snake_case() {
        let (harness, request) = rejecting_harness("show-rejected").await;
        let view = show(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap();
        assert_eq!(view.state, JobState::NothingToPromote);
        let [decision] = view.decisions.as_slice() else {
            panic!("{:#?}", view.decisions);
        };
        assert_eq!(
            decision.recomputed,
            Decision::Reject(RejectReason::NoImprovement)
        );
        assert!(!decision.mismatch, "{decision:#?}");
        let rendered = serde_json::to_value(&view).unwrap();
        assert_eq!(
            rendered["decisions"][0]["journaled"],
            serde_json::json!({"decision": "reject", "reason": "no_improvement"})
        );
        assert_eq!(
            rendered["state"],
            serde_json::json!({"state": "nothing_to_promote"})
        );
        assert!(!rendered.to_string().contains("NoImprovement"));
    }

    /// Ruling P-N3: the decision the rounds ended on is followed by
    /// `BudgetExhausted { round: None }` and recomputes like any other.
    #[tokio::test]
    async fn the_decision_an_unaffordable_rerun_ended_on_recomputes() {
        let harness = Harness::new().await;
        let executor = script(base_executor(), "candidate", &VALIDATION_CASES[..2], |_| {
            ScriptedExecutor::not_evidence("did:key:trial")
        });
        let request = harness.request("show-tight", DEFINITION, budgets(50));
        settle(
            &harness,
            &request,
            &executor,
            &repeating_proposer(CANDIDATE_PROMPT),
        )
        .await;
        let view = show(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap();
        assert_eq!(view.state, JobState::Exhausted);
        assert_eq!(view.checkpoint, None);
        let [decision] = view.decisions.as_slice() else {
            panic!("{:#?}", view.decisions);
        };
        assert_eq!((decision.round, decision.attempt), (Some(1), 0));
        assert_eq!(
            decision.recomputed,
            Decision::Inconclusive(InconclusiveReason::Insufficient)
        );
        assert!(!decision.mismatch && !decision.invalidated, "{decision:#?}");
        assert!(view
            .job
            .journal
            .iter()
            .any(|entry| matches!(entry, JournalEntry::BudgetExhausted { round: None, .. })));
    }

    /// Ruling P-N5: a text refused before materialization has a text-only
    /// digest and no pack; it is a round used and no decision.
    #[tokio::test]
    async fn a_text_refused_before_materialization_is_a_round_and_no_decision() {
        let harness = Harness::new().await;
        let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
        let request = harness.request("show-empty", DEFINITION, budgets(1_000));
        let proposer = ScriptedProposer::new(vec![("   ".to_owned(), "nothing".to_owned()); 3]);
        settle(&harness, &request, &executor, &proposer).await;
        let view = show(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap();
        assert_eq!(view.state, JobState::NothingToPromote);
        assert_eq!(view.rounds_used, 3);
        assert_eq!(view.checkpoint, None);
        assert!(view.decisions.is_empty(), "{:#?}", view.decisions);
        assert!(view.job.journal.iter().any(|entry| matches!(
            entry,
            JournalEntry::Proposed { candidate_digest, .. } if candidate_digest.starts_with("text-sha256:")
        )));
    }
}
