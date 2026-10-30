mod decisions;
mod doubles;
mod drift;
pub(crate) mod fixtures;
mod journal;
mod resume;

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use self::doubles::CancellingProposer;
use self::fixtures::{
    base_executor, behavior, budgets, context, definition, fail, policy, repeating_proposer,
    script, tools, BASELINE_PROMPT, CANDIDATE_PROMPT, DEFINITION, FEEDBACK_DEFINITION,
    SMALL_DEFINITION, TRIALS_PER_CASE, VALIDATION_CASES,
};
use self::journal::runs_started;
use crate::config_client::ConfigAccess;
use crate::document_config::EvalSplit;
use crate::eval::checks::CheckRegistry;
use crate::eval::runner::freeze::tests::{Launching, OWNER};
use crate::eval::runner::{Capture, RunOptions, ScriptedExecutor, TrialExecutor};
use crate::optimization::driver::{run_job, JobOutcome, JobRequest};
use crate::optimization::job::{load_job, Budgets, JobState, JournalEntry};
use crate::optimization::proposer::Proposer;
use crate::optimization::target::JobTarget;
use crate::Collection;

pub(crate) struct Harness {
    launching: Launching,
    jobs_dir: PathBuf,
    pack: PathBuf,
}

impl Harness {
    pub(crate) async fn new() -> Self {
        let launching = Launching::new().await;
        launching
            .install(vec![
                (
                    Collection::EvalDefinition,
                    definition(DEFINITION, "captured_rows_count", &VALIDATION_CASES, 1),
                ),
                (
                    Collection::EvalDefinition,
                    definition(
                        SMALL_DEFINITION,
                        "captured_rows_count",
                        &VALIDATION_CASES[..5],
                        1,
                    ),
                ),
                (
                    Collection::EvalDefinition,
                    definition(FEEDBACK_DEFINITION, "feedback_check", &VALIDATION_CASES, 1),
                ),
                tools(),
                context(BASELINE_PROMPT),
                behavior("Monitor"),
            ])
            .await;
        let pack = launching.pack("subject", "Off");
        let jobs_dir = launching
            .runs_dir()
            .parent()
            .expect("runs_dir is <scratch>/eval/runs")
            .join("jobs");
        Self {
            launching,
            jobs_dir,
            pack,
        }
    }

    pub(crate) fn access(&self) -> &ConfigAccess {
        &self.launching.access
    }

    pub(crate) async fn install(&self, documents: Vec<(Collection, Value)>) {
        self.launching.install(documents).await;
    }

    pub(crate) async fn delete(&self, collection: Collection, id: &str) {
        self.launching.delete(collection, id).await;
    }

    pub(crate) fn request(
        &self,
        job_id: &str,
        definition_id: &str,
        budgets: Budgets,
    ) -> JobRequest {
        JobRequest {
            job_id: job_id.into(),
            owner: OWNER.into(),
            evaluator_did: self.launching.evaluator_did(),
            agent_id: "monitor".into(),
            target: JobTarget::Context,
            definition_id: definition_id.into(),
            inference_profile_id: "local".into(),
            baseline_pack: self.pack.clone(),
            trials_per_case: TRIALS_PER_CASE,
            budgets,
            max_text_bytes: 32 * 1024,
            seed_base: 1_000,
            jobs_dir: self.jobs_dir.clone(),
            runs_dir: self.launching.runs_dir(),
            source_commit: "matrix".into(),
            source_dirty: false,
            concurrency: 1,
            max_infra_retries: 0,
            breaker_threshold: 100,
            deadline_secs: Some(600),
            captures: vec![findings_capture()],
            run_options: RunOptions {
                poll_backoff_base: std::time::Duration::from_millis(1),
                poll_backoff_cap: std::time::Duration::from_millis(2),
                trial_budget: None,
            },
        }
    }
}

pub(crate) fn findings_capture() -> Capture {
    Capture::Documents {
        name: "findings".into(),
        collection: "ExperimentFinding".into(),
        filter: json!({}),
        fields: vec!["finding_id".into()],
    }
}

pub(crate) async fn drive(
    harness: &Harness,
    request: &JobRequest,
    executor: &dyn TrialExecutor,
    proposer: &dyn Proposer,
    registry: &CheckRegistry,
    cancel: CancellationToken,
) -> Result<JobOutcome> {
    run_job(
        harness.access(),
        request,
        executor,
        proposer,
        registry,
        &policy(),
        cancel,
    )
    .await
}

