use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use gents::documents::EvalSplit;
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::EmbeddedHome;
use gents::eval::runner::{
    run, CellRequest, CellSource, RunOptions, RunOutcome, RunRequest, ScriptKey, ScriptedExecutor,
    TrialEvidence, TrialExecutor,
};
use gents::store::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::{Collection, ConfigAccess, NodeIdentity};
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
pub(crate) const CANDIDATE_PROMPT: &str = "Watch the mailbox, and name the collection.\n";

pub(crate) struct Fixture {
    pub(crate) ctx: EvalContext,
    pub(crate) pack: PathBuf,
    _home: EmbeddedHome,
    _dirs: TempDir,
}

/// A test's `#[tokio::test]` runtime is its entry point; the fixture's
/// context takes that runtime's handle from here.
#[allow(
    clippy::disallowed_methods,
    reason = "a test's #[tokio::test] runtime is its entry-point runtime"
)]
pub(crate) fn test_runtime_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let mut home = EmbeddedHome::create_temp("cli-eval").await.unwrap();
        let owner = home.did().to_string();
        let dirs = tempfile::tempdir().unwrap();
        let evaluator = std::sync::Arc::new(
            gents::KeyIdentity::load_or_create(dirs.path().join("evaluator.key"), None).unwrap(),
        );
        let client_signing = evaluator.client_signing_capability().unwrap();
        home.set_evaluator(evaluator.clone(), client_signing.clone());
        let admin = ConfigAccess::local_as(
            home.node.clone(),
            ::identity::Did::new(owner.clone()).unwrap(),
        );
        gents::store::identity::GrantSet::OperatorUser
            .grant(&admin, &::identity::Did::new(evaluator.did()).unwrap())
            .await
            .unwrap();
        let access = home.client_access(evaluator.did()).unwrap();
        let pack = dirs.path().join("subject");
        write_pack(&pack);
        let ctx = EvalContext {
            access,
            home_dir: dirs.path().join("home"),
            owner: owner.clone(),
            runtime: test_runtime_handle(),
            evaluator,
            client_signing,
        };
        let binding =
            gents::store::identity::provision_agent_identity(home.identity.as_ref()).unwrap();
        let mut initial = vec![(Collection::Node, json!({"node_did": &owner}))];
        let (client_documents, owner_documents): (Vec<_>, Vec<_>) =
            documents(&owner).into_iter().partition(|(collection, _)| {
                !matches!(collection, Collection::Tools | Collection::AgentContext)
            });
        initial.extend(client_documents);
        install(&ctx.access, initial).await;
        // Tools and AgentContext are source-projected, so the Node owner writes
        // them; the baseline Agent is installed with its recorded slot binding
        // so optimization can prove the installed baseline.
        install(&admin, owner_documents).await;
        install_baseline(&admin, &owner, &pack, &binding).await;
        set_default_agent(&ctx.access, &owner, &binding.agent_did).await;
        Self {
            ctx,
            pack,
            _home: home,
            _dirs: dirs,
        }
    }

    /// This home as a command sees it with no `gents server` running: its
    /// configuration access is the embedded node rather than a served endpoint.
    pub(crate) fn unserved(mut self) -> Self {
        self.ctx.access = ConfigAccess::local_as(
            self._home.node.clone(),
            ::identity::Did::new(self.ctx.owner.clone()).unwrap(),
        );
        self
    }

    pub(crate) fn pack_arg(&self) -> String {
        self.pack.display().to_string()
    }

    pub(crate) fn request(&self, run_id: &str) -> RunRequest {
        RunRequest {
            run_id: run_id.into(),
            owner: self.ctx.owner.clone(),
            evaluator_did: self.ctx.access.actor_did().expect("held evaluator"),
            definition_id: DEFINITION.into(),
            split: EvalSplit::Validation,
            case_ids: None,
            cells: ["baseline", "candidate"]
                .into_iter()
                .map(|cell_id| CellRequest {
                    cell_id: cell_id.into(),
                    label: cell_id.into(),
                    source: CellSource::Directory(self.pack.clone()),
                    agent_slot: "monitor".into(),
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
        trial_budget: None,
        poll_backoff_base: Duration::from_millis(1),
        poll_backoff_cap: Duration::from_millis(2),
    }
}

pub(crate) fn pass() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
}

pub(crate) fn fail() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", Vec::new())
}

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

