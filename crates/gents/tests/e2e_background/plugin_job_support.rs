use std::path::{Path, PathBuf};
use std::time::Duration;

use gents::document_config::{AgentTools, IntegrationTools, PluginToolRef, Tools};
use gents::eval::runner::embedded::EmbeddedHome;
use gents::pack::{
    install_prepared_document_pack, load_pack_config, prepare_document_pack_install, DriftPolicy,
    InstalledVia, PackInferenceBindings, PackInstallOptions,
};
use gents::pack_archive::{pack_dir, PackArchive};
use gents::plugin_job::job::{
    JobChildState, JobControlRequest, JobEffects, JobGuestPin, JobPolicy, JobReadError, JobSpawn,
    JobState, JobStepOutput, JobView, PluginJobSettings,
};
use gents::plugin_job::view::{load_job_view, JobKey};
use gents::{ConfigAccess, DocumentRuntimeOptions, Gents, NodeIdentity};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnRuntime, AcceptedTurnSpec,
    PreparedAcceptedTurn,
};
use crate::support::fixtures::{
    agent_target, bind_agent_backend, configure_agent_tools, configure_child_agent,
};
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse,
};
use crate::support::{test_db, test_db_from_home, TestDb};

pub(crate) const PARENT: &str = "plugin-job-parent";
pub(crate) const CHILD: &str = "plugin-job-child";
pub(crate) const PACK: &str = "fixture/job_plugin_fixture";
pub(crate) const PLUGIN: &str = "fixture/job_fixture";
pub(crate) const MODEL: &str = "plugin-job-model";
const BACKEND_ID: &str = "plugin-job-backend";

/// One entry of the fixture guest's `args.plan` table.
pub(crate) struct PlanEntry {
    pub when: Value,
    pub output: JobStepOutput,
    pub note: bool,
    pub scratch: bool,
    pub control: Option<JobControlRequest>,
}

