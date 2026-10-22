mod support;

use std::path::{Path, PathBuf};

use gents::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::{EvalDefinition, EvalSplit, InferenceBackend, InferenceProfile};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::{EmbeddedExecutor, EmbeddedHome};
use gents::eval::runner::{Capture, RunOptions};
use gents::optimization::target::JobTarget;
use gents::optimization::{
    materialize_pack, run_cost, run_job, show, Budgets, Decision, JobOutcome, JobRequest, JobState,
    JobView, Mode, PolicyV2, ScriptedProposer,
};
use gents::{Collection, ConfigAccess, DocumentRuntimeOptions};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const PACK_VAR: &str = "GENTS_OPTIMIZATION_LIVE_PACK";
const DEFINITION_VAR: &str = "GENTS_OPTIMIZATION_LIVE_DEFINITION_ID";
const AGENT_VAR: &str = "GENTS_OPTIMIZATION_LIVE_AGENT";
const POLICY_VAR: &str = "GENTS_OPTIMIZATION_LIVE_POLICY";
const TRIALS_VAR: &str = "GENTS_OPTIMIZATION_LIVE_TRIALS_PER_CASE";
const CAPTURES_VAR: &str = "GENTS_OPTIMIZATION_LIVE_CAPTURES";
const THIN_PROMPT: &str = "Look at the mailbox and say something.\n";
const DEGRADED_PROMPT: &str = "Answer in one word. Do not use any tools.\n";

#[derive(PartialEq, Eq)]
enum Baseline {
    Thin,
    Golden,
}

#[tokio::test]
#[ignore = "M5: needs GENTS_EVAL_TARGET, GENTS_OPTIMIZATION_LIVE_PACK, GENTS_OPTIMIZATION_LIVE_DEFINITION_ID and a real backend"]
async fn live_accept_the_golden_prompt_beats_a_thin_one_and_reaches_ready_to_promote() {
    let fixture = Fixture::new("live-accept", Baseline::Thin).await;
    let proposer = fixture.proposer(
        &fixture.golden_prompt,
        "restore the full monitor instructions",
    );
    let outcome = fixture.drive(&proposer).await;
    let view = fixture.show().await;
    tracing::info!(state = outcome.state.label(), decisions = ?view.decisions, "live accept scenario");
    assert_eq!(
        outcome.state,
        JobState::ReadyToPromote,
        "the golden prompt did not beat a thin one: {outcome:#?}\n{:#?}",
        view.decisions
    );
    assert_eq!(
        outcome.checkpoint.expect("a checkpoint").text,
        fixture.golden_prompt
    );
    let confirm = view
        .decisions
        .iter()
        .find(|decision| decision.mode == Mode::Confirm)
        .unwrap_or_else(|| panic!("no held-out confirmation: {:#?}", view.decisions));
    assert_eq!(confirm.journaled, Decision::Accept, "{confirm:#?}");
    assert!(!confirm.mismatch, "{confirm:#?}");
}

#[tokio::test]
#[ignore = "M5: needs GENTS_EVAL_TARGET, GENTS_OPTIMIZATION_LIVE_PACK, GENTS_OPTIMIZATION_LIVE_DEFINITION_ID and a real backend"]
async fn live_reject_a_prompt_that_ignores_the_task_never_reaches_ready_to_promote() {
    let fixture = Fixture::new("live-reject", Baseline::Golden).await;
    let proposer = fixture.proposer(DEGRADED_PROMPT, "shorter is better");
    let outcome = fixture.drive(&proposer).await;
    let view = fixture.show().await;
    tracing::info!(state = outcome.state.label(), decisions = ?view.decisions, "live reject scenario");
    assert_eq!(
        outcome.state,
        JobState::NothingToPromote,
        "a prompt that ignores the task did not end with nothing to promote: {outcome:#?}\n{:#?}",
        view.decisions
    );
    assert!(outcome.checkpoint.is_none(), "{outcome:#?}");
    let round_one = view
        .decisions
        .iter()
        .rfind(|decision| decision.round == Some(1) && decision.mode == Mode::Improve)
        .unwrap_or_else(|| panic!("round 1 journaled no decision: {:#?}", view.decisions));
    assert!(
        matches!(round_one.journaled, Decision::Reject(_)),
        "round 1 was not a policy rejection: {round_one:#?}"
    );
    assert!(!round_one.mismatch, "{round_one:#?}");
}

