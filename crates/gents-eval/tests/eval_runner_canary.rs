use gents::support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(feature = "live-e2e")]
use gents::eval::Anchor;
use gents::eval::{load_trials, load_verdicts, OutcomeKind, ProviderReason, TrialRecord};
use gents::DocumentRuntimeOptions;
use gents_documents::apply_desired_state_plan;
use gents_documents::EvalSplit;
use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
#[cfg(feature = "live-e2e")]
use gents_documents::{InferenceBackend, InferenceProfile};
use gents_eval::checks::{verdict_detail, CheckRegistry};
use gents_eval::runner::embedded::{
    export_retained_transcript, EmbeddedExecutor, EmbeddedHome, TranscriptCalls, TranscriptOptions,
};
use gents_eval::runner::{
    freeze_refused, run, Capture, CellRequest, CellSource, RunOptions, RunRequest, TrialLocator,
};
#[cfg(feature = "live-e2e")]
use gents_eval::runner::{CaptureResult, TrialExecutor};
use gents_observe::observe::adapter_projection::ProjectionRedactionMode;
use gents_store::{collection::Collection, config_client::ConfigAccess};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamChunk, StreamPlan, StreamResponse,
};
const MODEL: &str = "canary-model";
const SOLO_MARKER: &str = "canary-solo";
const REPORT_MARKER: &str = "canary-report";
const CONFIRM_MARKER: &str = "canary-confirm";
const CANARY_ITEM_SDL: &str = "type CanaryItem {\n  item_id: String\n  label: String\n}\n";
#[cfg(feature = "live-e2e")]
const SEED_MARKER: &str = "Seeded item: wh-1";
const SEED_ITEM_SDL: &str = "type SeedItem {\n  label: String\n}\n";
const QUIET_ITEM_SDL: &str = "type QuietItem {\n  label: String\n}\n";

