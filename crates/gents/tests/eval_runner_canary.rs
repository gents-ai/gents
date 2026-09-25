//! The M2 eval-runner canary: two cases run end to end on real embedded trial
//! homes against a scripted model, and the run's refusals are asked at the
//! door rather than after a home exists.
//!
//! This is the acceptance test for the runner stack. Nothing here mocks the
//! runner: `run` freezes the request, the embedded executor creates one
//! DefraDB home per trial, installs the frozen pack and inference binding,
//! boots a runtime, submits one request per stage and reads the captures back
//! out. Only the provider is scripted. A failure here is a defect in the
//! stack, not in this file.

mod support;

use std::path::{Path, PathBuf};

use gents::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::{EvalSplit, InferenceBackend, InferenceProfile};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::{EmbeddedExecutor, EmbeddedHome};
use gents::eval::runner::{
    freeze_refused, run, Capture, CaptureResult, CellRequest, CellSource, RunOptions, RunRequest,
    TrialExecutor, TrialLocator,
};
use gents::eval::{load_trials, load_verdicts, Anchor, OutcomeKind, ProviderReason, TrialRecord};
use gents::{Collection, ConfigAccess, DocumentRuntimeOptions};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use support::streaming_backend::{MockStreamingBackend, StreamPlan, StreamResponse};

/// The model name the scripted backend advertises and the canary profile names.
const MODEL: &str = "canary-model";

/// One marker per stage prompt, so the scripted backend can answer each stage
/// differently. A later stage of the same case carries the earlier stage's
/// prompt in its conversation history, so the plans are ordered from the last
/// stage to the first: [`MockStreamingBackend`] takes the first plan whose
/// marker the request body contains.
const SOLO_MARKER: &str = "canary-solo";
const REPORT_MARKER: &str = "canary-report";
const CONFIRM_MARKER: &str = "canary-confirm";

/// The app collection a case's fixtures seed and the `items` capture reads.
const CANARY_ITEM_SDL: &str = "type CanaryItem {\n  item_id: String\n  label: String\n}\n";