pub(crate) async fn settle(
    harness: &Harness,
    request: &JobRequest,
    executor: &ScriptedExecutor,
    proposer: &dyn Proposer,
) -> JobOutcome {
    drive(
        harness,
        request,
        executor,
        proposer,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
    )
    .await
    .unwrap()
}

pub(crate) async fn journal(harness: &Harness, job_id: &str) -> Vec<JournalEntry> {
    load_job(harness.access(), OWNER, job_id)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no job {job_id}"))
        .journal
}

async fn cancel_while_proposing(
    harness: &Harness,
    request: &JobRequest,
    executor: &ScriptedExecutor,
    at: u32,
) -> JobOutcome {
    let token = CancellationToken::new();
    let proposer = CancellingProposer {
        inner: repeating_proposer(CANDIDATE_PROMPT),
        at,
        token: token.clone(),
    };
    let stopped = drive(
        harness,
        request,
        executor,
        &proposer,
        &CheckRegistry::builtin(),
        token,
    )
    .await
    .unwrap();
    assert_eq!(stopped.state, JobState::Running, "{stopped:#?}");
    stopped
}

pub(crate) async fn accepting_harness(job_id: &str) -> (Harness, JobRequest) {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request(job_id, DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::ReadyToPromote, "{outcome:#?}");
    (harness, request)
}

pub(crate) const CANDIDATE_TEMPLATE: &str = "Do {{ doc.goal }} for {{ doc.owner }}, and say why.\n";

async fn task_harness() -> (Harness, PathBuf) {
    use crate::optimization::subject::tests::{write_task_fixture_pack, FIXTURE_TEMPLATE};
    let harness = Harness::new().await;
    harness
        .install(vec![
            (
                Collection::Task,
                json!({
                    "task_id": "plan",
                    "node_did": OWNER,
                    "agent_id": "monitor",
                    "prompt_template": FIXTURE_TEMPLATE,
                }),
            ),
            (
                Collection::EventSource,
                json!({
                    "event_source_id": "plan-source",
                    "node_did": OWNER,
                    "source_collection": "PlanItem",
                }),
            ),
            (
                Collection::Trigger,
                json!({
                    "trigger_id": "plan-trigger",
                    "node_did": OWNER,
                    "task_id": "plan",
                    "source": {"kind": "event", "event_source_id": "plan-source"},
                }),
            ),
        ])
        .await;
    let pack = harness.jobs_dir.parent().unwrap().join("task-subject");
    write_task_fixture_pack(&pack, false);
    (harness, pack)
}

async fn install_task_definition(harness: &Harness, seeded_splits: &[&str]) {
    let mut value = definition(DEFINITION, "captured_rows_count", &VALIDATION_CASES, 1);
    for case in value["cases"].as_array_mut().unwrap() {
        if seeded_splits.contains(&case["split"].as_str().unwrap()) {
            let stage = &mut case["stages"][0];
            stage["prompt"] = json!("");
            stage["seed"] = json!({
                "collection": "PlanItem", "document": {"goal": "plan work", "owner": "operator"},
            });
        }
    }
    harness
        .install(vec![(Collection::EvalDefinition, value)])
        .await;
}