impl PlanEntry {
    pub(crate) fn to_json(&self) -> Value {
        let mut entry = json!({
            "when": self.when,
            "output": serde_json::to_value(&self.output).expect("serialize the step output"),
        });
        if self.note {
            entry["note"] = json!(true);
        }
        if self.scratch {
            entry["scratch"] = json!(true);
        }
        if let Some(control) = &self.control {
            entry["control"] = serde_json::to_value(control).expect("serialize the control");
        }
        entry
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packs/job_plugin_fixture")
}

pub(crate) fn guest_pin(handle: &str) -> JobGuestPin {
    let asset =
        std::fs::read(fixture_dir().join("fixture_assets/a.txt")).expect("read the fixture asset");
    JobGuestPin {
        script_id: "fixture".into(),
        source_digest: format!("sha256:{:x}", Sha256::digest(&asset)),
        agent_budget: 8,
        handle: handle.into(),
    }
}

pub(crate) fn spawn(slot: &str, prompt: &str) -> JobSpawn {
    JobSpawn {
        slot: slot.into(),
        attempt: 0,
        target: CHILD.into(),
        prompt: prompt.into(),
        title: slot.into(),
        cwd: None,
    }
}

pub(crate) fn effects(pin: Option<JobGuestPin>, spawn: Vec<JobSpawn>) -> JobStepOutput {
    JobStepOutput::Effects {
        effects: JobEffects {
            state: None,
            pin,
            resume: None,
            spawn,
            message: Vec::new(),
            interrupt: Vec::new(),
            progress: None,
            writes: Vec::new(),
            scratch: Vec::new(),
        },
    }
}

pub(crate) fn result(value: Value) -> JobStepOutput {
    JobStepOutput::Result {
        result: value,
        writes: Vec::new(),
        scratch: Vec::new(),
        progress: None,
        control: Vec::new(),
        pin: None,
        resume: None,
    }
}

pub(crate) fn job_settings() -> PluginJobSettings {
    PluginJobSettings {
        agent_targets: vec![CHILD.into()],
        max_live: PluginJobSettings::DEFAULT_MAX_LIVE,
        max_spawns: PluginJobSettings::DEFAULT_MAX_SPAWNS,
        max_messages_per_slot: PluginJobSettings::DEFAULT_MAX_MESSAGES_PER_SLOT,
        result_bytes: PluginJobSettings::DEFAULT_RESULT_BYTES,
        max_wall_secs: PluginJobSettings::DEFAULT_MAX_WALL_SECS,
        policy: JobPolicy {
            trusted_packs: vec![PACK.into()],
            ..JobPolicy::default()
        },
    }
}

pub(crate) fn job_call(tool_call_id: &str, plan: &[PlanEntry]) -> StreamChunk {
    let entries: Vec<Value> = plan.iter().map(PlanEntry::to_json).collect();
    StreamChunk::tool_call(
        tool_call_id,
        "job_fixture",
        json!({"args": {"plan": entries}}).to_string(),
    )
}

/// Installs the fixture pack through the document-pack install owners and
/// wires the parent and child agents around it.
pub(crate) async fn install_job_fixture(
    db: &TestDb,
    plugin_home: &Path,
    settings: PluginJobSettings,
) {
    let did = db.node_identity.did().to_string();
    gents::ensure_node(&db.node, &did)
        .await
        .expect("create the node document the pack install requires");
    let (bytes, _) = pack_dir(&fixture_dir()).expect("pack the job fixture");
    let archive = PackArchive::from_bytes(&bytes).expect("read the job fixture archive");
    let config = load_pack_config(
        archive.manifest(),
        &PackInstallOptions {
            node_did: did.clone(),
        },
        &|path| Ok(archive.asset(path)?.to_vec()),
        &|_| None,
    )
    .expect("load the job fixture pack config");
    let access = ConfigAccess::local(db.node.clone());
    let prepared = prepare_document_pack_install(
        &access,
        &did,
        archive.manifest(),
        &config,
        &PackInferenceBindings::new(),
        &[],
        InstalledVia::Operator,
        Some(plugin_home),
        &|_| None,
    )
    .await
    .expect("prepare the job fixture install");
    install_prepared_document_pack(
        &access,
        &did,
        &archive,
        &[],
        &prepared,
        Some(plugin_home),
        false,
        InstalledVia::Operator,
        DriftPolicy::Refuse,
    )
    .await
    .expect("install the job fixture pack");

    configure_child_agent(
        &db.node,
        &did,
        CHILD,
        &format!("{CHILD}:tools"),
        Vec::new(),
        false,
    )
    .await;
    configure_child_agent(
        &db.node,
        &did,
        PARENT,
        &format!("{PARENT}:tools"),
        Vec::new(),
        false,
    )
    .await;
    configure_agent_tools(
        &db.node,
        &did,
        PARENT,
        None,
        Tools {
            tools_id: format!("{PARENT}:tools"),
            node_did: did.clone(),
            agents: Some(AgentTools {
                target_ids: vec![CHILD.into()],
                enabled: Some(false),
            }),
            integrations: Some(IntegrationTools {
                plugins: Some(vec![PluginToolRef {
                    plugin: PLUGIN.into(),
                    digest: None,
                    tool_calls: false,
                    input_fields: Vec::new(),
                    job: Some(settings),
                }]),
                ..Default::default()
            }),
            ..Default::default()
        },
        vec![(
            gents::Collection::AgentTarget,
            serde_json::to_value(agent_target(&did, CHILD, &did, CHILD))
                .expect("serialize the child target"),
        )],
    )
    .await;
}

pub(crate) struct JobHarness {
    pub db: TestDb,
    pub plugin_home: PathBuf,
    pub runtime: AcceptedTurnRuntime,
    pub session_id: String,
}

async fn boot_runtime(
    db: &TestDb,
    plugin_home: &Path,
    prepared: PreparedAcceptedTurn,
) -> AcceptedTurnRuntime {
    let identity: std::sync::Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            plugin_home: Some(plugin_home.to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .expect("build the plugin job runtime");
    boot_prepared_accepted_turn(db, prepared, agent).await
}

pub(crate) async fn boot_job_harness(
    db: TestDb,
    plugin_home: PathBuf,
    session_id: &str,
    accepted_chunks: Vec<StreamChunk>,
    child_plans: Vec<StreamPlan>,
) -> JobHarness {
    install_job_fixture(&db, &plugin_home, job_settings()).await;
    // A child inherits the pin's cwd, and an empty one cannot be resolved.
    let cwd = db.data_path().join("job_cwd");
    std::fs::create_dir_all(&cwd).expect("create the job working folder");
    let cwd = cwd
        .canonicalize()
        .expect("resolve the job working folder")
        .to_string_lossy()
        .into_owned();
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: BACKEND_ID,
            model: MODEL,
            parent_agent_id: PARENT,
            configured_agent_ids: &[PARENT, CHILD],
            request_id: &format!("{session_id}-parent"),
            session_id,
            prompt: &format!("{session_id} start"),
            accepted_chunks,
            child_plans,
            valid_until: None,
            request_hop: None,
            request_setup: Some(Box::new(move |request| request.input.cwd = Some(cwd))),
        },
    )
    .await;
    let runtime = boot_runtime(&db, &plugin_home, prepared).await;
    JobHarness {
        db,
        plugin_home,
        runtime,
        session_id: session_id.into(),
    }
}