#[tokio::test]
async fn the_canary_runs_two_cases_end_to_end_on_an_embedded_home_with_a_scripted_model() {
    let backend = MockStreamingBackend::start_with_plans(MODEL, answering_plans()).unwrap();
    let (canary, request) = canary_request(backend.endpoint(), "run-canary").await;
    let executor = EmbeddedExecutor::new(DocumentRuntimeOptions::default(), canary.runs_dir());

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
    // The verdicts are evidence-derived: every one of them is the seeded
    // `items` capture counted by the check the case named.
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
        "the scripted backend reports usage in its final chunk: {:?}",
        completion.usage
    );

    // The trial home outlived the run, so the file capture can be read again
    // out of the retained home rather than only out of the evidence.
    let recollected = executor
        .recollect(&locator(two), &request.captures)
        .await
        .expect("the retained trial home is readable");
    let CaptureResult::Files {
        files,
        outside_workspace,
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

    // An identical second run in a fresh launching home anchors the same way.
    let (again, repeat) = canary_request(backend.endpoint(), "run-canary").await;
    let repeat_executor =
        EmbeddedExecutor::new(DocumentRuntimeOptions::default(), again.runs_dir());
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

    // `TrialCompletion` carries no evidence digest, so the loop records each
    // trial's own beside its home. The digest covers the trial's request ids
    // and its messages' timestamps, both new in every run, so two runs of one
    // case digest differently by construction. What has to agree across runs
    // is the anchor the digest was taken over; what the digest itself pins is
    // that it follows the evidence, so two trials of one run differ in it.
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

#[tokio::test]
async fn a_failing_first_stage_skips_the_second_and_grades_it_skipped_prerequisite() {
    // The report stage answers 503 on every attempt, so the interactive retry
    // ladder (one retry) is spent and the request fails on the provider
    // boundary. `max_infra_retries: 0` keeps the slot from being planned again,
    // and `breaker_threshold: 10` keeps one empty trial from stopping the run.
    let mut plans = answering_plans();
    plans[1] = StreamPlan::new(
        REPORT_MARKER,
        vec![
            StreamResponse::service_unavailable("HTTP status 503 for the canary report stage"),
            StreamResponse::service_unavailable("HTTP status 503 for the canary report stage"),
            StreamResponse::service_unavailable("HTTP status 503 for the canary report stage"),
        ],
    );
    let backend = MockStreamingBackend::start_with_plans(MODEL, plans).unwrap();
    let (canary, request) = canary_request(backend.endpoint(), "run-provider-down").await;
    let executor = EmbeddedExecutor::new(DocumentRuntimeOptions::default(), canary.runs_dir());

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
async fn freeze_refuses_unrestricted_bash_and_an_oauth_backend_before_creating_any_home() {
    // Nothing is ever called, so the endpoint only has to be well formed.
    let (canary, base) = canary_request("http://127.0.0.1:1/v1", "run-refused").await;
    let executor = EmbeddedExecutor::new(DocumentRuntimeOptions::default(), canary.runs_dir());
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
    assert!(refusal.contains("principal_oauth"), "{refusal}");

    for run_id in ["run-bash", "run-oauth"] {
        assert!(
            !canary.runs_dir().join(run_id).exists(),
            "a refused request leaves no run directory behind: {run_id}"
        );
    }
}

/// One case, one trial per case, two cells against one real provider: do the
/// two arms of a pair answer identically at the seed they share?
///
/// A pair is how the runner shares a seed. A trial's seed is
/// `seed_base + trial_index` (`plan::plan`), so two trials of one case are
/// deliberately different draws and could never answer this question; the two
/// cells at one trial index are the same draw. Both cells here name the same
/// pack, behavior and profile, so the seed is the only thing they share and the
/// only thing that could make them agree.
///
/// The finding is reported, not asserted. Equality at one seed is evidence the
/// provider honoured it; inequality means it ignored the seed, or is not
/// deterministic at all. Both are legitimate answers about a provider, which is
/// why sampling runs at `temperature: 0.7`: at 0.0 the arms would agree
/// whatever the provider did with the seed, and the run would report nothing.
#[tokio::test]
#[ignore = "needs GENTS_EVAL_TARGET and a real backend"]
async fn live_smoke_one_trial_on_a_real_provider_reports_whether_seed_was_honoured() {
    // A test binary installs no subscriber, so an unreported finding is a
    // dropped one, even under `--nocapture`.
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "off,eval_runner_canary=info",
        ))
        .try_init();

    let target = support::live_inference::live_target();
    let (canary, mut request) = canary_request(target.endpoint(), "run-live").await;
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
                Collection::InferenceSampling,
                json!({
                    "agent_did": canary.owner,
                    "sampling_id": "canary-sampling",
                    "temperature": 0.7,
                }),
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
    request.case_ids = Some(vec!["one-stage".to_string()]);
    request.trials_per_case = 1;
    let candidate = CellRequest {
        cell_id: "candidate".into(),
        label: "candidate".into(),
        ..request.cells[0].clone()
    };
    request.cells.push(candidate);

    let executor = EmbeddedExecutor::new(DocumentRuntimeOptions::default(), canary.runs_dir());
    run(
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
    assert_eq!(trials.len(), 2, "{trials:#?}");
    let mut arms = Vec::new();
    for cell_id in ["baseline", "candidate"] {
        let trial = trials
            .iter()
            .find(|trial| trial.identity.cell_id == cell_id)
            .unwrap_or_else(|| panic!("no {cell_id} trial in {trials:#?}"));
        // `recollect` reopens the retained trial home and reads its assistant
        // messages; the first one is the arm's answer.
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

/// The launching home and everything the canary installed into it.
///
/// It owns the home and its directories: a `ConfigAccess` alone would keep the
/// node alive while its temporary directory was already removed.
struct Canary {
    _home: EmbeddedHome,
    access: ConfigAccess,
    dirs: TempDir,
    owner: String,
    pack_dir: PathBuf,
}

impl Canary {
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
            .transact("canary.install", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
    }
}

/// A launching home with the canary definition, its inference documents and
/// one request against the checked-in canary pack.
///
/// The request's owner is the launching home's DID: the run belongs to the
/// principal that launched it, and each trial gets its own DID of its own.
async fn canary_request(endpoint: &str, run_id: &str) -> (Canary, RunRequest) {
    let home = EmbeddedHome::create_temp("eval-canary").await.unwrap();
    let access = ConfigAccess::Local(home.node.clone());
    let owner = home.did().to_string();
    gents::ensure_agent_principal(home.node.as_ref(), &owner)
        .await
        .unwrap();
    let canary = Canary {
        access,
        owner: owner.clone(),
        pack_dir: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/eval_runner/canary_pack"),
        dirs: tempfile::tempdir().unwrap(),
        _home: home,
    };
    canary
        .install(vec![
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
                    "agent_did": owner,
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
        // The launching home both owns this run and evaluates it. They are
        // separate facts about it, so both are supplied rather than one
        // standing in for the other.
        evaluator_did: owner.clone(),
        owner,
        definition_id: "canary".into(),
        split: EvalSplit::Validation,
        case_ids: None,
        cells: vec![CellRequest {
            cell_id: "baseline".into(),
            label: "baseline".into(),
            source: CellSource::Directory(canary.pack_dir.clone()),
            behavior_id: "canary".into(),
            inference_profile_id: "canary".into(),
        }],
        trials_per_case: 1,
        seed_base: 1000,
        deadline_secs: Some(120),
        concurrency: 1,
        // An acceptance test wants the defect, not a retry that hides it, and
        // one empty trial must not stop the run.
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
            },
        ],
        runs_dir: canary.runs_dir(),
    };
    (canary, request)
}

/// Two cases on the validation split: one stage, then two. Every stage counts
/// the rows of the `items` capture, so a stage that ran says the same thing
/// about its subject wherever it sits in a case.
fn definition_document(owner: &str) -> Value {
    json!({
        "definition_id": "canary",
        "agent_did": owner,
        "comparability_version": 1,
        "title": "Eval runner canary",
        "subject": {"kind": "behavior", "inference_slots": ["primary"]},
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
        "agent_did": owner,
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

/// A backend that would spend the launching principal's own subscription
/// credential. `principal_oauth` is only compatible with an agent-scoped OAuth
/// provider kind, so the refusal has to be asked of a realistic document.
fn subscription_backend_document(owner: &str, backend_id: &str) -> Value {
    json!({
        "agent_did": owner,
        "backend_id": backend_id,
        "name": "Eval canary subscription backend",
        "provider_kind": "ClaudeCliSubscription",
        "endpoint": "https://api.anthropic.com",
        "auth": {"kind": "principal_oauth"},
    })
}

fn profile_document(owner: &str, profile_id: &str, backend_id: &str, model: &str) -> Value {
    json!({
        "agent_did": owner,
        "profile_id": profile_id,
        "backend_id": backend_id,
        "model_name": model,
        "sampling_id": "canary-sampling",
    })
}

/// One plan per stage marker, last stage first: a stage-2 request carries
/// stage 1's prompt in its history, and the backend takes the first plan whose
/// marker the body contains.
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
        trial_agent_did: trial.identity.trial_agent_did.clone(),
        session_id: trial.identity.session_id.clone(),
        home_hint: trial.identity.home_hint.clone(),
    }
}

/// What each case's trial anchored to, by case: the comparable part of a run's
/// result, independent of which home it ran in.
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

/// What the loop recorded beside each retained trial home, by case: the
/// `evidence.json` holding that trial's evidence digest and its anchor.
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

/// The canary pack with its one `Tools` document rewritten to grant this
/// host's shell, which is what an embedded trial may never be handed.
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