struct Fixture {
    _home: EmbeddedHome,
    _dirs: TempDir,
    access: ConfigAccess,
    request: JobRequest,
    policy: PolicyV2,
    golden_prompt: String,
}

impl Fixture {
    async fn new(job_id: &str, baseline: Baseline) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "off,optimization_live=info,gents::optimization=info",
            ))
            .try_init();

        let pack = PathBuf::from(
            std::env::var(PACK_VAR).unwrap_or_else(|_| panic!("{PACK_VAR} must name the M3 pack")),
        );
        let definition_id = std::env::var(DEFINITION_VAR)
            .unwrap_or_else(|_| panic!("{DEFINITION_VAR} must name the definition in that pack"));
        let behavior = std::env::var(AGENT_VAR).unwrap_or_else(|_| "monitor".into());
        let policy = match std::env::var(POLICY_VAR) {
            Ok(raw) => serde_json::from_str::<PolicyV2>(&raw)
                .unwrap_or_else(|error| panic!("{POLICY_VAR} is not a PolicyV2: {error}")),
            Err(_) => PolicyV2::uncalibrated(),
        };
        let trials_per_case = match std::env::var(TRIALS_VAR) {
            Ok(raw) => raw
                .parse::<u32>()
                .unwrap_or_else(|error| panic!("{TRIALS_VAR} is not a u32: {error}")),
            Err(_) => 2,
        };
        let captures = match std::env::var(CAPTURES_VAR) {
            Ok(raw) => serde_json::from_str::<Vec<Capture>>(&raw)
                .unwrap_or_else(|error| panic!("{CAPTURES_VAR} is not a Vec<Capture>: {error}")),
            Err(_) => Vec::new(),
        };

        let home = EmbeddedHome::create_temp("optimization-live")
            .await
            .unwrap();
        let access = ConfigAccess::local(home.node.clone());
        let owner = home.did().to_string();
        gents::ensure_node(&home.node, &owner).await.unwrap();
        let dirs = tempfile::tempdir().unwrap();

        let golden = materialize_pack(
            &pack,
            &owner,
            &behavior,
            &JobTarget::Context,
        )
        .unwrap_or_else(|error| {
            panic!("assumption failed: the M3 pack loads as the subject of behavior {behavior:?}: {error:#}")
        });
        let golden_prompt = gents::optimization::baseline_text(&golden).expect(
            "assumption failed: the M3 pack's subject context carries a readable system prompt",
        );
        let definition = golden
            .config
            .eval_definitions
            .iter()
            .find(|definition| definition.definition_id == definition_id)
            .unwrap_or_else(|| {
                panic!("assumption failed: the M3 pack declares definition {definition_id:?} in eval_definitions")
            });
        let baseline_pack = dirs.path().join("baseline-pack");
        copy_tree(&pack, &baseline_pack);
        if baseline == Baseline::Thin {
            let sidecar = golden.prompt_asset.clone().expect(
                "assumption failed: the M3 pack keeps its system prompt in a sidecar asset",
            );
            std::fs::write(baseline_pack.join(&sidecar), THIN_PROMPT).unwrap();
        }

        let target = support::live_inference::live_target();
        install(
            &access,
            vec![
                (
                    Collection::EvalDefinition,
                    serde_json::to_value(definition).unwrap(),
                ),
                (
                    Collection::InferenceBackend,
                    serde_json::to_value(InferenceBackend {
                        backend_id: "live".into(),
                        ..target.backend(&owner)
                    })
                    .unwrap(),
                ),
                (
                    Collection::InferenceSampling,
                    json!({"node_did": owner, "sampling_id": "live", "temperature": 0.0}),
                ),
                (
                    Collection::InferenceProfile,
                    serde_json::to_value(InferenceProfile {
                        profile_id: "live".into(),
                        backend_id: "live".into(),
                        sampling_id: Some("live".into()),
                        execution_id: None,
                        ..target.profile(&owner)
                    })
                    .unwrap(),
                ),
            ]
            .into_iter()
            .chain(live_subject(&baseline_pack, &owner, &behavior))
            .collect(),
        )
        .await;

        let request = JobRequest {
            job_id: job_id.into(),
            owner: owner.clone(),
            evaluator_did: owner,
            agent_id: behavior,
            target: JobTarget::Context,
            definition_id,
            inference_profile_id: "live".into(),
            baseline_pack,
            trials_per_case,
            budgets: Budgets {
                max_rounds: policy.max_rounds,
                max_case_trials: case_trial_ceiling(definition, &policy, trials_per_case),
                max_tokens: u64::MAX,
                deadline_unix_secs: None,
            },
            max_text_bytes: 32 * 1024,
            seed_base: 7_000,
            jobs_dir: dirs.path().join("eval/jobs"),
            runs_dir: dirs.path().join("eval/runs"),
            source_commit: "live".into(),
            source_dirty: false,
            concurrency: 2,
            max_infra_retries: 1,
            breaker_threshold: 8,
            deadline_secs: Some(900),
            captures,
            run_options: RunOptions::default(),
        };

        Self {
            _home: home,
            _dirs: dirs,
            access,
            request,
            policy,
            golden_prompt,
        }
    }

    fn proposer(&self, text: &str, rationale: &str) -> ScriptedProposer {
        ScriptedProposer::new(vec![
            (text.to_owned(), rationale.to_owned());
            self.policy.max_rounds as usize
        ])
    }

    async fn show(&self) -> JobView {
        show(&self.access, &self.request.owner, &self.request.job_id)
            .await
            .unwrap()
    }

    async fn drive(&self, proposer: &ScriptedProposer) -> JobOutcome {
        let executor = EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            self.request.runs_dir.clone(),
            support::runtime_handle(),
        );
        run_job(
            &self.access,
            &self.request,
            &executor,
            proposer,
            &CheckRegistry::builtin(),
            &self.policy,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    }
}