/// Boots a second runtime on the store and plugin home a killed worker left
/// behind. It installs nothing and writes no request.
pub(crate) async fn reboot_job_harness(
    data_path: &Path,
    plugin_home: PathBuf,
    session_id: &str,
    plans: Vec<StreamPlan>,
) -> JobHarness {
    let db = test_db_from_home(
        EmbeddedHome::open_retained(data_path)
            .await
            .expect("reopen the retained store"),
    );
    let backend =
        MockStreamingBackend::start_with_plans(MODEL, plans).expect("start the restart backend");
    for agent_id in [PARENT, CHILD] {
        bind_agent_backend(
            &db.node,
            db.node_identity.did(),
            agent_id,
            BACKEND_ID,
            backend.endpoint(),
            MODEL,
        )
        .await;
    }
    let runtime = boot_runtime(&db, &plugin_home, PreparedAcceptedTurn { backend }).await;
    JobHarness {
        db,
        plugin_home,
        runtime,
        session_id: session_id.into(),
    }
}

fn address(h: &JobHarness, tool_call_id: &str) -> JobKey {
    JobKey::Address {
        session_id: h.session_id.clone(),
        tool_call_id: tool_call_id.into(),
    }
}

pub(crate) async fn job_row(h: &JobHarness, tool_call_id: &str) -> String {
    load_job_view(
        &ConfigAccess::local(h.db.node.clone()),
        None,
        address(h, tool_call_id),
    )
    .await
    .expect("read the job view")
    .row_doc_id
}

pub(crate) async fn wait_job(
    h: &JobHarness,
    tool_call_id: &str,
    until: impl Fn(&JobView) -> bool,
    timeout: Duration,
) -> JobView {
    let access = ConfigAccess::local(h.db.node.clone());
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let last = load_job_view(&access, None, address(h, tool_call_id)).await;
        match &last {
            Ok(view) if until(view) => return last.unwrap(),
            Ok(_) | Err(JobReadError::NotYetObservable | JobReadError::NotAJob) => {}
            Err(error) => panic!("job {tool_call_id} is unreadable: {error:?}"),
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("job {tool_call_id} did not reach the awaited state; last read: {last:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scripted_job_adopts_one_child_and_completes() {
    let db = test_db("plugin-job-smoke").await;
    let plugin_home = tempfile::tempdir().expect("plugin home");
    let plan = [
        PlanEntry {
            when: json!({"step": 0}),
            output: effects(
                Some(guest_pin("job-1-handle")),
                vec![spawn("a", "job child a")],
            ),
            note: false,
            scratch: false,
            control: None,
        },
        PlanEntry {
            when: json!({"children": {"a": "completed"}}),
            output: result(json!("done")),
            note: false,
            scratch: false,
            control: None,
        },
        PlanEntry {
            when: json!({"children": {"a": "live"}}),
            output: effects(None, Vec::new()),
            note: false,
            scratch: false,
            control: None,
        },
    ];
    let h = boot_job_harness(
        db,
        plugin_home.path().to_path_buf(),
        "plugin-job-smoke-session",
        vec![job_call("job-1", &plan)],
        vec![StreamPlan::current_authored_user(
            "job child a",
            vec![StreamResponse::completes("job child a", ["child a done"])],
        )],
    )
    .await;

    let view = wait_job(
        &h,
        "job-1",
        |view| view.state == JobState::Completed,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(view.children.len(), 1, "{view:?}");
    assert_eq!(view.children[0].slot, "a");
    assert_eq!(view.children[0].state, JobChildState::Completed);

    let row_doc_id = job_row(&h, "job-1").await;
    let response = ConfigAccess::local(h.db.node.clone())
        .execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
            gents::graphql::escape_graphql_string(&row_doc_id),
        ))
        .await
        .expect("read the job row");
    let row = &response["data"]["AgentToolCall"][0];
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert!(!row["plugin_execution_receipt"].is_null(), "{row}");
    h.runtime.shutdown().await;
}
