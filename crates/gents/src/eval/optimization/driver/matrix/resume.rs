use std::sync::atomic::AtomicBool;

use tokio_util::sync::CancellationToken;

use super::doubles::{CancellingExecutor, CancelsFirstValidationRun, ErroringProposer};
use super::fixtures::{
    base_executor, budgets, fail, repeating_proposer, script, script_attempts, CANDIDATE_PROMPT,
    DEFINITION, VALIDATION_CASES,
};
use super::journal::{assert_held_out_only_at_finalize, decisions, normalized, runs_started};
use super::{cancel_while_proposing, drive, journal, settle, Harness};
use crate::documents::EvalSplit;
use crate::eval::checks::CheckRegistry;
use crate::eval::load_trials;
use crate::eval::optimization::driver::baseline_dir;
use crate::eval::optimization::job::{create_job, load_job, JobState, JournalEntry};
use crate::eval::optimization::policy::Decision;
use crate::eval::optimization::proposer::ScriptedProposer;
use crate::eval::optimization::subject::{baseline_text, materialize_candidate, materialize_pack};
use crate::eval::optimization::target::JobTarget;
use crate::eval::runner::freeze::tests::OWNER;
use crate::eval::runner::ScriptedExecutor;

#[tokio::test]
async fn a_resumed_job_reaches_the_journal_of_its_uninterrupted_twin() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let registry = CheckRegistry::builtin();

    let twin = harness.request("twin-a", DEFINITION, budgets(1_000));
    let uninterrupted = settle(
        &harness,
        &twin,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(uninterrupted.state, JobState::ReadyToPromote);

    let resumed = harness.request("twin-b", DEFINITION, budgets(1_000));
    cancel_while_proposing(&harness, &resumed, &executor, 1).await;
    let partial = journal(&harness, "twin-b").await;
    assert!(
        matches!(
            partial.last(),
            Some(JournalEntry::Proposed { round: 1, .. })
        ),
        "{partial:#?}"
    );
    assert_eq!(
        runs_started(&partial, EvalSplit::Train),
        vec!["twin-b-r1-train".to_owned()]
    );

    let error = drive(
        &harness,
        &resumed,
        &executor,
        &ErroringProposer,
        &registry,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("proposer unavailable"),
        "{error:#}"
    );
    let partial = journal(&harness, "twin-b").await;
    assert_eq!(decisions(&partial), vec![(Some(1), Decision::Accept)]);
    assert_eq!(
        runs_started(&partial, EvalSplit::Train),
        vec!["twin-b-r1-train".to_owned(), "twin-b-r2-train".to_owned()]
    );

    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let finished = settle(&harness, &resumed, &executor, &proposer).await;
    assert_eq!(finished.state, JobState::ReadyToPromote);
    assert_eq!(
        finished.checkpoint.map(|held| held.text),
        uninterrupted.checkpoint.map(|held| held.text)
    );
    let asked: Vec<u32> = proposer
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.round)
        .collect();
    assert_eq!(
        asked,
        vec![2, 3],
        "a journaled proposal is never asked again"
    );

    assert_eq!(
        normalized(journal(&harness, "twin-b").await, "twin-b"),
        normalized(journal(&harness, "twin-a").await, "twin-a"),
    );
}