pub(crate) async fn eval_with(
    fixture: &Fixture,
    argv: &[&str],
    deps: &Deps<'_>,
) -> anyhow::Result<String> {
    let mut out = Vec::new();
    execute(&fixture.ctx, eval_command(argv), deps, &mut out).await?;
    Ok(String::from_utf8(out)?)
}

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
                "node_did": owner,
                "comparability_version": 1,
                "subject": {"kind": "agent", "inference_slots": ["primary"]},
                "cases": cases,
            }),
        ),
        (
            Collection::InferenceBackend,
            json!({
                "node_did": owner,
                "backend_id": "backend",
                "name": "Workstation",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:8000/v1",
                "auth": {"kind": "unauthenticated"},
            }),
        ),
        (
            Collection::InferenceSampling,
            json!({"node_did": owner, "sampling_id": "sampling", "temperature": 0.0}),
        ),
        (
            Collection::InferenceProfile,
            json!({
                "node_did": owner,
                "profile_id": "local",
                "backend_id": "backend",
                "model_name": "test-model",
                "sampling_id": "sampling",
            }),
        ),
        (
            Collection::Tools,
            json!({
                "tools_id": "monitor-tools",
                "node_did": owner,
                "display_name": "Monitor tools",
                "host": {"bash": {"mode": "Off"}},
            }),
        ),
        (
            Collection::AgentContext,
            json!({
                "context_id": "monitor-context",
                "node_did": owner,
                "display_name": "Monitor",
                "system_prompt": BASELINE_PROMPT,
                "tools_id": "monitor-tools",
            }),
        ),
    ]
}

/// Installs the subject pack's Agent with its recorded slot binding, bound to
/// the `local` profile, as the Node owner.
async fn install_baseline(
    owner_access: &ConfigAccess,
    owner: &str,
    pack: &Path,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let manifest: gents::pack::PackManifest =
        serde_json::from_slice(&std::fs::read(pack.join("manifest.json")).unwrap()).unwrap();
    let files: std::collections::BTreeMap<String, Vec<u8>> = gents::pack::declared_paths(&manifest)
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(pack.join(&path)).unwrap();
            (path, bytes)
        })
        .collect();
    let read = |path: &str| {
        files
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("pack has no asset {path:?}"))
    };
    let digest = gents::pack::digest_declared_assets(&manifest, |path| {
        files
            .get(path)
            .map(Vec::as_slice)
            .ok_or_else(|| anyhow::anyhow!("pack has no asset {path:?}"))
    })
    .unwrap();
    let authored = gents::pack::load_pack_authoring_config(
        &manifest,
        &gents::pack::PackInstallOptions {
            node_did: owner.to_owned(),
        },
        &read,
        &|_| None,
    )
    .unwrap();
    let mut config = authored.documents.clone();
    let mut agent = authored.agent("monitor").unwrap().clone();
    let fields = agent.as_object_mut().unwrap();
    fields.remove("agent_slot");
    fields.insert("agent_did".into(), json!(binding.agent_did));
    fields.insert("creation_nonce".into(), json!(binding.creation_nonce));
    fields.insert("node_did".into(), json!(owner));
    config.agents.push(serde_json::from_value(agent).unwrap());
    let agent_bindings = std::collections::BTreeMap::from([("monitor".into(), binding.clone())]);
    let config = gents::pack::BoundPackConfig {
        installation_doc_id: None,
        config,
        agent_bindings: agent_bindings.clone(),
    }
    .bind_inference(
        &manifest,
        &std::collections::BTreeMap::from([("primary".into(), "local".into())]),
    )
    .unwrap();
    let mut identity = gents::pack::PackIdentity::new(&manifest, digest, Vec::new());
    identity.agent_bindings = agent_bindings;
    gents::pack::install_pack_documents(
        owner_access,
        owner,
        &identity,
        &config,
        gents::pack::DriftPolicy::Overwrite,
    )
    .await
    .unwrap();
}

async fn set_default_agent(access: &ConfigAccess, owner: &str, agent_did: &str) {
    access
        .transact_write("cli.eval.test_default_agent", |txn| {
            Box::pin(async move {
                let (_, mut node) = gents::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    Collection::Node,
                    owner,
                    owner,
                )
                .await?
                .expect("fixture Node");
                node["default_agent_did"] = json!(agent_did);
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::Node,
                    add: node.clone(),
                    update: node,
                }])?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .unwrap();
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
        .transact_write("cli.eval.test_fixture", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
}

fn write_pack(root: &Path) {
    std::fs::create_dir_all(root.join("agents/monitor")).unwrap();
    std::fs::write(root.join("README.md"), "# monitor fixture\n").unwrap();
    std::fs::write(
        root.join("agents/monitor/system_prompt.md"),
        BASELINE_PROMPT,
    )
    .unwrap();
    let manifest = json!({
        "manifest_version": 1,
        "name": "monitor_fixture",
        "version": "1.0.0",
        "description": "CLI eval fixture: one monitor agent.",
        "authors": ["gents-ai contributors"],
        "kind": "documents",
        "assets": [
            "README.md",
            "agents/monitor/system_prompt.md",
            "pack_config.json",
        ],
        "config": "pack_config.json",
        "inference_slots": [{
            "name": "primary",
            "description": "Runs the monitor agent.",
            "agents": ["monitor"],
        }],
    });
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let config = json!({
        "node": {},
        "agents": [{
            "agent_slot": "monitor",
            "display_name": "Monitor",
            "context_id": "monitor-context",
            "inference_profile_id": "gents:inference-slot:primary",
        }],
        "contexts": [{
            "context_id": "monitor-context",
            "display_name": "Monitor",
            "system_prompt": "./agents/monitor/system_prompt.md",
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