#[cfg(feature = "live-e2e")]
#[tokio::test]
async fn the_canary_runs_two_cases_end_to_end_on_an_embedded_home_with_live_inference() {
    let target = support::live_inference::live_target().await;
    let (canary, request) = live_canary_request(&target, "run-canary").await;
    let executor = canary.executor();

    let outcome = run(
        &canary.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();

    let trials = load_trials(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    assert_eq!(
        (
            outcome.completed,
            outcome.not_evidence,
            outcome.breaker_tripped
        ),
        (2, 0, false),
        "{trials:#?}"
    );
    assert_eq!(trials.len(), 2);
    assert!(trials.iter().all(|trial| trial.completion.is_some()));

    let verdicts = load_verdicts(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 3, "{verdicts:#?}");
    assert!(
        verdicts
            .iter()
            .all(|verdict| verdict.kind == OutcomeKind::Passed && verdict.score_bp == Some(10_000)),
        "{verdicts:#?}"
    );
    assert!(
        verdicts
            .iter()
            .all(|verdict| verdict.check == "captured_rows_count"
                && verdict.raw["reason_code"] == "in_range"
                && verdict.raw["count"] == 1),
        "{verdicts:#?}"
    );

    let two = trial(&trials, "two-stage");
    let completion = two.completion.clone().unwrap();
    assert_eq!(completion.anchor.requests, 2, "{completion:#?}");
    assert!(
        completion.usage.output_tokens.is_some(),
        "the live provider reports output usage: {:?}",
        completion.usage
    );
    let recollected = executor
        .recollect(&locator(two), &request.captures)
        .await
        .expect("the retained trial home is readable");
    let CaptureResult::Files {
        files,
        outside_workspace,
        ..
    } = &recollected.stages[0].captures["notes"]
    else {
        panic!(
            "expected a file capture: {:?}",
            recollected.stages[0].captures
        );
    };
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].path, "notes.txt");
    assert!(
        outside_workspace.is_empty(),
        "nothing in the trial's workspace pointed out of it: {outside_workspace:?}"
    );
    for (case_id, record) in evidence_records(&canary.runs_dir(), &request.run_id, &trials) {
        let live = &record["live"];
        let stages = if case_id == "two-stage" { 2 } else { 1 };
        assert_eq!(live["requests"], stages, "{case_id}: {record:#}");
        assert!(
            live["model_turns"].as_u64() >= Some(stages),
            "{case_id}: {record:#}"
        );
        assert!(live["output_tokens"].is_u64(), "{case_id}: {record:#}");
        assert!(
            live["documents"]["Agent"].as_u64() >= Some(1),
            "the pack's own behavior: {record:#}"
        );
        assert_eq!(live["captures"]["items"], 1, "{case_id}: {record:#}");
        assert_eq!(
            record["goal"],
            json!([{"collection": "CanaryItem", "capture": "items", "min": 1}]),
            "one entry however many stages count the capture: {record:#}"
        );
    }
    let view = gents_eval::runner::read_run_view(&canary.runs_dir().join(&request.run_id))
        .expect("the loop leaves its view beside the run");
    assert_eq!(view.trials.len(), 2);
    assert_eq!(view.report.cells[0].counts.pass, 2);
    let (again, repeat) = live_canary_request(&target, "run-canary").await;
    let repeat_executor = again.executor();
    run(
        &again.access,
        &repeat,
        &repeat_executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();
    let repeated = load_trials(&again.access, &repeat.owner, &repeat.run_id)
        .await
        .unwrap();
    assert_eq!(anchors(&trials), anchors(&repeated));
    for (run_id, runs_dir, trials) in [
        (&request.run_id, canary.runs_dir(), &trials),
        (&repeat.run_id, again.runs_dir(), &repeated),
    ] {
        let recorded = evidence_records(&runs_dir, run_id, trials);
        assert_eq!(
            recorded
                .iter()
                .map(|(case_id, record)| (case_id.clone(), record["anchor"].clone()))
                .collect::<Vec<_>>(),
            anchors(trials)
                .into_iter()
                .map(|(case_id, anchor)| (case_id, serde_json::to_value(anchor).unwrap()))
                .collect::<Vec<_>>(),
            "every trial records the anchor its completion did"
        );
        assert_ne!(
            recorded[0].1["evidence_digest"], recorded[1].1["evidence_digest"],
            "the digest follows what the trial observed: {recorded:#?}"
        );
    }
}
#[cfg(feature = "live-e2e")]
#[tokio::test]
async fn a_subject_profile_bound_to_an_execution_and_retry_policy_starts_its_trial() {
    let target = support::live_inference::live_target().await;
    let (canary, request) = live_canary_request(&target, "run-init-profile").await;
    let owner = canary.owner.clone();
    let mut profile = profile_document(&owner, "canary", "canary-backend", target.model());
    profile["execution_id"] = json!("canary-execution");
    canary
        .install(vec![
            (
                Collection::InferenceRetryPolicy,
                json!({
                    "node_did": owner,
                    "retry_policy_id": "canary-retry",
                    "max_transport_retries": 1,
                }),
            ),
            (
                Collection::InferenceExecution,
                json!({
                    "node_did": owner,
                    "execution_id": "canary-execution",
                    "display_name": "Default",
                    "retry_policy_id": "canary-retry",
                }),
            ),
            (Collection::InferenceProfile, profile),
        ])
        .await;
    let executor = canary.executor();

    let outcome = run(
        &canary.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();

    let trials = load_trials(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    assert_eq!(
        (outcome.completed, outcome.not_evidence),
        (2, 0),
        "{trials:#?}"
    );
    assert!(
        trials.iter().all(|trial| trial
            .completion
            .as_ref()
            .is_some_and(|completion| completion.anchor.requests > 0)),
        "{trials:#?}"
    );
}

#[cfg(feature = "live-e2e")]
#[tokio::test]
async fn a_seed_stage_fires_the_pack_trigger_and_observes_the_task_request() {
    seed_stage_routes_to_session(false).await;
}

#[cfg(feature = "live-e2e")]
#[tokio::test]
async fn a_seed_stage_routes_to_the_session_supplied_by_the_fixture() {
    seed_stage_routes_to_session(true).await;
}

#[cfg(feature = "live-e2e")]
async fn seed_stage_routes_to_session(existing_session: bool) {
    let target = support::live_inference::live_target().await;
    let (canary, mut request) = live_canary_request(&target, "run-seed").await;
    let mut definition = live_seed_definition_document(&canary.owner);
    if existing_session {
        definition["fixtures"]["documents"] = json!([{
            "collection": "AgentSession",
            "document": {
                "session_id": "supplied-destination",
                "node_did": "$trial",
                "requester_did": "$evaluator",
                "agent_did": "gents:agent-slot:seeded",
                "created_at": "2026-01-01T00:00:00Z"
            }
        }]);
    }
    canary
        .install(vec![(Collection::EvalDefinition, definition)])
        .await;
    request.definition_id = "seed".into();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents")
        .join("tests/fixtures/eval_runner/trigger_pack");
    let pack = tempfile::tempdir().unwrap();
    if existing_session {
        copy_tree(&source, pack.path());
        let path = pack.path().join("pack_config.json");
        let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        config["triggers"][0]["session_id_template"] = json!("supplied-destination");
        std::fs::write(path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    }
    request.cells[0].source = CellSource::Directory(if existing_session {
        pack.path().to_path_buf()
    } else {
        source
    });
    request.cells[0].agent_slot = "seeded".into();
    let executor = canary.executor();

    let outcome = run(
        &canary.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();

    let trials = load_trials(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    if outcome.not_evidence != 0 {
        let retained = canary
            .runs_dir()
            .join(locator(trial(&trials, "seeded")).home_hint.unwrap())
            .join("home");
        let home = EmbeddedHome::open_retained(&retained).await.unwrap();
        let state = gents_store::graphql::graphql_with_transaction_retry(
            &home.node,
            "{ Trigger { trigger_id last_status last_error } AgentRequest { request_id lifecycle_state failure_reason session_id requester_did } AgentSession { session_id node_did requester_did agent_did } }",
            "failed routing canary",
        ).await.unwrap();
        panic!("routing produced no evidence: {state:#?}");
    }
    assert_eq!(outcome.completed, 1);
    let completion = trial(&trials, "seeded").completion.clone().unwrap();
    assert_eq!(completion.anchor.requests, 1, "{completion:#?}");
    assert_eq!(
        completion.anchor.terminal_states,
        vec![gents_protocol::request_lifecycle::RequestLifecycleState::Completed],
        "{completion:#?}"
    );
    let verdicts = load_verdicts(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 1, "{verdicts:#?}");
    assert_eq!(
        (verdicts[0].kind, &verdicts[0].raw["count"]),
        (OutcomeKind::Passed, &json!(1)),
        "the fired request is the stage's evidence: {verdicts:#?}"
    );
    let seeded = trial(&trials, "seeded");
    let home_dir = canary
        .runs_dir()
        .join(locator(seeded).home_hint.expect("a retained home"))
        .join("home");
    let home = EmbeddedHome::open_retained(&home_dir).await.unwrap();
    // The trial's rows are document-owned; read them as the evaluator, the
    // trial's enrolled User, not as an anonymous node caller.
    let evaluator = ConfigAccess::local_as(
        home.node.clone(),
        ::identity::Did::new(&request.evaluator_did).unwrap(),
    );
    let fired = evaluator
        .execute(r#"{ AgentRequest(filter: { caused_by_trigger_id: { _eq: "seed-trigger" } }) { content } }"#)
        .await
        .unwrap();
    let fired_content = fired
        .get("data")
        .and_then(|data| data["AgentRequest"].as_array())
        .filter(|requests| requests.len() == 1)
        .and_then(|requests| requests[0]["content"].as_str())
        .unwrap_or_else(|| panic!("one fired request: {fired:#?}"));
    assert!(
        fired_content.starts_with("[event trigger:seed-trigger ")
            && fired_content.ends_with(&format!("]\n\n{SEED_MARKER}")),
        "the fired request carries the trigger source header and the seed body: {fired_content:?}"
    );
    if existing_session {
        let routed = evaluator
            .execute(r#"{ AgentRequest(filter: { caused_by_trigger_id: { _eq: "seed-trigger" } }) { session_id } AgentSession { node_did session_id created_at } }"#)
            .await
            .unwrap();
        let data = &routed["data"];
        assert_eq!(
            data["AgentRequest"],
            json!([{"session_id":"supplied-destination"}])
        );
        assert_eq!(
            data["AgentSession"],
            json!([{
                "node_did": home.did(),
                "session_id": "supplied-destination",
                "created_at": "2026-01-01T00:00:00Z"
            }]),
            "the request must reuse the fixture's session"
        );
    }
}
async fn setup_seed_request(
    endpoint: &str,
    run_id: &str,
    definition_id: &str,
    schemas: &[&str],
    stage: Value,
    pack_dir: PathBuf,
) -> (Canary, RunRequest) {
    let (canary, mut request) = canary_request(endpoint, run_id).await;
    canary
        .install(vec![(
            Collection::EvalDefinition,
            seed_definition_document(&canary.owner, definition_id, schemas, stage),
        )])
        .await;
    request.definition_id = definition_id.into();
    request.cells[0].source = CellSource::Directory(pack_dir);
    request.cells[0].agent_slot = "seeded".into();
    (canary, request)
}

#[tokio::test]
async fn a_seed_no_trigger_fires_for_fails_the_subject_and_says_why() {
    let backend = ScriptedBackend::start_with_plans(MODEL, Vec::new()).unwrap();
    let (canary, request) = setup_seed_request(
        backend.endpoint(),
        "run-seed-quiet",
        "seed-quiet",
        &[SEED_ITEM_SDL, QUIET_ITEM_SDL],
        json!({
            "stage_id": "fire",
            "seed": {"collection": "QuietItem", "document": {"label": "wh-2"}},
            "deadline_secs": 2,
            "capture": [fired_capture()],
            "checks": [fired_count(1, None)],
        }),
        trigger_pack_dir(),
    )
    .await;

    let (outcome, verdicts) = run_seed_case(&canary, &request).await;
    assert_eq!((outcome.completed, outcome.not_evidence), (1, 0));
    assert_eq!(verdicts.len(), 1, "{verdicts:#?}");
    assert_eq!(
        (verdicts[0].kind, verdicts[0].score_bp),
        (OutcomeKind::ModelAcceptance, Some(0)),
        "a seed nothing fired for in a running home is the subject's failure: {verdicts:#?}"
    );
    let detail = verdict_detail(&verdicts[0].raw).unwrap_or_default();
    assert!(
        detail.starts_with("no trigger fired a request within 2s of seeding QuietItem"),
        "{detail}"
    );
    assert_eq!(
        backend.observed_completion_bodies().len(),
        0,
        "no request reached the provider"
    );
}

#[tokio::test]
async fn a_seed_that_must_be_skipped_passes_once_the_home_is_quiet() {
    let backend = ScriptedBackend::start_with_plans(MODEL, Vec::new()).unwrap();
    let (canary, mut request) = setup_seed_request(
        backend.endpoint(),
        "run-seed-skipped",
        "seed-skipped",
        &[SEED_ITEM_SDL, QUIET_ITEM_SDL],
        json!({
            "stage_id": "skip",
            "seed": {"collection": "QuietItem", "document": {"label": "wh-3"}},
            "expect_fire": false,
            "deadline_secs": 180,
            "capture": [{"kind": "runs", "name": "runs", "of_seed": true}],
            "checks": [{
                "check": "captured_rows_count",
                "params": {"name": "runs", "min": 0, "max": 0},
                "tier": "acceptance",
            }],
        }),
        trigger_pack_dir(),
    )
    .await;
    request.deadline_secs = Some(300);

    let (outcome, verdicts) = run_seed_case(&canary, &request).await;
    assert_eq!((outcome.completed, outcome.not_evidence), (1, 0));
    assert_eq!(
        verdicts
            .iter()
            .map(|verdict| (verdict.kind, verdict.score_bp))
            .collect::<Vec<_>>(),
        [(OutcomeKind::Passed, Some(10_000))],
        "{verdicts:#?}"
    );
    assert_eq!(backend.observed_completion_bodies().len(), 0);
}

#[tokio::test]
async fn a_seed_that_must_be_skipped_fails_when_a_trigger_fires_for_it() {
    let backend = ScriptedBackend::start_with_plans(MODEL, Vec::new()).unwrap();
    let (canary, request) = setup_seed_request(
        backend.endpoint(),
        "run-seed-unskipped",
        "seed-unskipped",
        &[SEED_ITEM_SDL],
        json!({
            "stage_id": "skip",
            "seed": {"collection": "SeedItem", "document": {"label": "wh-4"}},
            "expect_fire": false,
            "deadline_secs": 180,
            "capture": [fired_capture()],
            "checks": [fired_count(0, Some(0))],
        }),
        trigger_pack_dir(),
    )
    .await;

    let (outcome, verdicts) = run_seed_case(&canary, &request).await;
    assert_eq!((outcome.completed, outcome.not_evidence), (1, 0));
    assert_eq!(
        (verdicts[0].kind, verdicts[0].score_bp),
        (OutcomeKind::ModelAcceptance, Some(0)),
        "{verdicts:#?}"
    );
    let detail = verdicts[0].raw["detail"].as_str().unwrap_or_default();
    assert!(
        detail.starts_with("trigger seed-trigger fired request ")
            && detail.ends_with(" for the SeedItem seed"),
        "{detail}"
    );
}

#[tokio::test]
async fn a_fire_its_task_template_fails_names_the_trigger_error_in_the_verdict() {
    let backend = ScriptedBackend::start_with_plans(MODEL, Vec::new()).unwrap();
    let pack = trigger_pack_with(|config| {
        config["tasks"][0]["prompt_template"] = json!("Seeded item: {{ doc.missing_field }}");
    });
    let (canary, request) = setup_seed_request(
        backend.endpoint(),
        "run-seed-broken",
        "seed-broken",
        &[SEED_ITEM_SDL],
        json!({
            "stage_id": "fire",
            "seed": {"collection": "SeedItem", "document": {"label": "wh-5"}},
            "deadline_secs": 120,
            "capture": [fired_capture()],
            "checks": [fired_count(1, None)],
        }),
        pack.path().to_path_buf(),
    )
    .await;

    let (outcome, verdicts) = run_seed_case(&canary, &request).await;
    assert_eq!((outcome.completed, outcome.not_evidence), (1, 0));
    assert_eq!(
        (verdicts[0].kind, verdicts[0].raw["reason_code"].as_str()),
        (OutcomeKind::Runtime, Some("stage_failed")),
        "{verdicts:#?}"
    );
    let detail = verdict_detail(&verdicts[0].raw).unwrap_or_default();
    assert!(
        detail.starts_with("trigger seed-trigger error: template: "),
        "eval trial shows the trigger's recorded error: {detail}"
    );
}

#[tokio::test]
async fn a_runs_capture_grades_the_reply_of_a_run_the_stage_did_not_start() {
    let backend = ScriptedBackend::start_with_plans(
        MODEL,
        vec![
            StreamPlan::new(
                "Seeded item: wh-6",
                vec![StreamResponse::completes(
                    "Seeded item: wh-6",
                    ["reported wh-6"],
                )],
            ),
            StreamPlan::new(
                "Echo item: wh-6",
                vec![StreamResponse::completes(
                    "Echo item: wh-6",
                    ["echoed wh-6"],
                )],
            ),
        ],
    )
    .unwrap();
    let pack = trigger_pack_with(|config| {
        config["tasks"].as_array_mut().unwrap().push(json!({
            "task_id": "echo-task",
            "display_name": "Echo a seeded item",
            "agent_did": "gents:agent-slot:seeded",
            "prompt_template": "Echo item: {{ doc.label }}"
        }));
        config["triggers"].as_array_mut().unwrap().push(json!({
            "trigger_id": "echo-trigger",
            "display_name": "echo-trigger",
            "task_id": "echo-task",
            "source": {"kind": "event", "event_source_id": "seed-source"},
            "concurrency": "serial"
        }));
    });
    let (canary, mut request) = setup_seed_request(
        backend.endpoint(),
        "run-seed-runs",
        "seed-runs",
        &[SEED_ITEM_SDL],
        json!({
            "stage_id": "fire",
            "seed": {"collection": "SeedItem", "document": {"label": "wh-6"}},
            "deadline_secs": 240,
            "settle": true,
            "capture": [
                {"kind": "runs", "name": "echo", "trigger_id": "echo-trigger"},
                {"kind": "runs", "name": "both", "source_collection": "SeedItem", "of_seed": true},
            ],
            "checks": [
                {
                    "check": "captured_fields_match",
                    "params": {"name": "echo", "min_rows": 1, "max_rows": 1, "expect": [
                        {"field": "final_message", "equals": "echoed wh-6"},
                        {"field": "lifecycle_state", "equals": "completed"},
                        {"field": "source_collection", "equals": "SeedItem"},
                        {"field": "session_id", "matches": ".+"},
                    ]},
                    "tier": "acceptance",
                },
                {
                    "check": "captured_rows_count",
                    "params": {"name": "both", "min": 2, "max": 2},
                    "tier": "acceptance",
                },
            ],
        }),
        pack.path().to_path_buf(),
    )
    .await;
    request.deadline_secs = Some(300);

    let (outcome, mut verdicts) = run_seed_case(&canary, &request).await;
    assert_eq!((outcome.completed, outcome.not_evidence), (1, 0));
    verdicts.sort_by(|left, right| left.check.cmp(&right.check));
    assert_eq!(
        verdicts
            .iter()
            .map(|verdict| (verdict.check.as_str(), verdict.kind, verdict.score_bp))
            .collect::<Vec<_>>(),
        [
            ("captured_fields_match", OutcomeKind::Passed, Some(10_000)),
            ("captured_rows_count", OutcomeKind::Passed, Some(10_000)),
        ],
        "{verdicts:#?}"
    );
}

#[tokio::test]
async fn a_seed_stage_whose_event_sources_do_not_reconcile_fails_at_once() {
    let backend = ScriptedBackend::start_with_plans(MODEL, Vec::new()).unwrap();
    let (canary, request) = setup_seed_request(
        backend.endpoint(),
        "run-seed-unreconciled",
        "seed-unreconciled",
        &[QUIET_ITEM_SDL],
        json!({
            "stage_id": "fire",
            "seed": {"collection": "QuietItem", "document": {"label": "wh-2"}},
            "deadline_secs": 2,
            "capture": [fired_capture()],
            "checks": [fired_count(1, None)],
        }),
        trigger_pack_dir(),
    )
    .await;
    let (outcome, verdicts) = run_seed_case(&canary, &request).await;
    assert_eq!((outcome.completed, outcome.not_evidence), (1, 1));

    let trials = load_trials(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    let trial = &trials[0];
    let completion = trial.completion.as_ref().unwrap();
    assert_eq!(completion.anchor.requests, 0);
    assert_eq!(completion.anchor.inference_calls, 0);
    assert_eq!(backend.observed_completion_bodies().len(), 0);
    let home = EmbeddedHome::open_retained(
        &canary
            .runs_dir()
            .join(trial.identity.home_hint.as_deref().unwrap())
            .join("home"),
    )
    .await
    .unwrap();
    let seeds = ConfigAccess::local(home.node.clone())
        .execute("{ QuietItem { _docID } }")
        .await
        .unwrap();
    assert_eq!(seeds["data"]["QuietItem"], json!([]), "{seeds:#?}");
    home.node.shutdown().await.expect("node shutdown");

    assert_eq!(verdicts.len(), 1, "{verdicts:#?}");
    assert_eq!(
        verdicts[0].kind,
        OutcomeKind::Infrastructure,
        "{verdicts:#?}"
    );
}

#[tokio::test]
async fn a_failing_first_stage_skips_the_second_and_grades_it_skipped_prerequisite() {
    let mut plans = answering_plans();
    plans[1] = StreamPlan::new(
        REPORT_MARKER,
        vec![
            StreamResponse::service_unavailable("HTTP status 503 for the canary report stage"),
            StreamResponse::service_unavailable("HTTP status 503 for the canary report stage"),
            StreamResponse::service_unavailable("HTTP status 503 for the canary report stage"),
        ],
    );
    let backend = ScriptedBackend::start_with_plans(MODEL, plans).unwrap();
    let (canary, request) = canary_request(backend.endpoint(), "run-provider-down").await;
    let executor = canary.executor();

    let outcome = run(
        &canary.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        (
            outcome.completed,
            outcome.not_evidence,
            outcome.breaker_tripped
        ),
        (2, 1, false),
        "both attempts finished; only the two-stage slot learned nothing"
    );

    let trials = load_trials(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    let two = trial(&trials, "two-stage");
    let completion = two.completion.clone().unwrap();
    assert_eq!(
        completion.anchor.requests, 1,
        "a failed first stage stops submission: {completion:#?}"
    );

    let verdicts = load_verdicts(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    let mut rows = verdicts
        .iter()
        .filter(|verdict| verdict.trial_id == two.identity.trial_id)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.stage_id.cmp(&right.stage_id));
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert_eq!(
        (
            rows[0].stage_id.as_str(),
            rows[0].kind,
            rows[0].provider_reason,
            rows[0].score_bp
        ),
        ("confirm", OutcomeKind::SkippedPrerequisite, None, Some(0)),
        "{rows:#?}"
    );
    assert_eq!(
        (
            rows[1].stage_id.as_str(),
            rows[1].kind,
            rows[1].provider_reason,
            rows[1].score_bp
        ),
        (
            "report",
            OutcomeKind::Provider,
            Some(ProviderReason::Unavailable),
            None
        ),
        "{rows:#?}"
    );
}

#[tokio::test]
async fn a_slot_whose_provider_dropped_its_streams_is_attempted_again() {
    let backend = ScriptedBackend::start_with_plans(MODEL, answering_plans()).unwrap();
    backend.hold(SOLO_MARKER).close();
    let (canary, mut request) = canary_request(backend.endpoint(), "run-dropped").await;
    request.case_ids = Some(vec!["one-stage".into()]);
    request.max_infra_retries = 2;
    let executor = canary.executor();
    let registry = CheckRegistry::builtin();
    let options = RunOptions::default();
    let mut trial_changes =
        gents_store::subscription_source::subscribe_collections(&canary._home.node, ["EvalTrial"])
            .unwrap();
    let provider_returns = async {
        loop {
            let trials = load_trials(&canary.access, &request.owner, &request.run_id)
                .await
                .unwrap();
            if trials.iter().any(|trial| trial.completion.is_some()) {
                backend.release(SOLO_MARKER);
                break;
            }
            trial_changes
                .recv()
                .await
                .expect("the trial change stream stays open");
        }
    };

    let (outcome, ()) = tokio::join!(
        run(
            &canary.access,
            &request,
            &executor,
            &registry,
            CancellationToken::new(),
            &options,
        ),
        provider_returns
    );
    let outcome = outcome.unwrap();

    let mut trials = load_trials(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    trials.sort_by_key(|trial| trial.identity.attempt);
    assert_eq!(
        (
            outcome.completed,
            outcome.not_evidence,
            outcome.breaker_tripped
        ),
        (2, 0, false),
        "{trials:#?}"
    );
    let stages = |trial: &TrialRecord| {
        trial
            .completion
            .as_ref()
            .unwrap()
            .stages
            .iter()
            .map(|stage| (stage.failure_kind, stage.provider_reason))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        trials.iter().map(stages).collect::<Vec<_>>(),
        [
            vec![(
                Some(OutcomeKind::Provider),
                Some(ProviderReason::Unavailable)
            )],
            vec![(None, None)],
        ],
        "{trials:#?}"
    );

    let report = gents_eval::report::load_report(
        &canary.access,
        &request.owner,
        &canary.runs_dir(),
        &request.run_id,
    )
    .await
    .unwrap();
    assert_eq!(
        report.cells[0]
            .slots
            .iter()
            .map(|slot| (slot.case_id.as_str(), slot.class, slot.attempts))
            .collect::<Vec<_>>(),
        [("one-stage", gents_eval::report::SlotClass::Pass, 2)],
        "the report counts the slot once, by its latest attempt"
    );
}

#[tokio::test]
async fn a_finished_trial_exports_the_bodies_its_provider_received_and_the_message_it_ended_on() {
    const REASONING: &str = "weighing what the canary should report";
    let mut plans = answering_plans();
    plans[1] = StreamPlan::new(
        REPORT_MARKER,
        vec![StreamResponse::streams(
            REPORT_MARKER,
            vec![
                StreamChunk::reasoning(REASONING),
                StreamChunk::text("reported"),
            ],
        )],
    );
    let backend = ScriptedBackend::start_with_plans(MODEL, plans).unwrap();
    let (canary, request) = canary_request(backend.endpoint(), "run-export").await;
    let outcome = run(
        &canary.access,
        &request,
        &canary.executor(),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, 2, "{outcome:#?}");
    let trials = load_trials(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    let two = trial(&trials, "two-stage");
    let completion = two.completion.clone().unwrap();
    let runtime = support::runtime_handle();
    let runs_dir = canary.runs_dir();
    let at = locator(two);
    let stage_requests = completion
        .stages
        .iter()
        .map(|stage| stage.request_id.clone().unwrap())
        .collect::<Vec<_>>();
    let export = |calls, redaction| {
        export_retained_transcript(
            &runtime,
            &runs_dir,
            &at,
            canary.evaluator.as_ref(),
            stage_requests.clone(),
            TranscriptOptions { calls, redaction },
        )
    };

    let full = export(TranscriptCalls::Final, ProjectionRedactionMode::Full)
        .await
        .unwrap();
    assert_eq!(
        (full.usage.input_tokens, full.usage.output_tokens),
        (
            completion.usage.input_tokens,
            completion.usage.output_tokens
        ),
        "the trial's calls sum as the run report sums them: {full:#?}"
    );
    let [report, confirm] = full.stages.as_slice() else {
        panic!("one transcript per stage: {full:#?}");
    };
    let observed = backend.observed_completion_bodies();
    for stage in [report, confirm] {
        let [call] = stage.calls.as_slice() else {
            panic!("the final call only: {stage:#?}");
        };
        assert!(
            observed.contains(&call.request),
            "an exported body is one the provider received: {call:#?}"
        );
        assert!(call.request["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()));
        assert_eq!(
            stage.usage.input_tokens,
            call.usage.as_ref().and_then(|usage| usage.input_tokens)
        );
    }
    let report_output = report.final_output.as_ref().unwrap();
    assert_eq!(report_output.reasoning.as_deref(), Some(REASONING));
    let confirm_history = confirm.calls[0].request["messages"].as_array().unwrap();
    assert!(
        !report_output.wire.is_empty()
            && report_output
                .wire
                .iter()
                .all(|item| confirm_history.contains(item)),
        "the next stage's provider saw the report stage's final message as rendered: {report_output:#?}"
    );
    assert!(!confirm.calls[0].request.to_string().contains(REASONING));
    let confirm_output = confirm.final_output.as_ref().unwrap();
    assert!(serde_json::to_string(&confirm_output.wire)
        .unwrap()
        .contains("confirmed"));

    let all = export(TranscriptCalls::All, ProjectionRedactionMode::Full)
        .await
        .unwrap();
    for (every, last) in all.stages.iter().zip(&full.stages) {
        assert_eq!(
            every.calls.last().map(|call| &call.request),
            Some(&last.calls[0].request)
        );
    }

    let redacted = export(
        TranscriptCalls::Final,
        ProjectionRedactionMode::TrainingSafe,
    )
    .await
    .unwrap();
    let roles = |messages: &Value| {
        messages
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["role"].clone())
            .collect::<Vec<_>>()
    };
    let redacted_history = &redacted.stages[1].calls[0].request["messages"];
    assert_eq!(
        roles(redacted_history),
        roles(&confirm.calls[0].request["messages"])
    );
    assert!(!redacted_history.to_string().contains(REPORT_MARKER));
    assert_eq!(
        redacted.stages[0]
            .final_output
            .as_ref()
            .and_then(|output| output.reasoning.as_deref()),
        Some("[training_safe_redacted]")
    );
}

#[tokio::test]
async fn a_multiturn_tool_calling_stage_exports_reasoning_for_every_assistant_turn() {
    const TURN_0_REASONING: &str = "first check which files exist in the canary workspace";
    const TURN_1_REASONING: &str = "notes.txt is present, so read it before answering";
    const TURN_1_TEXT: &str = "Reading notes.txt now.";
    const TURN_2_REASONING: &str = "summarize the file contents for the final report";
    const FINAL_TEXT: &str = "reported after inspecting notes.txt";

    let mut plans = answering_plans();
    plans[2] = StreamPlan::new(
        SOLO_MARKER,
        vec![
            StreamResponse::streams(
                SOLO_MARKER,
                vec![
                    StreamChunk::reasoning(TURN_0_REASONING),
                    StreamChunk::tool_call("call-list-1", "list_files", r#"{"path":"."}"#),
                ],
            ),
            StreamResponse::streams(
                SOLO_MARKER,
                vec![
                    StreamChunk::reasoning(TURN_1_REASONING),
                    StreamChunk::text(TURN_1_TEXT),
                    StreamChunk::tool_call("call-read-2", "read_file", r#"{"path":"notes.txt"}"#),
                ],
            ),
            StreamResponse::streams(
                SOLO_MARKER,
                vec![
                    StreamChunk::reasoning(TURN_2_REASONING),
                    StreamChunk::text(FINAL_TEXT),
                ],
            ),
        ],
    );
    let backend = ScriptedBackend::start_with_plans(MODEL, plans).unwrap();
    let (canary, mut request) = canary_request(backend.endpoint(), "run-export-multiturn").await;
    request.case_ids = Some(vec!["one-stage".into()]);
    let outcome = run(
        &canary.access,
        &request,
        &canary.executor(),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.completed, 1, "{outcome:#?}");

    let trials = load_trials(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    let solo = trial(&trials, "one-stage");
    let completion = solo.completion.clone().unwrap();
    let runtime = support::runtime_handle();
    let runs_dir = canary.runs_dir();
    let at = locator(solo);
    let stage_requests = completion
        .stages
        .iter()
        .map(|stage| stage.request_id.clone().unwrap())
        .collect::<Vec<_>>();
    let export = |calls, redaction| {
        export_retained_transcript(
            &runtime,
            &runs_dir,
            &at,
            canary.evaluator.as_ref(),
            stage_requests.clone(),
            TranscriptOptions { calls, redaction },
        )
    };

    let final_only = export(TranscriptCalls::Final, ProjectionRedactionMode::Full)
        .await
        .unwrap();
    let [stage] = final_only.stages.as_slice() else {
        panic!("one stage transcript: {final_only:#?}");
    };
    assert_eq!(stage.model_turns, 3, "{stage:#?}");
    assert_eq!(
        stage.calls.len(),
        1,
        "final mode exports one call: {stage:#?}"
    );
    assert_eq!(
        stage.outputs.len(),
        3,
        "every assistant turn is exported even in final-call mode: {stage:#?}"
    );
    assert_eq!(
        stage
            .outputs
            .iter()
            .map(|output| output.reasoning.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some(TURN_0_REASONING),
            Some(TURN_1_REASONING),
            Some(TURN_2_REASONING),
        ],
        "reasoning is preserved on both intermediate tool-call turns and the final turn: {stage:#?}"
    );
    assert_eq!(
        stage
            .outputs
            .iter()
            .map(|output| (output.turn_index, output.attempt))
            .collect::<Vec<_>>(),
        vec![(Some(0), Some(0)), (Some(1), Some(0)), (Some(2), Some(0))],
        "{stage:#?}"
    );
    assert!(
        stage
            .outputs
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence),
        "outputs stay ordered by session sequence: {stage:#?}"
    );
    assert!(
        stage
            .outputs
            .iter()
            .all(|output| output.call_id.is_some() && output.call_seq.is_some()),
        "each output carries its inference call join keys: {stage:#?}"
    );
    assert_eq!(
        stage.final_output.as_ref(),
        stage.outputs.last(),
        "final_output stays compatible with the last exported assistant output"
    );
    assert_eq!(
        stage.calls[0].output.as_ref(),
        stage.final_output.as_ref(),
        "the final call's output matches final_output"
    );

    let final_request_messages = stage.calls[0].request["messages"].as_array().unwrap();
    let intermediate_wire_assistants = final_request_messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .collect::<Vec<_>>();
    assert_eq!(
        intermediate_wire_assistants.len(),
        2,
        "the final request carries both intermediate assistant turns: {final_request_messages:#?}"
    );
    for (turn, (expected_wire, exported_output)) in intermediate_wire_assistants
        .iter()
        .zip(&stage.outputs[..2])
        .enumerate()
    {
        assert_eq!(
            exported_output.wire.as_slice(),
            std::slice::from_ref(*expected_wire),
            "intermediate turn {turn} wire matches the final request history verbatim"
        );
    }
    assert!(
        !stage.calls[0]
            .request
            .to_string()
            .contains(TURN_0_REASONING)
            && !stage.calls[0]
                .request
                .to_string()
                .contains(TURN_1_REASONING),
        "chat-completions request history never replays prior reasoning"
    );
    assert!(serde_json::to_string(&stage.outputs[1].wire)
        .unwrap()
        .contains(TURN_1_TEXT));
    assert!(serde_json::to_string(&stage.outputs[2].wire)
        .unwrap()
        .contains(FINAL_TEXT));

    let all_calls = export(TranscriptCalls::All, ProjectionRedactionMode::Full)
        .await
        .unwrap();
    let [all_stage] = all_calls.stages.as_slice() else {
        panic!("one stage transcript: {all_calls:#?}");
    };
    let inference_calls = all_stage
        .calls
        .iter()
        .filter(|call| call.capture_scope.as_deref() == Some("inference.1"))
        .collect::<Vec<_>>();
    assert_eq!(inference_calls.len(), 3, "{all_stage:#?}");
    for (call, expected_output) in inference_calls.iter().zip(&stage.outputs) {
        assert_eq!(
            call.output.as_ref(),
            Some(expected_output),
            "each provider call pairs with its exact assistant output: {call:#?}"
        );
        assert_eq!(call.call_id, expected_output.call_id);
        assert_eq!(call.call_seq, expected_output.call_seq);
    }

    let redacted = export(TranscriptCalls::All, ProjectionRedactionMode::TrainingSafe)
        .await
        .unwrap();
    let [redacted_stage] = redacted.stages.as_slice() else {
        panic!("one stage transcript: {redacted:#?}");
    };
    assert_eq!(
        redacted_stage
            .outputs
            .iter()
            .map(|output| output.reasoning.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("[training_safe_redacted]"),
            Some("[training_safe_redacted]"),
            Some("[training_safe_redacted]"),
        ],
        "training-safe redaction scrubs reasoning on every turn: {redacted_stage:#?}"
    );
    let redacted_json = serde_json::to_string(redacted_stage).unwrap();
    for secret in [
        TURN_0_REASONING,
        TURN_1_REASONING,
        TURN_1_TEXT,
        TURN_2_REASONING,
        FINAL_TEXT,
    ] {
        assert!(
            !redacted_json.contains(secret),
            "training-safe export leaked {secret:?}"
        );
    }
}

async fn run_responses_case(backend: &ScriptedBackend, run_id: &str, case_id: &str) -> Vec<Value> {
    let (canary, mut request) = canary_request(backend.endpoint(), run_id).await;
    let mut responses = backend_document(
        &canary.owner,
        "canary-backend",
        backend.endpoint(),
        json!({"kind": "unauthenticated"}),
    );
    responses["openai_wire_api"] = json!("responses");
    let mut profile = profile_document(&canary.owner, "canary", "canary-backend", MODEL);
    profile["reasoning_effort"] = json!("high");
    canary
        .install(vec![
            (Collection::InferenceBackend, responses),
            (Collection::InferenceProfile, profile),
        ])
        .await;
    request.case_ids = Some(vec![case_id.into()]);
    let outcome = run(
        &canary.access,
        &request,
        &canary.executor(),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!((outcome.completed, outcome.not_evidence), (1, 0));
    backend
        .recorded_requests()
        .into_iter()
        .filter(|request| request.path == "/v1/responses")
        .map(|request| serde_json::from_str::<Value>(&request.body).unwrap())
        .collect()
}

#[tokio::test]
async fn a_responses_backend_runs_its_trial_without_a_sampling_seed() {
    let backend = ScriptedBackend::start(MODEL, Vec::new()).unwrap();
    let bodies = run_responses_case(&backend, "run-responses", "one-stage").await;
    assert!(
        bodies.iter().all(|body| body.get("seed").is_none()),
        "{bodies:#?}"
    );
    let turns = bodies
        .into_iter()
        .filter(|body| body.get("tools").is_some())
        .collect::<Vec<_>>();
    assert!(!turns.is_empty(), "the trial's agent reached the provider");
    for mut body in turns {
        let fields = body.as_object_mut().unwrap();
        for content in ["input", "tools"] {
            fields.remove(content);
        }
        assert!(fields.remove("user").is_some_and(|user| user.is_string()));
        assert_eq!(
            body,
            json!({
                "model": MODEL,
                "max_output_tokens": gents_inference::resolved::DEFAULT_MAX_OUTPUT_TOKENS,
                "stream": true,
                "temperature": 0.0,
                "tool_choice": "auto",
                "reasoning": {"effort": "high"},
                "include": ["reasoning.encrypted_content"],
            }),
            "{body}"
        );
    }
}

#[tokio::test]
async fn a_responses_reasoning_backend_runs_a_second_request_in_the_session() {
    let backend = ScriptedBackend::builder(MODEL)
        .unscripted(gents_inference::test_support::scripted_backend::Unscripted::ReasoningToolTurn)
        .start()
        .unwrap();
    let inputs = run_responses_case(&backend, "run-responses-replay", "two-stage")
        .await
        .into_iter()
        .filter(|body| body.get("tools").is_some())
        .map(|body| body["input"].as_array().unwrap().clone())
        .collect::<Vec<_>>();
    assert_eq!(inputs.len(), 4, "two stages of two turns each: {inputs:#?}");
    let second_request = &inputs[2];
    let provider_items = second_request
        .iter()
        .filter(|item| item["role"] != "system" && item["role"] != "user")
        .map(|item| {
            let mut item = item.clone();
            if item["type"] == "function_call_output" {
                item["output"].take();
            }
            item
        })
        .collect::<Vec<_>>();
    let first_call = inputs[1]
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(
        provider_items,
        vec![
            json!({"type": "reasoning", "id": "rs_4", "summary": [],
                "encrypted_content": "sealed-4"}),
            first_call.clone(),
            json!({"type": "function_call_output", "call_id": "call_4", "output": null,
                "status": "completed"}),
            json!({"type": "reasoning", "id": "rs_5", "summary": [],
                "encrypted_content": "sealed-5"}),
            json!({"type": "message", "id": "msg_5", "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "mock response",
                    "annotations": []}]}),
        ],
        "the second request returns every item the first request's responses produced"
    );
    assert_eq!(second_request.last().unwrap()["role"], "user");
    assert_eq!(inputs[3][..second_request.len()], second_request[..]);
}

#[tokio::test]
async fn freeze_refuses_unrestricted_bash_and_an_oauth_backend_before_creating_any_home() {
    let (canary, base) = canary_request("http://127.0.0.1:1/v1", "run-refused").await;
    let executor = canary.executor();
    let registry = CheckRegistry::builtin();

    let unrestricted = canary.dirs.path().join("unrestricted-pack");
    write_pack_with_unrestricted_bash(&canary.pack_dir, &unrestricted);
    let mut bash = base.clone();
    bash.run_id = "run-bash".into();
    bash.cells[0].source = CellSource::Directory(unrestricted);
    let error = run(
        &canary.access,
        &bash,
        &executor,
        &registry,
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap_err();
    let refusal = freeze_refused(&error)
        .unwrap_or_else(|| panic!("expected a FreezeRefused: {error:#}"))
        .0
        .clone();
    assert!(
        refusal.contains("canary-tools") && refusal.contains("host bash"),
        "{refusal}"
    );

    canary
        .install(vec![
            (
                Collection::InferenceBackend,
                subscription_backend_document(&canary.owner, "canary-oauth-backend"),
            ),
            (
                Collection::InferenceProfile,
                profile_document(&canary.owner, "canary-oauth", "canary-oauth-backend", MODEL),
            ),
        ])
        .await;
    let mut oauth = base.clone();
    oauth.run_id = "run-oauth".into();
    oauth.cells[0].inference_profile_id = "canary-oauth".into();
    let error = run(
        &canary.access,
        &oauth,
        &executor,
        &registry,
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap_err();
    let refusal = freeze_refused(&error)
        .unwrap_or_else(|| panic!("expected a FreezeRefused: {error:#}"))
        .0
        .clone();
    assert!(refusal.contains("node_oauth"), "{refusal}");

    for run_id in ["run-bash", "run-oauth"] {
        assert!(
            !canary.runs_dir().join(run_id).exists(),
            "a refused request leaves no run directory behind: {run_id}"
        );
    }
}
#[cfg(feature = "live-e2e")]
#[tokio::test]
async fn live_paired_trials_preserve_the_seed_and_retain_both_answers() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "off,eval_runner_canary=info",
        ))
        .try_init();

    let target = support::live_inference::live_target().await;
    let (canary, mut request) = live_canary_request(&target, "run-live").await;
    canary
        .install(vec![(
            Collection::InferenceSampling,
            json!({
                "node_did": canary.owner,
                "sampling_id": "canary-sampling",
                "temperature": 0.7,
            }),
        )])
        .await;
    request.case_ids = Some(vec!["one-stage".to_string()]);
    request.trials_per_case = 1;
    let candidate = CellRequest {
        cell_id: "candidate".into(),
        label: "candidate".into(),
        ..request.cells[0].clone()
    };
    request.cells.push(candidate);

    let executor = canary.executor();
    let outcome = run(
        &canary.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();

    let trials = load_trials(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    assert_eq!(
        (outcome.completed, outcome.not_evidence),
        (2, 0),
        "{trials:#?}"
    );
    assert!(
        trials.iter().all(|trial| trial.completion.is_some()),
        "{trials:#?}"
    );
    assert_eq!(trials.len(), 2, "{trials:#?}");
    let mut arms = Vec::new();
    for cell_id in ["baseline", "candidate"] {
        let trial = trials
            .iter()
            .find(|trial| trial.identity.cell_id == cell_id)
            .unwrap_or_else(|| panic!("no {cell_id} trial in {trials:#?}"));
        let evidence = executor
            .recollect(&locator(trial), &[])
            .await
            .expect("the retained trial home is readable");
        let answer = evidence
            .stages
            .first()
            .and_then(|stage| stage.messages.first())
            .map(|message| message.content.clone())
            .unwrap_or_default();
        assert!(
            !answer.trim().is_empty(),
            "the {cell_id} arm answered nothing, so there is no finding to report"
        );
        arms.push((cell_id, trial.identity.seed, answer));
    }

    assert_eq!(
        arms[0].1, arms[1].1,
        "the two arms of a pair run at one seed"
    );
    tracing::info!(
        baseline_cell = arms[0].0,
        baseline_seed = arms[0].1,
        candidate_cell = arms[1].0,
        candidate_seed = arms[1].1,
        equal = arms[0].2 == arms[1].2,
        "seed honoured?"
    );
}
struct Canary {
    _home: EmbeddedHome,
    evaluator: Arc<gents_store::identity::KeyIdentity>,
    access: ConfigAccess,
    dirs: TempDir,
    owner: String,
    pack_dir: PathBuf,
}

impl Canary {
    /// Trial homes act as the held evaluator, the User that froze the run.
    fn executor(&self) -> EmbeddedExecutor {
        EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            self.runs_dir(),
            support::runtime_handle(),
        )
        .with_evaluator_identity(
            self.evaluator.clone(),
            self.evaluator.client_signing_capability().unwrap(),
        )
    }

    fn runs_dir(&self) -> PathBuf {
        self.dirs.path().join("eval/runs")
    }

    async fn install(&self, documents: Vec<(Collection, Value)>) {
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        self.access
            .transact_write("canary.install", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                    .map(|_| ())
                })
            })
            .await
            .unwrap();
    }
}
#[cfg(feature = "live-e2e")]
async fn live_canary_request(
    target: &support::live_inference::InferenceTarget,
    run_id: &str,
) -> (Canary, RunRequest) {
    let (canary, request) = canary_request(target.endpoint(), run_id).await;
    canary
        .install(vec![
            (
                Collection::InferenceBackend,
                serde_json::to_value(InferenceBackend {
                    backend_id: "canary-backend".into(),
                    ..target.backend(&canary.owner)
                })
                .unwrap(),
            ),
            (
                Collection::InferenceProfile,
                serde_json::to_value(InferenceProfile {
                    profile_id: "canary".into(),
                    backend_id: "canary-backend".into(),
                    sampling_id: Some("canary-sampling".into()),
                    execution_id: None,
                    ..target.profile(&canary.owner)
                })
                .unwrap(),
            ),
        ])
        .await;
    (canary, request)
}

async fn canary_request(endpoint: &str, run_id: &str) -> (Canary, RunRequest) {
    use gents_store::identity::NodeIdentity;

    support::init_test_tracing();
    let mut home = EmbeddedHome::create_temp("eval-canary").await.unwrap();
    let dirs = tempfile::tempdir().unwrap();
    let evaluator_path = dirs.path().join("evaluator.key");
    let evaluator = Arc::new(
        tokio::task::spawn_blocking(move || {
            gents_store::identity::KeyIdentity::load_or_create(evaluator_path, None)
        })
        .await
        .unwrap()
        .unwrap(),
    );
    let evaluator_did = evaluator.did().to_owned();
    let capability = evaluator.client_signing_capability().unwrap();
    home.set_evaluator(evaluator.clone(), capability);
    let admin =
        ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(home.did()).unwrap());
    let evaluator_actor = ::identity::Did::new(&evaluator_did).unwrap();
    // The evaluator is an enrolled User of the controller home.
    gents_store::identity::GrantSet::OperatorUser
        .grant(&admin, &evaluator_actor)
        .await
        .unwrap();
    let access = home.client_access(&evaluator_did).unwrap();
    let owner = home.did().to_string();
    let canary = Canary {
        access,
        owner: owner.clone(),
        pack_dir: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gents")
            .join("tests/fixtures/eval_runner/canary_pack"),
        dirs,
        evaluator,
        _home: home,
    };
    canary
        .install(vec![
            (Collection::Node, json!({"node_did": owner})),
            (Collection::EvalDefinition, definition_document(&owner)),
            (
                Collection::InferenceBackend,
                backend_document(
                    &owner,
                    "canary-backend",
                    endpoint,
                    json!({"kind": "unauthenticated"}),
                ),
            ),
            (
                Collection::InferenceSampling,
                json!({
                    "node_did": owner,
                    "sampling_id": "canary-sampling",
                    "temperature": 0.0,
                }),
            ),
            (
                Collection::InferenceProfile,
                profile_document(&owner, "canary", "canary-backend", MODEL),
            ),
        ])
        .await;

    let request = RunRequest {
        run_id: run_id.to_string(),
        evaluator_did,
        owner,
        definition_id: "canary".into(),
        split: EvalSplit::Validation,
        case_ids: None,
        cells: vec![CellRequest {
            cell_id: "baseline".into(),
            label: "baseline".into(),
            source: CellSource::Directory(canary.pack_dir.clone()),
            agent_slot: "canary".into(),
            inference_profile_id: "canary".into(),
        }],
        trials_per_case: 1,
        seed_base: 1000,
        deadline_secs: Some(120),
        concurrency: 1,
        max_infra_retries: 0,
        breaker_threshold: 10,
        purpose: "eval".into(),
        source_commit: "canary".into(),
        source_dirty: false,
        captures: vec![
            Capture::Documents {
                name: "items".into(),
                collection: "CanaryItem".into(),
                filter: json!({}),
                fields: Vec::new(),
            },
            Capture::File {
                name: "notes".into(),
                glob: "**/*.txt".into(),
                text_paths: Default::default(),
            },
        ],
        runs_dir: canary.runs_dir(),
        divergences: None,
    };
    (canary, request)
}
fn definition_document(owner: &str) -> Value {
    json!({
        "definition_id": "canary",
        "node_did": owner,
        "comparability_version": 1,
        "title": "Eval runner canary",
        "subject": {"kind": "agent", "inference_slots": ["primary"]},
        "fixtures": {
            "assets": ["notes.txt"],
            "schemas": [CANARY_ITEM_SDL],
            "documents": [{
                "collection": "CanaryItem",
                "document": {"item_id": "seeded", "label": "the row every case starts from"},
            }],
        },
        "cases": [
            {
                "case_id": "one-stage",
                "split": "validation",
                "stages": [stage_document("report", SOLO_MARKER)],
            },
            {
                "case_id": "two-stage",
                "split": "validation",
                "stages": [
                    stage_document("report", REPORT_MARKER),
                    stage_document("confirm", CONFIRM_MARKER),
                ],
            },
        ],
    })
}
fn seed_definition_document(
    owner: &str,
    definition_id: &str,
    schemas: &[&str],
    stage: Value,
) -> Value {
    json!({
        "definition_id": definition_id,
        "node_did": owner,
        "comparability_version": 1,
        "title": "Eval runner seed stage",
        "subject": {"kind": "agent", "inference_slots": ["primary"]},
        "fixtures": {"schemas": schemas},
        "cases": [{"case_id": "seeded", "split": "validation", "stages": [stage]}],
    })
}

fn fired_capture() -> Value {
    json!({
        "kind": "documents",
        "name": "fired",
        "collection": "AgentRequest",
        "filter": {},
        "fields": ["content"]
    })
}

fn fired_count(min: u64, max: Option<u64>) -> Value {
    json!({
        "check": "captured_rows_count",
        "params": {"name": "fired", "min": min, "max": max},
        "tier": "acceptance",
    })
}

fn trigger_pack_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents")
        .join("tests/fixtures/eval_runner/trigger_pack")
}

fn trigger_pack_with(edit: impl FnOnce(&mut Value)) -> TempDir {
    let pack = tempfile::tempdir().unwrap();
    copy_tree(&trigger_pack_dir(), pack.path());
    let path = pack.path().join("pack_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut config);
    std::fs::write(path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    pack
}

async fn run_seed_case(
    canary: &Canary,
    request: &RunRequest,
) -> (
    gents_eval::runner::RunOutcome,
    Vec<gents::eval::VerdictRecord>,
) {
    let outcome = run(
        &canary.access,
        request,
        &canary.executor(),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();
    let verdicts = load_verdicts(&canary.access, &request.owner, &request.run_id)
        .await
        .unwrap();
    (outcome, verdicts)
}

#[cfg(feature = "live-e2e")]
fn live_seed_definition_document(owner: &str) -> Value {
    json!({
        "definition_id": "seed",
        "node_did": owner,
        "comparability_version": 1,
        "title": "Eval runner seed stage",
        "subject": {"kind": "agent", "inference_slots": ["primary"]},
        "fixtures": {"schemas": [SEED_ITEM_SDL]},
        "cases": [{
            "case_id": "seeded",
            "split": "validation",
            "stages": [{
                "stage_id": "fire",
                "seed": {"collection": "SeedItem", "document": {"label": "wh-1"}},
                "deadline_secs": 120,
                "capture": [{
                    "kind": "documents",
                    "name": "fired",
                    "collection": "AgentRequest",
                    "filter": {"caused_by_trigger_id": {"_eq": "seed-trigger"}},
                    "fields": ["content"]
                }],
                "checks": [{
                    "check": "captured_rows_count",
                    "params": {"name": "fired", "min": 1},
                    "tier": "acceptance",
                }],
            }],
        }],
    })
}

fn stage_document(stage_id: &str, marker: &str) -> Value {
    json!({
        "stage_id": stage_id,
        "prompt": format!("Say one sentence about the canary. Marker: {marker}."),
        "deadline_secs": 120,
        "checks": [{
            "check": "captured_rows_count",
            "params": {"name": "items", "min": 1},
            "tier": "acceptance",
        }],
    })
}

fn backend_document(owner: &str, backend_id: &str, endpoint: &str, auth: Value) -> Value {
    json!({
        "node_did": owner,
        "backend_id": backend_id,
        "name": "Eval canary backend",
        "provider_kind": "OpenAiCompatible",
        "openai_wire_api": "chat_completions",
        "endpoint": endpoint,
        "auth": auth,
        "max_concurrent": 4,
        "max_queue_depth": 100,
    })
}
fn subscription_backend_document(owner: &str, backend_id: &str) -> Value {
    json!({
        "node_did": owner,
        "backend_id": backend_id,
        "name": "Eval canary subscription backend",
        "provider_kind": "ClaudeCliSubscription",
        "endpoint": "https://api.anthropic.com",
        "auth": {"kind": "node_oauth"},
    })
}

fn profile_document(owner: &str, profile_id: &str, backend_id: &str, model: &str) -> Value {
    json!({
        "node_did": owner,
        "profile_id": profile_id,
        "backend_id": backend_id,
        "model_name": model,
        "sampling_id": "canary-sampling",
    })
}
fn answering_plans() -> Vec<StreamPlan> {
    vec![
        StreamPlan::new(
            CONFIRM_MARKER,
            vec![StreamResponse::completes(CONFIRM_MARKER, ["confirmed"])],
        ),
        StreamPlan::new(
            REPORT_MARKER,
            vec![StreamResponse::completes(REPORT_MARKER, ["reported"])],
        ),
        StreamPlan::new(
            SOLO_MARKER,
            vec![StreamResponse::completes(SOLO_MARKER, ["reported"])],
        ),
    ]
}

fn trial<'a>(trials: &'a [TrialRecord], case_id: &str) -> &'a TrialRecord {
    trials
        .iter()
        .find(|trial| trial.identity.case_id == case_id)
        .unwrap_or_else(|| panic!("no {case_id} trial in {trials:#?}"))
}

fn locator(trial: &TrialRecord) -> TrialLocator {
    TrialLocator {
        trial_node_did: trial.identity.trial_node_did.clone(),
        session_id: trial.identity.session_id.clone(),
        home_hint: trial.identity.home_hint.clone(),
    }
}
#[cfg(feature = "live-e2e")]
fn anchors(trials: &[TrialRecord]) -> Vec<(String, Anchor)> {
    let mut anchors = trials
        .iter()
        .map(|trial| {
            (
                trial.identity.case_id.clone(),
                trial
                    .completion
                    .as_ref()
                    .unwrap_or_else(|| panic!("an unfinished trial: {trial:#?}"))
                    .anchor
                    .clone(),
            )
        })
        .collect::<Vec<_>>();
    anchors.sort_by(|left, right| left.0.cmp(&right.0));
    anchors
}
#[cfg(feature = "live-e2e")]
fn evidence_records(runs_dir: &Path, run_id: &str, trials: &[TrialRecord]) -> Vec<(String, Value)> {
    let mut records = trials
        .iter()
        .map(|trial| {
            let path = runs_dir
                .join(run_id)
                .join("trials")
                .join(&trial.identity.trial_id)
                .join("evidence.json");
            let recorded: Value = serde_json::from_slice(
                &std::fs::read(&path)
                    .unwrap_or_else(|error| panic!("reading {}: {error}", path.display())),
            )
            .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()));
            let digest = recorded["evidence_digest"].as_str().unwrap_or_default();
            assert!(
                digest.len() == 64 && digest.chars().all(|byte| byte.is_ascii_hexdigit()),
                "{}: {recorded}",
                path.display()
            );
            (trial.identity.case_id.clone(), recorded)
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| left.0.cmp(&right.0));
    records
}
fn write_pack_with_unrestricted_bash(source: &Path, destination: &Path) {
    copy_tree(source, destination);
    let path = destination.join("pack_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["tools"][0]["host"]["bash"] = json!({"mode": "Unrestricted"});
    std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}