#[tokio::test]
async fn a_resumed_job_with_no_budget_slack_reaches_the_journal_of_its_twin() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let registry = CheckRegistry::builtin();

    let twin = harness.request("tight-a", DEFINITION, budgets(76));
    let uninterrupted = settle(&harness, &twin, &executor, &proposer).await;
    assert_eq!(
        uninterrupted.state,
        JobState::ReadyToPromote,
        "{uninterrupted:#?}"
    );
    let twin_journal = journal(&harness, "tight-a").await;
    assert_eq!(
        runs_started(&twin_journal, EvalSplit::Train),
        vec!["tight-a-r1-train".to_owned(), "tight-a-r2-train".to_owned()],
        "round 2 fits the budget exactly"
    );
    assert!(twin_journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::BudgetExhausted { round: Some(3), .. })));

    let resumed = harness.request("tight-b", DEFINITION, budgets(76));
    cancel_while_proposing(&harness, &resumed, &executor, 1).await;
    let round_one_only = ScriptedProposer::new(vec![(
        CANDIDATE_PROMPT.to_owned(),
        "name the collection the monitor should read".to_owned(),
    )]);
    let error = drive(
        &harness,
        &resumed,
        &executor,
        &round_one_only,
        &registry,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("no answer for round 2"),
        "{error:#}"
    );
    let finished = settle(&harness, &resumed, &executor, &proposer).await;
    assert_eq!(finished.state, JobState::ReadyToPromote);

    assert_eq!(
        normalized(journal(&harness, "tight-b").await, "tight-b"),
        normalized(twin_journal, "tight-a"),
    );
}

#[tokio::test]
async fn a_job_stopped_before_its_held_out_run_resumes_to_the_journal_of_its_twin() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());

    let twin = harness.request("before-held-out-a", DEFINITION, budgets(1_000));
    let uninterrupted = settle(
        &harness,
        &twin,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(uninterrupted.state, JobState::ReadyToPromote);

    let resumed = harness.request("before-held-out-b", DEFINITION, budgets(1_000));
    cancel_while_proposing(&harness, &resumed, &executor, 3).await;
    let partial = journal(&harness, "before-held-out-b").await;
    assert!(
        matches!(
            partial.last(),
            Some(JournalEntry::StructuralReject { round: 3, .. })
        ),
        "{partial:#?}"
    );
    assert!(runs_started(&partial, EvalSplit::HeldOut).is_empty());

    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let finished = settle(&harness, &resumed, &executor, &proposer).await;
    assert_eq!(finished.state, JobState::ReadyToPromote);
    assert!(proposer.calls.lock().unwrap().is_empty());
    let resumed_journal = journal(&harness, "before-held-out-b").await;
    assert_held_out_only_at_finalize(&resumed_journal);
    assert_eq!(
        normalized(resumed_journal, "before-held-out-b"),
        normalized(
            journal(&harness, "before-held-out-a").await,
            "before-held-out-a"
        ),
    );
}

fn accepting_executor_with_retry() -> ScriptedExecutor {
    script_attempts(
        base_executor(),
        "baseline",
        &VALIDATION_CASES,
        &[1, 2],
        |_| fail(),
    )
}

