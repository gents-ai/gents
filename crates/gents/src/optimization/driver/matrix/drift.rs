use tokio_util::sync::CancellationToken;

use super::fixtures::{
    base_executor, behavior, budgets, definition, fail, policy, repeating_proposer, script,
    CANDIDATE_PROMPT, DEFINITION, TRAIN_CASES, TRIALS_PER_CASE, VALIDATION_CASES,
};
use super::journal::runs_started;
use super::{
    accepting_harness, cancel_while_proposing, drive, frozen_job, journal, settle, Harness,
};
use crate::documents::EvalSplit;
use crate::eval::checks::CheckRegistry;
use crate::eval::runner::freeze::tests::OWNER;
use crate::eval::runner::ScriptedExecutor;
use crate::optimization::driver::{baseline_dir, spend_so_far, JobRequest};
use crate::optimization::job::{JobState, JournalEntry};
use crate::Collection;

#[tokio::test]
async fn a_resume_with_another_capture_list_is_refused() {
    let (harness, request) = accepting_harness("captures-resume").await;
    let before = journal(&harness, &request.job_id).await;
    let mut changed = request.clone();
    changed.captures.clear();
    let error = settle_err(&harness, &changed).await;
    let refusal =
        crate::optimization::driver::job_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
    assert!(refusal.0.contains("captures"), "{}", refusal.0);
    assert_eq!(journal(&harness, &request.job_id).await, before);
}

#[tokio::test]
async fn a_pack_that_is_not_the_live_configuration_is_refused_at_freeze() {
    let harness = Harness::new().await;
    let refusal = |error: anyhow::Error| {
        crate::optimization::driver::job_refused(&error)
            .unwrap_or_else(|| panic!("{error:#}"))
            .0
            .clone()
    };

    let mut request = harness.request("other-tools", DEFINITION, budgets(1_000));
    request.baseline_pack = harness.launching.pack("read-only", "ReadOnly");
    let error = crate::optimization::driver::freeze_job(harness.access(), &request, &policy())
        .await
        .unwrap_err();
    let reason = refusal(error);
    assert!(
        reason.contains("Tools \"monitor-tools\"") && reason.contains("host"),
        "{reason}"
    );
    assert!(!baseline_dir(&request.jobs_dir, &request.job_id).exists());

    harness.install(vec![behavior("Monitor, renamed")]).await;
    let request = harness.request("other-behavior", DEFINITION, budgets(1_000));
    let error = crate::optimization::driver::freeze_job(harness.access(), &request, &policy())
        .await
        .unwrap_err();
    let reason = refusal(error);
    assert!(
        reason.contains("Agent \"monitor\"") && reason.contains("display_name"),
        "{reason}"
    );
    assert!(!baseline_dir(&request.jobs_dir, &request.job_id).exists());

    harness.install(vec![behavior("Monitor")]).await;
    let request = harness.request("equivalent", DEFINITION, budgets(1_000));
    crate::optimization::driver::freeze_job(harness.access(), &request, &policy())
        .await
        .unwrap();
    assert!(baseline_dir(&request.jobs_dir, &request.job_id).exists());
}

async fn settle_err(harness: &Harness, request: &JobRequest) -> anyhow::Error {
    drive(
        harness,
        request,
        &base_executor(),
        &repeating_proposer(CANDIDATE_PROMPT),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err()
}

#[tokio::test]
async fn a_baseline_edited_after_the_freeze_fails_the_job_before_any_further_spend() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("drifted", DEFINITION, budgets(1_000));
    let (before, spent) = stopped_after_round_one_train(&harness, &request, &executor).await;

    harness.install(vec![behavior("Monitor, renamed")]).await;
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    let failed = JobState::Failed {
        reason: "baseline_drifted".into(),
    };
    assert_eq!(outcome.state, failed, "{outcome:#?}");
    assert_nothing_after_drift(&harness, "drifted", before, spent, failed).await;
}

#[tokio::test]
async fn a_definition_edited_after_the_freeze_fails_the_job_as_definition_changed() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("definition-drift", DEFINITION, budgets(1_000));
    let (before, spent) = stopped_after_round_one_train(&harness, &request, &executor).await;

    harness
        .install(vec![(
            Collection::EvalDefinition,
            definition(DEFINITION, "captured_rows_count", &VALIDATION_CASES, 2),
        )])
        .await;
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    let failed = JobState::Failed {
        reason: "definition_changed".into(),
    };
    assert_eq!(outcome.state, failed, "{outcome:#?}");
    assert_nothing_after_drift(&harness, "definition-drift", before, spent, failed).await;
}

async fn stopped_after_round_one_train(
    harness: &Harness,
    request: &JobRequest,
    executor: &ScriptedExecutor,
) -> (Vec<JournalEntry>, u64) {
    cancel_while_proposing(harness, request, executor, 1).await;
    let journal = journal(harness, &request.job_id).await;
    assert!(
        matches!(
            journal.last(),
            Some(JournalEntry::Proposed { round: 1, .. })
        ),
        "{journal:#?}"
    );
    assert!(runs_started(&journal, EvalSplit::Validation).is_empty());
    let spent = spend_so_far(harness.access(), OWNER, &journal)
        .await
        .unwrap()
        .case_trials;
    assert_eq!(
        spent,
        (TRAIN_CASES.len() as u64) * u64::from(TRIALS_PER_CASE),
        "the train run completed before the stop"
    );
    (journal, spent)
}

async fn assert_nothing_after_drift(
    harness: &Harness,
    job_id: &str,
    before: Vec<JournalEntry>,
    spent: u64,
    failed: JobState,
) {
    let after = journal(harness, job_id).await;
    let mut expected = before;
    expected.push(JournalEntry::Finalized { state: failed });
    assert_eq!(after, expected);
    assert_eq!(
        spend_so_far(harness.access(), OWNER, &after)
            .await
            .unwrap()
            .case_trials,
        spent,
        "no trial ran after the drift"
    );
}

#[tokio::test]
async fn a_definition_deleted_after_the_freeze_fails_the_job_as_definition_changed() {
    let harness = Harness::new().await;
    let request = frozen_job(&harness, "definition-deleted").await;
    harness
        .launching
        .delete(Collection::EvalDefinition, DEFINITION)
        .await;
    let outcome = settle(
        &harness,
        &request,
        &base_executor(),
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(
        outcome.state,
        JobState::Failed {
            reason: "definition_changed".into()
        }
    );
}

#[tokio::test]
async fn a_definition_that_no_longer_validates_fails_the_job_as_definition_changed() {
    let harness = Harness::new().await;
    let request = frozen_job(&harness, "definition-invalid").await;
    harness
        .access()
        .transact_write("matrix.invalidate_definition", |txn| {
            Box::pin(async move {
                txn.execute(&format!(
                    r#"mutation {{ update_EvalDefinition(filter: {{ definition_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }} }}, input: {{ comparability_version: 0 }}) {{ _docID }} }}"#,
                    crate::graphql::escape_graphql_string(DEFINITION),
                    crate::graphql::escape_graphql_string(OWNER),
                ))
                .await
                .map(|_| ())
            })
        })
        .await
        .unwrap();
    let error = crate::optimization::driver::load_definition(harness.access(), OWNER, DEFINITION)
        .await
        .unwrap_err();
    assert!(
        crate::eval::runner::freeze_refused(&error).is_some(),
        "{error:#}"
    );
    let outcome = settle(
        &harness,
        &request,
        &base_executor(),
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(
        outcome.state,
        JobState::Failed {
            reason: "definition_changed".into()
        }
    );
}
