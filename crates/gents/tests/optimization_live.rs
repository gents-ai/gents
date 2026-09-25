//! The two live scenarios M6 is judged by: one accept and one reject on a real
//! provider. Never in CI; run in M5 (ruling R7).
//!
//! The subject and its definition come from M3's pack, named by environment
//! variable, so this file compiles and stays ignored without that branch.
//!
//! # What the default policy demands (ruling T42-2)
//!
//! Both scenarios default to `PolicyV2::uncalibrated()` and two trials per
//! case. M5 overrides them with `GENTS_OPTIMIZATION_LIVE_POLICY`, a serialized
//! `PolicyV2`, and `GENTS_OPTIMIZATION_LIVE_TRIALS_PER_CASE`. It is expected to
//! pass its calibrated policy that way, because the placeholder is strict on
//! M3's 8 train / 6 validation / 6 held-out split (ruling N12):
//!
//! - `alpha_effective = 50_000 / 3 = 16_666` ppm. With 6 validation cases, the
//!   exact one-sided sign-flip test reaches `p = 1/64 = 15_625` ppm only when
//!   all 6 per-case mean differences are strictly positive. One tied or
//!   worsened case gives at least `2/64 = 31_250` ppm, so the golden prompt
//!   must strictly beat the thin one on every validation case.
//! - `max_token_increase_bp: 2500` caps the candidate's mean tokens at 1.25x
//!   the baseline's, on validation and again on held-out. In the accept
//!   scenario the baseline is the one-line thin prompt, so a golden prompt that
//!   drives tool use can fail on cost (`Reject(CostRegression)` in validation,
//!   `Failed` at held-out) even when its scores are better.
//!
//! The proposer is given the same text for every round. Only round 1 is a real
//! trial. Rounds 2 and later propose a digest the job has already evaluated, so
//! the structural gate rejects them as duplicates without running anything. If
//! round 1 is not accepted, the job ends `NothingToPromote`.
//!
//! A job's policy must have `max_rounds` equal to its budget, so the budget
//! and the script length both follow the policy. The case-trial budget is
//! what the policy, the trials per case and the definition's split can ever
//! charge, so no override ends a scenario `Exhausted`.
//!
//! # Captures (ruling F-1)
//!
//! M3's stages declare their own `EvalStage.capture` (its `items` capture),
//! and the runner reads a stage's capture before the request's. The job's
//! `captures` are only the request-level fallback, so these scenarios pass
//! none unless `GENTS_OPTIMIZATION_LIVE_CAPTURES` names a serialized
//! `Vec<Capture>`, and they do not depend on it.

mod support;

use std::path::{Path, PathBuf};

use gents::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::{EvalDefinition, EvalSplit, InferenceBackend, InferenceProfile};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::{EmbeddedExecutor, EmbeddedHome};
use gents::eval::runner::{Capture, RunOptions};
use gents::optimization::{
    materialize_pack, run_cost, run_job, show, Budgets, Decision, JobOutcome, JobRequest, JobState,
    JobView, Mode, PolicyV2, ScriptedProposer,
};
use gents::{Collection, ConfigAccess, DocumentRuntimeOptions};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// The M3 pack directory: the monitor subject and its eval definition.
const PACK_VAR: &str = "GENTS_OPTIMIZATION_LIVE_PACK";
/// The id of the definition inside that pack (M3 names it `monitor-findings`).
const DEFINITION_VAR: &str = "GENTS_OPTIMIZATION_LIVE_DEFINITION_ID";
/// The behavior whose context is optimized; `monitor` unless overridden.
const BEHAVIOR_VAR: &str = "GENTS_OPTIMIZATION_LIVE_BEHAVIOR";
/// A serialized `PolicyV2`; `PolicyV2::uncalibrated()` unless set.
const POLICY_VAR: &str = "GENTS_OPTIMIZATION_LIVE_POLICY";
/// Trials per case for both arms; 2 unless set.
const TRIALS_VAR: &str = "GENTS_OPTIMIZATION_LIVE_TRIALS_PER_CASE";
/// A serialized `Vec<Capture>`, the job's request-level capture fallback;
/// empty unless set.
const CAPTURES_VAR: &str = "GENTS_OPTIMIZATION_LIVE_CAPTURES";
/// A deliberately thin baseline, so a real provider has room to improve.
const THIN_PROMPT: &str = "Look at the mailbox and say something.\n";
/// A prompt that removes the output contract the checks grade.
const DEGRADED_PROMPT: &str = "Answer in one word. Do not use any tools.\n";