async fn assert_twin_after_cancel_inside(
    executor: &ScriptedExecutor,
    job: &str,
    run_suffix: &str,
    expected: JobState,
) {
    let harness = Harness::new().await;
    let twin_id = format!("{job}-a");
    let mut twin = harness.request(&twin_id, DEFINITION, budgets(1_000));
    twin.max_infra_retries = 1;
    let uninterrupted = settle(
        &harness,
        &twin,
        executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(uninterrupted.state, expected, "{uninterrupted:#?}");

    let resumed_id = format!("{job}-b");
    let mut resumed = harness.request(&resumed_id, DEFINITION, budgets(1_000));
    resumed.max_infra_retries = 1;
    let run_id = format!("{resumed_id}-{run_suffix}");
    let token = CancellationToken::new();
    let cancelling = CancellingExecutor {
        inner: executor,
        run_id: run_id.clone(),
        token: token.clone(),
    };
    let stopped = drive(
        &harness,
        &resumed,
        &cancelling,
        &repeating_proposer(CANDIDATE_PROMPT),
        &CheckRegistry::builtin(),
        token,
    )
    .await
    .unwrap();
    assert_eq!(stopped.state, JobState::Running, "{stopped:#?}");
    let partial = journal(&harness, &resumed_id).await;
    assert!(
        matches!(
            partial.last(),
            Some(JournalEntry::RunStarted { run_id: started, .. }) if *started == run_id
        ),
        "{partial:#?}"
    );
    let trials = load_trials(harness.access(), OWNER, &run_id).await.unwrap();
    assert_eq!(trials.len(), 1, "one trial launched, then abandoned");
    assert!(trials[0].completion.is_none());

    let finished = settle(
        &harness,
        &resumed,
        executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(finished.state, expected, "{finished:#?}");
    assert_eq!(
        normalized(journal(&harness, &resumed_id).await, &resumed_id),
        normalized(journal(&harness, &twin_id).await, &twin_id),
    );
}

#[tokio::test]
async fn a_job_stopped_inside_its_validation_run_resumes_to_the_journal_of_its_twin() {
    assert_twin_after_cancel_inside(
        &accepting_executor_with_retry(),
        "in-validation",
        "r1-v0",
        JobState::ReadyToPromote,
    )
    .await;
}

#[tokio::test]
async fn a_job_stopped_inside_its_rerun_resumes_to_the_journal_of_its_twin() {
    let executor = script_attempts(
        base_executor(),
        "candidate",
        &VALIDATION_CASES[..2],
        &[1, 2],
        |_| ScriptedExecutor::not_evidence("did:key:trial"),
    );
    assert_twin_after_cancel_inside(&executor, "in-rerun", "r1-v1", JobState::NothingToPromote)
        .await;
}

#[tokio::test]
async fn a_job_stopped_inside_its_held_out_run_resumes_to_the_journal_of_its_twin() {
    assert_twin_after_cancel_inside(
        &accepting_executor_with_retry(),
        "in-held-out",
        "held-out",
        JobState::ReadyToPromote,
    )
    .await;
}

#[tokio::test]
async fn a_job_created_without_frozen_resumes_to_the_journal_of_its_twin() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let proposer = repeating_proposer(CANDIDATE_PROMPT);

    let twin = harness.request("unfrozen-a", DEFINITION, budgets(1_000));
    let uninterrupted = settle(&harness, &twin, &executor, &proposer).await;
    assert_eq!(uninterrupted.state, JobState::ReadyToPromote);
    let origin = load_job(harness.access(), OWNER, "unfrozen-a")
        .await
        .unwrap()
        .unwrap()
        .origin;

    let resumed = harness.request("unfrozen-b", DEFINITION, budgets(1_000));
    let source = materialize_pack(
        &resumed.baseline_pack,
        OWNER,
        "monitor",
        &JobTarget::Context,
    )
    .unwrap();
    let copy = materialize_candidate(
        &source,
        OWNER,
        &baseline_text(&source).unwrap(),
        &baseline_dir(&resumed.jobs_dir, "unfrozen-b"),
    )
    .unwrap();
    assert_eq!(copy.digest, origin.subject.pack_digest);
    create_job(harness.access(), "unfrozen-b", OWNER, &origin)
        .await
        .unwrap();
    assert!(journal(&harness, "unfrozen-b").await.is_empty());

    let finished = settle(&harness, &resumed, &executor, &proposer).await;
    assert_eq!(finished.state, JobState::ReadyToPromote);
    let resumed_journal = journal(&harness, "unfrozen-b").await;
    assert_eq!(resumed_journal[0], JournalEntry::Frozen);
    assert_eq!(
        normalized(resumed_journal, "unfrozen-b"),
        normalized(journal(&harness, "unfrozen-a").await, "unfrozen-a"),
    );
}

#[tokio::test]
async fn a_cancelled_validation_run_journals_no_decision_and_the_job_resumes() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("job-cancelled", DEFINITION, budgets(1_000));
    let marking = CancelsFirstValidationRun {
        inner: &executor,
        marked: AtomicBool::new(false),
    };
    let stopped = drive(
        &harness,
        &request,
        &marking,
        &repeating_proposer(CANDIDATE_PROMPT),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(stopped.state, JobState::Running);
    let entries = journal(&harness, "job-cancelled").await;
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            JournalEntry::RunStarted {
                split: EvalSplit::Validation,
                ..
            }
        )),
        "{entries:#?}"
    );
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(entry, JournalEntry::Decided { .. })),
        "a cancelled run is never decided on: {entries:#?}"
    );

    let resumed = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(resumed.state, JobState::ReadyToPromote, "{resumed:#?}");
}
