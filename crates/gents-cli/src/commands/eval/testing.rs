//! A launching home for the `gents eval` and `gents optimization` command
//! tests. `gents::eval::runner::freeze::tests::Launching` and the optimizer's
//! matrix `Harness` are `#[cfg(test)] pub(crate)` inside `gents`, so this
//! crate builds its own from the same public pieces: an embedded home, the
//! documents a run and a job read, and a directory pack whose behavior's
//! prompt matches the installed context (the optimizer's freeze checks that).
//!
//! This file duplicates `write_fixture_pack` and `Launching` in
//! `crates/gents/src/eval/runner/freeze.rs` (its `tests` module) and the
//! definition shape of `crates/gents/src/optimization/driver/matrix.rs`.
//! Those files and this one move together: a change to the fixture pack's
//! layout or the definition's case shape there is made here in the same PR.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use gents::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::EvalSplit;
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::EmbeddedHome;
use gents::eval::runner::{
    run, CellRequest, CellSource, RunOptions, RunOutcome, RunRequest, ScriptKey, ScriptedExecutor,
    TrialEvidence, TrialExecutor,
};
use gents::{Collection, ConfigAccess};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::{execute, Deps, EvalContext};
use crate::cli::{Cli, Command, EvalCommand};

pub(crate) const DEFINITION: &str = "cli-def";
pub(crate) const TRAIN_CASES: [&str; 1] = ["train-a"];
pub(crate) const VALIDATION_CASES: [&str; 6] =
    ["val-a", "val-b", "val-c", "val-d", "val-e", "val-f"];
pub(crate) const HELD_OUT_CASES: [&str; 6] = ["ho-a", "ho-b", "ho-c", "ho-d", "ho-e", "ho-f"];
pub(crate) const TRIALS: u32 = 2;
pub(crate) const BASELINE_PROMPT: &str = "Watch the mailbox.\n";
/// What a scripted proposer offers: the optimizer matrix's candidate text.
pub(crate) const CANDIDATE_PROMPT: &str = "Watch the mailbox, and name the collection.\n";

pub(crate) struct Fixture {
    pub(crate) ctx: EvalContext,
    pub(crate) pack: PathBuf,
    _home: EmbeddedHome,
    _dirs: TempDir,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let home = EmbeddedHome::create_temp("cli-eval").await.unwrap();
        let owner = home.did().to_string();
        gents::ensure_agent_principal(home.node.as_ref(), &owner)
            .await
            .unwrap();
        let dirs = tempfile::tempdir().unwrap();
        let pack = dirs.path().join("subject");
        write_pack(&pack);
        let ctx = EvalContext {
            access: ConfigAccess::Local(home.node.clone()),
            home_dir: dirs.path().join("home"),
            owner: owner.clone(),
        };
        install(&ctx.access, documents(&owner)).await;
        Self {
            ctx,
            pack,
            _home: home,
            _dirs: dirs,
        }
    }

    pub(crate) fn pack_arg(&self) -> String {
        self.pack.display().to_string()
    }

    /// Two cells over the six validation cases, two trials each: 24 slots.
    pub(crate) fn request(&self, run_id: &str) -> RunRequest {
        RunRequest {
            run_id: run_id.into(),
            owner: self.ctx.owner.clone(),
            evaluator_did: self.ctx.owner.clone(),
            definition_id: DEFINITION.into(),
            split: EvalSplit::Validation,
            case_ids: None,
            cells: ["baseline", "candidate"]
                .into_iter()
                .map(|cell_id| CellRequest {
                    cell_id: cell_id.into(),
                    label: cell_id.into(),
                    source: CellSource::Directory(self.pack.clone()),
                    behavior_id: "monitor".into(),
                    inference_profile_id: "local".into(),
                })
                .collect(),
            trials_per_case: TRIALS,
            seed_base: 1_000,
            deadline_secs: None,
            concurrency: 1,
            max_infra_retries: 1,
            breaker_threshold: 5,
            purpose: "eval".into(),
            source_commit: "cli-test".into(),
            source_dirty: false,
            captures: Vec::new(),
            runs_dir: self.ctx.runs_dir(),
        }
    }

    /// A run produced by the library with a scripted executor.
    pub(crate) async fn scripted_run(
        &self,
        run_id: &str,
        executor: &ScriptedExecutor,
        cancel: CancellationToken,
    ) -> RunOutcome {
        run(
            &self.ctx.access,
            &self.request(run_id),
            executor,
            &CheckRegistry::builtin(),
            cancel,
            &fast(),
        )
        .await
        .unwrap()
    }
}

pub(crate) fn fast() -> RunOptions {
    RunOptions {
        poll_backoff_base: Duration::from_millis(1),
        poll_backoff_cap: Duration::from_millis(2),
    }
}

/// A stage whose `findings` capture holds a row: `in_range`, 10000.
pub(crate) fn pass() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
}

/// A stage whose `findings` capture is empty: `below_min`, 0.
pub(crate) fn fail() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", Vec::new())
}

/// Every trial passes, except the `baseline` cell's first attempts on
/// `failing_baseline_cases`.
pub(crate) fn executor(failing_baseline_cases: &[&str]) -> ScriptedExecutor {
    let mut executor = ScriptedExecutor::new().with_default(pass());
    for case_id in failing_baseline_cases {
        for trial_index in 0..TRIALS {
            executor = executor.with(
                ScriptKey {
                    cell_label: "baseline".into(),
                    case_id: (*case_id).into(),
                    trial_index,
                    attempt: 1,
                },
                fail(),
            );
        }
    }
    executor
}