/// Which prompt the baseline subject carries.
enum Baseline {
    /// The M3 pack with its sidecar prompt replaced by [`THIN_PROMPT`].
    Thin,
    /// The M3 pack as written, whose prompt M3 wrote to pass these cases.
    Golden,
}

#[tokio::test]
#[ignore = "M5: needs GENTS_EVAL_TARGET, GENTS_OPTIMIZATION_LIVE_PACK, GENTS_OPTIMIZATION_LIVE_DEFINITION_ID and a real backend"]
async fn live_accept_the_golden_prompt_beats_a_thin_one_and_reaches_ready_to_promote() {
    let fixture = Fixture::new("live-accept", Baseline::Thin).await;
    // The candidate is the pack's own golden prompt; the baseline is the thin one.
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
    // The baseline is M3's golden subject, which passes these cases, so the
    // rejection is of a real regression and not a tie between two failing arms.
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
    // A dead backend, a structural reject or thin evidence also leaves nothing
    // to promote; only a journaled policy rejection that its rows still support
    // is a live reject.
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

/// A launching home with the M3 definition, a live inference binding, and a
/// copy of the M3 pack, thinned or not, as the baseline subject.
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
        // A test binary installs no subscriber, so an unreported finding is a
        // dropped one, even under `--nocapture`.
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
        let behavior = std::env::var(BEHAVIOR_VAR).unwrap_or_else(|_| "monitor".into());
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
        let access = ConfigAccess::Local(home.node.clone());
        let owner = home.did().to_string();
        gents::ensure_agent_principal(home.node.as_ref(), &owner)
            .await
            .unwrap();
        let dirs = tempfile::tempdir().unwrap();

        // The pack carries both the subject and the definition.
        let golden = materialize_pack(&pack, &owner, &behavior).unwrap_or_else(|error| {
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
        let baseline_prompt = match baseline {
            Baseline::Golden => golden_prompt.clone(),
            Baseline::Thin => {
                let sidecar = golden.prompt_asset.clone().expect(
                    "assumption failed: the M3 pack keeps its system prompt in a sidecar asset",
                );
                std::fs::write(baseline_pack.join(&sidecar), THIN_PROMPT).unwrap();
                THIN_PROMPT.to_owned()
            }
        };

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
                    json!({"agent_did": owner, "sampling_id": "live", "temperature": 0.0}),
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
                // Ruling R5: the live context must hold the pack's prompt.
                (
                    Collection::AgentContext,
                    json!({
                        "context_id": golden.context_id,
                        "agent_did": owner,
                        "display_name": "Monitor",
                        "system_prompt": baseline_prompt,
                    }),
                ),
                (
                    Collection::AgentBehavior,
                    json!({
                        "behavior_id": behavior,
                        "agent_did": owner,
                        "display_name": "Monitor",
                        "context_id": golden.context_id,
                        "inference_profile_id": "live",
                    }),
                ),
            ],
        )
        .await;

        let request = JobRequest {
            job_id: job_id.into(),
            owner: owner.clone(),
            evaluator_did: owner,
            behavior_id: behavior,
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

    /// The same proposal for every round the policy allows; see the module doc
    /// for why only round 1 is a real trial.
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

/// The most case-trials the job can ever be charged: every round's train run
/// and every validation attempt the policy allows, plus the reserved held-out
/// run. A budget of this size never ends a scenario `Exhausted`, whatever
/// trials-per-case or policy override M5 passes.
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
        .transact("optimization_live.install", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
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
