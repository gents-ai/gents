use tokio_util::sync::CancellationToken;

use super::doubles::{DeadlineProposer, FeedbackCheck};
use super::fixtures::{
    base_executor, budgets, fail, pass, policy, repeating_proposer, script, with_usage,
    CANDIDATE_PROMPT, DEFINITION, FEEDBACK, FEEDBACK_DEFINITION, HELD_OUT_CASES, SMALL_DEFINITION,
    TRAIN_CASES, TRIALS_PER_CASE, VALIDATION_CASES,
};
use super::journal::{
    assert_held_out_only_at_finalize, decisions, runs_started, structural_rejects,
};
use super::{
    accepting_harness, drive, findings_capture, journal, rejecting_harness, settle, Harness,
};
use crate::documents::EvalSplit;
use crate::eval::checks::CheckRegistry;
use crate::eval::optimization::driver::TEST_CLOCK_OFFSET;
use crate::eval::optimization::evidence::decision_evidence;
use crate::eval::optimization::job::{JobState, JournalEntry};
use crate::eval::optimization::policy::{Decision, InconclusiveReason, RejectReason};
use crate::eval::optimization::proposer::ScriptedProposer;
use crate::eval::report::load_run_rows;
use crate::eval::runner::freeze::tests::OWNER;
use crate::eval::runner::ScriptedExecutor;
use crate::eval::{load_run, load_verdicts};

#[tokio::test]
async fn a_candidate_that_wins_every_case_is_accepted_and_reaches_ready_to_promote() {
    let (harness, request) = accepting_harness("accept").await;
    let journal = journal(&harness, &request.job_id).await;
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Accept), (None, Decision::Accept)],
        "one validation decision, one held-out confirmation: {journal:#?}"
    );
    assert_eq!(
        structural_rejects(&journal),
        2,
        "rounds 2 and 3 repeat round 1's text"
    );
    assert_eq!(
        runs_started(&journal, EvalSplit::HeldOut),
        vec!["accept-held-out".to_owned()],
        "the held-out split is run exactly once"
    );
    assert_held_out_only_at_finalize(&journal);
    let retained = crate::eval::optimization::checkpoint(&journal).expect("round 1 was accepted");
    assert_eq!(
        (retained.round, retained.text.as_str()),
        (1, CANDIDATE_PROMPT)
    );
    let mut started = 0;
    for split in [EvalSplit::Train, EvalSplit::Validation, EvalSplit::HeldOut] {
        for run_id in runs_started(&journal, split) {
            let frozen = crate::eval::runner::freeze::snapshot::frozen_captures(
                &request.runs_dir.join(&run_id),
            )
            .unwrap();
            assert_eq!(frozen, Some(vec![findings_capture()]), "{run_id}");
            started += 1;
        }
    }
    assert_eq!(
        started, 5,
        "three train runs, one validation run, one held-out run: {journal:#?}"
    );
}

#[tokio::test]
async fn a_deadline_that_passes_after_an_accept_still_runs_the_held_out_confirmation() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let deadline = crate::eval::optimization::driver::now_unix_secs() + 3_600;
    let mut request = harness.request("deadline", DEFINITION, budgets(1_000));
    request.budgets.deadline_unix_secs = Some(deadline);
    let proposer = DeadlineProposer {
        inner: repeating_proposer(CANDIDATE_PROMPT),
        deadline,
    };
    let outcome = TEST_CLOCK_OFFSET
        .scope(
            std::cell::Cell::new(0),
            settle(&harness, &request, &executor, &proposer),
        )
        .await;
    let journal = journal(&harness, &request.job_id).await;
    assert_eq!(outcome.state, JobState::ReadyToPromote, "{journal:#?}");
    let exhausted = journal
        .iter()
        .position(|entry| {
            matches!(entry, JournalEntry::BudgetExhausted { round: Some(2), reason }
                if reason.contains("deadline"))
        })
        .unwrap_or_else(|| panic!("round 2 was not stopped by the deadline: {journal:#?}"));
    let held_out = journal
        .iter()
        .position(|entry| {
            matches!(
                entry,
                JournalEntry::RunStarted {
                    split: EvalSplit::HeldOut,
                    ..
                }
            )
        })
        .unwrap_or_else(|| panic!("no held-out run: {journal:#?}"));
    assert!(exhausted < held_out, "{journal:#?}");
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Accept), (None, Decision::Accept)]
    );
}