/// `gents eval <argv…>` as clap parses it.
pub(crate) fn eval_command(argv: &[&str]) -> EvalCommand {
    let cli = Cli::try_parse_from(["gents", "eval"].into_iter().chain(argv.iter().copied()))
        .unwrap_or_else(|error| panic!("{error}"));
    match cli.command {
        Command::Eval { command } => command,
        _ => panic!("not an eval command"),
    }
}

pub(crate) fn deps<'a>(
    executor: &'a dyn TrialExecutor,
    registry: &'a CheckRegistry,
    cancel: CancellationToken,
) -> Deps<'a> {
    Deps {
        executor,
        registry,
        cancel,
        options: fast(),
    }
}

/// Run `gents eval <argv…>` with `deps` and return what it wrote.
pub(crate) async fn eval_with(
    fixture: &Fixture,
    argv: &[&str],
    deps: &Deps<'_>,
) -> anyhow::Result<String> {
    let mut out = Vec::new();
    execute(&fixture.ctx, eval_command(argv), deps, &mut out).await?;
    Ok(String::from_utf8(out)?)
}

/// Run `gents eval <argv…>` with a scripted executor where every trial passes.
pub(crate) async fn eval(fixture: &Fixture, argv: &[&str]) -> anyhow::Result<String> {
    let executor = executor(&[]);
    let registry = CheckRegistry::builtin();
    eval_with(
        fixture,
        argv,
        &deps(&executor, &registry, CancellationToken::new()),
    )
    .await
}

/// The whitespace-split row of `output` that starts with `first` and has
/// exactly `width` columns.
pub(crate) fn row<'a>(output: &'a str, first: &str, width: usize) -> Vec<&'a str> {
    output
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|words| words.first() == Some(&first) && words.len() == width)
        .unwrap_or_else(|| panic!("no {width}-column row for {first} in\n{output}"))
}

fn documents(owner: &str) -> Vec<(Collection, Value)> {
    let case = |case_id: &str, split: &str| {
        json!({
            "case_id": case_id,
            "split": split,
            "stages": [{
                "stage_id": "check",
                "prompt": "Run the monitor.",
                "deadline_secs": 600,
                "checks": [{
                    "check": "captured_rows_count",
                    "params": {"name": "findings", "min": 1},
                    "tier": "acceptance",
                }],
            }],
        })
    };
    let cases: Vec<Value> = TRAIN_CASES
        .iter()
        .map(|id| case(id, "train"))
        .chain(VALIDATION_CASES.iter().map(|id| case(id, "validation")))
        .chain(HELD_OUT_CASES.iter().map(|id| case(id, "held_out")))
        .collect();
    vec![
        (
            Collection::EvalDefinition,
            json!({
                "definition_id": DEFINITION,
                "agent_did": owner,
                "comparability_version": 1,
                "subject": {"kind": "behavior", "inference_slots": ["primary"]},
                "cases": cases,
            }),
        ),
        (
            Collection::InferenceBackend,
            json!({
                "agent_did": owner,
                "backend_id": "backend",
                "name": "Workstation",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:8000/v1",
                "auth": {"kind": "unauthenticated"},
            }),
        ),
        (
            Collection::InferenceSampling,
            json!({"agent_did": owner, "sampling_id": "sampling", "temperature": 0.0}),
        ),
        (
            Collection::InferenceProfile,
            json!({
                "agent_did": owner,
                "profile_id": "local",
                "backend_id": "backend",
                "model_name": "test-model",
                "sampling_id": "sampling",
            }),
        ),
        (
            Collection::AgentContext,
            json!({
                "context_id": "monitor-context",
                "agent_did": owner,
                "display_name": "Monitor",
                "system_prompt": BASELINE_PROMPT,
            }),
        ),
        (
            Collection::AgentBehavior,
            json!({
                "behavior_id": "monitor",
                "agent_did": owner,
                "display_name": "Monitor",
                "context_id": "monitor-context",
                "inference_profile_id": "local",
            }),
        ),
    ]
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
        .transact("cli.eval.test_fixture", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
}

/// The shape of `gents::eval::runner::freeze::tests::write_fixture_pack`:
/// one `monitor` behavior whose context prompt is [`BASELINE_PROMPT`].
fn write_pack(root: &Path) {
    std::fs::create_dir_all(root.join("agent_behaviors/monitor")).unwrap();
    std::fs::write(root.join("README.md"), "# monitor fixture\n").unwrap();
    std::fs::write(
        root.join("agent_behaviors/monitor/system_prompt.md"),
        BASELINE_PROMPT,
    )
    .unwrap();
    let manifest = json!({
        "manifest_version": 1,
        "name": "monitor_fixture",
        "version": "1.0.0",
        "description": "CLI eval fixture: one monitor behavior.",
        "authors": ["gents-ai contributors"],
        "kind": "documents",
        "assets": [
            "README.md",
            "agent_behaviors/monitor/system_prompt.md",
            "pack_config.json",
        ],
        "config": "pack_config.json",
        "inference_slots": [{
            "name": "primary",
            "description": "Runs the monitor behavior.",
            "behaviors": ["monitor"],
        }],
    });
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let config = json!({
        "agent_principal": {},
        "agent_behaviors": [{
            "behavior_id": "monitor",
            "display_name": "Monitor",
            "context_id": "monitor-context",
            "inference_profile_id": "gents:inference-slot:primary",
        }],
        "contexts": [{
            "context_id": "monitor-context",
            "display_name": "Monitor",
            "system_prompt": "./agent_behaviors/monitor/system_prompt.md",
            "tools_id": "monitor-tools",
        }],
        "tools": [{
            "tools_id": "monitor-tools",
            "display_name": "Monitor tools",
            "host": {"bash": {"mode": "Off"}},
        }],
    });
    std::fs::write(
        root.join("pack_config.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();
}