fn case_trial_ceiling(definition: &EvalDefinition, policy: &PolicyV2, trials: u32) -> u64 {
    let attempts = u64::from(policy.max_reruns) + 1;
    let round = run_cost(definition, EvalSplit::Train, trials, 1).saturating_add(
        attempts.saturating_mul(run_cost(definition, EvalSplit::Validation, trials, 2)),
    );
    u64::from(policy.max_rounds)
        .saturating_mul(round)
        .saturating_add(run_cost(definition, EvalSplit::HeldOut, trials, 2))
}

async fn install(access: &ConfigAccess, documents: Vec<(Collection, Value)>) {
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
    access
        .transact_write("optimization_live.install", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
}

fn live_subject(pack: &Path, owner: &str, behavior: &str) -> Vec<(Collection, Value)> {
    let subject = materialize_pack(pack, owner, behavior, &JobTarget::Context).unwrap();
    let bindings = subject
        .manifest
        .metadata
        .inference_slots
        .iter()
        .map(|slot| (slot.name.clone(), "live".to_owned()))
        .collect();
    let bound =
        gents::pack::bind_pack_install_config(&subject.manifest, &subject.config, &bindings)
            .unwrap();
    DesiredStateApplyPlan::from_pack_config(&bound)
        .unwrap()
        .documents()
        .iter()
        .filter(|document| {
            !matches!(
                document.collection,
                Collection::Node | Collection::EvalDefinition
            )
        })
        .map(|document| (document.collection, document.add.clone()))
        .collect()
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