#[tokio::test]
async fn an_unexercised_task_target_is_refused_before_any_job_or_trial_is_created() {
    let (harness, pack) = task_harness().await;
    let rows = &crate::lean_vocab_test::lean_optimization_cases().seed_coverage;
    let scenarios = rows.iter().filter(|row| {
        matches!(
            row.name.as_str(),
            "task_prompt_only"
                | "only_train_seeds"
                | "only_validation_seeds"
                | "only_held_out_seeds"
        )
    });
    assert_eq!(scenarios.clone().count(), 4);
    for row in scenarios {
        let seeded_splits: Vec<&str> = row
            .cases
            .iter()
            .filter(|case| !case.seed_collections.is_empty())
            .map(|case| case.split.as_str())
            .collect();
        install_task_definition(&harness, &seeded_splits).await;
        let request = JobRequest {
            baseline_pack: pack.clone(),
            target: JobTarget::Task("plan".into()),
            ..harness.request(&row.name, DEFINITION, budgets(1_000))
        };
        let executor = base_executor();
        let proposer = repeating_proposer(CANDIDATE_TEMPLATE);
        let error = drive(
            &harness,
            &request,
            &executor,
            &proposer,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let reason = &super::job_refused(&error)
            .unwrap_or_else(|| panic!("{error:#}"))
            .0;
        assert!(reason.contains("task target \"plan\""), "{reason}");
        assert!(
            reason.contains(&format!(
                "in the {} split",
                row.missing_splits.join(" and ")
            )),
            "{}: {reason}",
            row.name,
        );
        assert!(executor.calls.lock().unwrap().is_empty());
        assert!(proposer.calls.lock().unwrap().is_empty());
        assert!(load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .is_none());
        assert!(!super::job_dir(&request.jobs_dir, &request.job_id).exists());
    }
    let rows = harness
        .access()
        .execute("{ EvalRun { run_id } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["EvalRun"], json!([]));
}

#[tokio::test]
async fn exact_seed_failures_reject_candidates_before_validation() {
    for row in &crate::lean_vocab_test::lean_optimization_cases().seed_rendering {
        if !matches!(
            row.name.as_str(),
            "missing_added_field"
                | "nested_missing_field"
                | "one_of_multiple_seeds_fails"
                | "added_field_present_in_every_seed"
                | "guarded_absent_field_renders"
                | "runtime_document_id_is_available"
                | "trial_owner_binding_reaches_the_guarded_missing_field"
        ) {
            continue;
        }
        let (harness, pack) = task_harness().await;
        let mut value = definition(DEFINITION, "captured_rows_count", &VALIDATION_CASES, 1);
        for case in value["cases"].as_array_mut().unwrap() {
            let stage = &mut case["stages"][0];
            stage["prompt"] = json!("");
            stage["seed"] = json!({"collection":"PlanItem", "document":row.seeds[0].document});
        }
        for (index, seed) in row.seeds.iter().enumerate() {
            value["cases"][1 + index]["stages"][0]["seed"]["document"] = seed.document.clone();
        }
        harness
            .install(vec![(Collection::EvalDefinition, value)])
            .await;
        let request = JobRequest {
            baseline_pack: pack,
            target: JobTarget::Task("plan".into()),
            ..harness.request(&row.name, DEFINITION, budgets(1_000))
        };
        let executor = base_executor();
        let proposer = repeating_proposer(&row.text);
        settle(&harness, &request, &executor, &proposer).await;
        let entries = journal(&harness, &request.job_id).await;
        let failed = !row.failed_stages.is_empty();
        assert_eq!(
            runs_started(&entries, EvalSplit::Validation).is_empty(),
            failed,
            "{}",
            row.name
        );
        let diagnostics: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                JournalEntry::StructuralReject { diagnostics, .. }
                    if diagnostics.starts_with("template_seed_invalid:") =>
                {
                    Some(diagnostics)
                }
                _ => None,
            })
            .collect();
        assert_eq!(!diagnostics.is_empty(), failed, "{}", row.name);
        if failed {
            assert!(diagnostics
                .iter()
                .all(|detail| detail.contains("case") && detail.contains("stage")));
            for input in proposer.calls.lock().unwrap().iter() {
                for rejection in &input.rejections {
                    assert!(!rejection.reason.contains("val-"), "{}", rejection.reason);
                    assert!(!rejection.reason.contains("check"), "{}", rejection.reason);
                }
            }
        }
    }
}

pub(crate) async fn accepting_task_harness(job_id: &str) -> (Harness, JobRequest) {
    let (harness, pack) = task_harness().await;
    install_task_definition(&harness, &["train", "validation", "held_out"]).await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = JobRequest {
        baseline_pack: pack,
        target: JobTarget::Task("plan".into()),
        ..harness.request(job_id, DEFINITION, budgets(1_000))
    };
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_TEMPLATE),
    )
    .await;
    assert_eq!(outcome.state, JobState::ReadyToPromote, "{outcome:#?}");
    (harness, request)
}

pub(crate) async fn rejecting_harness(job_id: &str) -> (Harness, JobRequest) {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES[..3], |_| {
        fail()
    });
    let request = harness.request(job_id, DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote, "{outcome:#?}");
    (harness, request)
}

pub(crate) async fn frozen_job(harness: &Harness, job_id: &str) -> JobRequest {
    let request = harness.request(job_id, DEFINITION, budgets(1_000));
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let first = drive(
        harness,
        &request,
        &base_executor(),
        &repeating_proposer(CANDIDATE_PROMPT),
        &CheckRegistry::builtin(),
        cancelled,
    )
    .await
    .unwrap();
    assert_eq!(first.state, JobState::Running);
    request
}