#[tokio::test]
async fn a_candidate_that_wins_only_half_the_cases_is_rejected_for_no_improvement() {
    let (harness, request) = rejecting_harness("no-improvement").await;
    let journal = journal(&harness, &request.job_id).await;
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Reject(RejectReason::NoImprovement))]
    );
    assert!(
        runs_started(&journal, EvalSplit::HeldOut).is_empty(),
        "the held-out split is never touched when no round accepted"
    );
}

#[tokio::test]
async fn one_broken_case_rejects_the_candidate_even_though_the_mean_improves() {
    let harness = Harness::new().await;
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES[1..], |_| {
        fail()
    });
    executor = script(executor, "candidate", &VALIDATION_CASES[..1], |_| fail());
    let request = harness.request("case-regression", DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    assert_eq!(
        decisions(&journal(&harness, "case-regression").await),
        vec![(Some(1), Decision::Reject(RejectReason::CaseRegression))]
    );
}

#[tokio::test]
async fn a_candidate_that_costs_ten_times_the_tokens_is_rejected_for_cost() {
    let harness = Harness::new().await;
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| {
        with_usage(fail(), 100)
    });
    executor = script(executor, "candidate", &VALIDATION_CASES, |_| {
        with_usage(pass(), 1_000)
    });
    let request = harness.request("cost-regression", DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    let journal = journal(&harness, "cost-regression").await;
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Reject(RejectReason::CostRegression))]
    );
    assert!(journal.iter().any(|entry| matches!(
        entry,
        JournalEntry::Decided { summary, .. } if !summary.cost_skipped
    )));
}

#[tokio::test]
async fn a_repeated_candidate_is_structurally_rejected_and_costs_no_validation_run() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("structural", DEFINITION, budgets(1_000));
    let outcome = settle(&harness, &request, &executor, &ScriptedProposer::echoing()).await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    let journal = journal(&harness, "structural").await;
    assert_eq!(structural_rejects(&journal), 3);
    assert!(journal.iter().any(|entry| matches!(
        entry,
        JournalEntry::StructuralReject { round: 1, diagnostics } if diagnostics.starts_with("duplicate_candidate")
    )));
    assert!(
        runs_started(&journal, EvalSplit::Validation).is_empty(),
        "a structural rejection spends no validation run"
    );
    assert!(decisions(&journal).is_empty(), "and reaches no decision");
}

#[tokio::test]
async fn a_round_that_cannot_be_afforded_is_budget_exhausted_and_spends_nothing() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("exhausted", DEFINITION, budgets(25));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::Exhausted);
    let journal = journal(&harness, "exhausted").await;
    assert!(journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::BudgetExhausted { round: Some(1), .. })));
    assert!(
        decisions(&journal).is_empty(),
        "exhaustion is not a rejection"
    );
    assert!(!journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::RunStarted { .. })));
}

#[tokio::test]
async fn an_inconclusive_round_is_rerun_on_a_new_seed_and_ends_inconclusive() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "candidate", &VALIDATION_CASES[..2], |_| {
        ScriptedExecutor::not_evidence("did:key:trial")
    });
    let request = harness.request("inconclusive", DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    let journal = journal(&harness, "inconclusive").await;
    let decided = journal
        .iter()
        .find_map(|entry| match entry {
            JournalEntry::Decided {
                round: Some(1),
                attempt,
                run_ids,
                decision,
                ..
            } => Some((*attempt, run_ids.clone(), *decision)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{journal:#?}"));
    assert_eq!(decided.0, 1, "one re-run");
    assert_eq!(
        decided.1,
        vec![
            "inconclusive-r1-v0".to_owned(),
            "inconclusive-r1-v1".to_owned()
        ]
    );
    assert_eq!(
        decided.2,
        Decision::Inconclusive(InconclusiveReason::Insufficient)
    );
    let mut seeds = Vec::new();
    for run_id in &decided.1 {
        let run = load_run(harness.access(), OWNER, run_id)
            .await
            .unwrap()
            .unwrap();
        seeds.push(run.origin.seed_base);
    }
    assert_ne!(seeds[0], seeds[1], "the re-run draws a new seed base");
    let definition =
        crate::eval::optimization::driver::load_definition(harness.access(), OWNER, DEFINITION)
            .await
            .unwrap();
    let mut runs = Vec::new();
    for run_id in &decided.1 {
        runs.push(
            load_run_rows(harness.access(), OWNER, run_id)
                .await
                .unwrap(),
        );
    }
    let evidence = decision_evidence(&definition, &runs, policy().max_missing_usage_bp);
    let val_c = evidence
        .cases
        .iter()
        .find(|case| case.case_id == "val-c")
        .unwrap();
    assert_eq!(val_c.pairs, 4, "two trials in each of two runs");
}

#[tokio::test]
async fn five_validation_cases_can_never_be_accepted() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES[..5], |_| {
        fail()
    });
    let request = harness.request("too-few", SMALL_DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    assert_eq!(
        decisions(&journal(&harness, "too-few").await),
        vec![(
            Some(1),
            Decision::Inconclusive(InconclusiveReason::TooFewCases)
        )]
    );
}

#[tokio::test]
async fn a_checkpoint_that_regresses_on_held_out_fails_the_job() {
    let harness = Harness::new().await;
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    executor = script(executor, "candidate", &HELD_OUT_CASES, |_| fail());
    let request = harness.request("held-out-regression", DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(
        outcome.state,
        JobState::Failed {
            reason: "held_out_regression".into()
        }
    );
    let journal = journal(&harness, "held-out-regression").await;
    assert_eq!(
        decisions(&journal),
        vec![
            (Some(1), Decision::Accept),
            (None, Decision::Reject(RejectReason::CaseRegression))
        ]
    );
    assert_eq!(runs_started(&journal, EvalSplit::HeldOut).len(), 1);
    assert_held_out_only_at_finalize(&journal);
}

#[tokio::test]
async fn feedback_reaches_the_proposer_only_from_the_train_run() {
    let harness = Harness::new().await;
    let registry = CheckRegistry::builtin().with(Box::new(FeedbackCheck));
    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let request = harness.request("feedback", FEEDBACK_DEFINITION, budgets(1_000));
    drive(
        &harness,
        &request,
        &base_executor(),
        &proposer,
        &registry,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let calls = proposer.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "one proposal per round");
    for call in &calls {
        assert_eq!(
            call.feedback.len(),
            TRAIN_CASES.len() * TRIALS_PER_CASE as usize
        );
        assert!(call
            .feedback
            .iter()
            .all(|entry| entry.feedback.as_deref() == Some(FEEDBACK)));
    }
    let validation = load_verdicts(harness.access(), OWNER, "feedback-r1-v0")
        .await
        .unwrap();
    assert!(!validation.is_empty());
    assert!(
        validation.iter().all(|verdict| verdict.feedback.is_none()),
        "the runner never records feedback off the train split"
    );
}

#[tokio::test]
async fn an_inconclusive_round_whose_rerun_is_unaffordable_keeps_its_decision_and_ends_the_rounds()
{
    let harness = Harness::new().await;
    let executor = script(base_executor(), "candidate", &VALIDATION_CASES[..2], |_| {
        ScriptedExecutor::not_evidence("did:key:trial")
    });
    let request = harness.request("inconclusive-tight", DEFINITION, budgets(50));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::Exhausted, "{outcome:#?}");
    assert_eq!(outcome.checkpoint, None);

    let journal = journal(&harness, "inconclusive-tight").await;
    let at = journal
        .iter()
        .position(|entry| matches!(entry, JournalEntry::Decided { .. }))
        .unwrap_or_else(|| panic!("{journal:#?}"));
    match &journal[at] {
        JournalEntry::Decided {
            round,
            attempt,
            run_ids,
            decision,
            ..
        } => {
            assert_eq!((*round, *attempt), (Some(1), 0));
            assert_eq!(run_ids, &vec!["inconclusive-tight-r1-v0".to_owned()]);
            assert_eq!(
                *decision,
                Decision::Inconclusive(InconclusiveReason::Insufficient)
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(
            &journal[at + 1],
            JournalEntry::BudgetExhausted { round: None, .. }
        ),
        "{journal:#?}"
    );
    assert_eq!(
        runs_started(&journal, EvalSplit::Validation),
        vec!["inconclusive-tight-r1-v0".to_owned()],
        "the unaffordable re-run never started"
    );
    assert_eq!(decisions(&journal).len(), 1);
    assert!(matches!(
        journal.last(),
        Some(JournalEntry::Finalized {
            state: JobState::Exhausted
        })
    ));
}
