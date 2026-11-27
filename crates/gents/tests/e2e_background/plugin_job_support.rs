use std::path::{Path, PathBuf};

use gents::{DocumentRuntimeOptions, Gents};
use gents_documents::{AgentTools, IntegrationTools, PluginToolRef, Tools};
use gents_pack::pack::archive::{pack_dir, PackArchive};
use gents_pack::pack::{
    install_prepared_document_pack, load_pack_config, prepare_document_pack_install, DriftPolicy,
    InstalledVia, PackInferenceBindings, PackInstallOptions,
};
use gents_store::{config_client::ConfigAccess, identity::NodeIdentity};
use gents_toolcalls::background::job::job::{
    JobControlRequest, JobEffects, JobGuestPin, JobPolicy, JobReadError, JobSpawn, JobStepOutput,
    JobView, PluginJobSettings,
};
use gents_toolcalls::background::job::view::{load_job_view, JobKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_managed_accepted_turn, AcceptedTurnRuntime,
    AcceptedTurnSpec, PreparedAcceptedTurn,
};
use crate::support::fixtures::{
    agent_target, bind_selected_managed_agent_backend, configure_agent_tools_as,
};
use crate::support::{managed_agent_fixture, trial_store_at, ManagedAgentFixture};
use gents_inference::test_support::scripted_backend::{ScriptedBackend, StreamChunk, StreamPlan};
use gents_store::test_support::TestStore;
use gents_store::test_support::STALL_CEILING;

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
    pub control: Option<JobControlRequest>,
}

impl PlanEntry {
    pub(crate) fn to_json(&self) -> Value {
        let mut entry = json!({
            "when": self.when,
            "output": serde_json::to_value(&self.output).expect("serialize the step output"),
        });
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

pub(crate) struct JobActors {
    pub fixture: ManagedAgentFixture,
    pub child_did: String,
}

pub(crate) async fn install_job_fixture(
    db: &TestStore,
    plugin_home: &Path,
    settings: PluginJobSettings,
) -> JobActors {
    let fixture = managed_agent_fixture(db).await;
    crate::support::grant_household_owner(db, &fixture.user).await;
    let (_, child) =
        crate::support::install_managed_agent(db, &fixture.access, &fixture.user, "Job child")
            .await;
    let did = db.node_identity.did().to_string();
    let parent_did = fixture.binding.agent_did.clone();
    let child_did = child.agent_did;
    let archive = tokio::task::spawn_blocking(|| {
        let source = fixture_dir();
        let staged = tempfile::tempdir().expect("stage the job fixture");
        let bytes = std::fs::read(source.join("manifest.json")).expect("read job manifest");
        let descriptor =
            gents_pack::pack::PackManifest::from_slice(&bytes).expect("decode job manifest");
        for relative in gents_pack::pack::declared_paths(&descriptor) {
            if relative == "manifest.json" {
                continue;
            }
            let destination = staged.path().join(&relative);
            std::fs::create_dir_all(destination.parent().unwrap()).expect("stage asset folder");
            std::fs::copy(source.join(&relative), destination).expect("stage job asset");
        }
        let mut manifest: Value = serde_json::from_slice(&bytes).expect("read job metadata");
        let args = manifest["plugins"][0]["input_schema"].take();
        manifest["plugins"][0]["input_schema"] = json!({
            "type": "object",
            "properties": {"args": args},
            "required": ["args"],
            "additionalProperties": false,
        });
        std::fs::write(
            staged.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .expect("stage model-facing job schema");
        let (bytes, _) = pack_dir(staged.path()).expect("pack the job fixture");
        PackArchive::from_bytes(&bytes).expect("read the job fixture archive")
    })
    .await
    .expect("prepare the model-facing job fixture");
    let config = load_pack_config(
        archive.manifest(),
        &PackInstallOptions {
            node_did: did.clone(),
        },
        &|path| Ok(archive.asset(path)?.to_vec()),
        &|_| None,
    )
    .expect("load the job fixture pack config");
    let access = &fixture.access;
    let prepared = prepare_document_pack_install(
        &gents_pack::pack::InstallTarget {
            access,
            owner: &did,
            plugin_home: Some(&gents_pack::plugin::store::StoreWriter::Offline(
                plugin_home.to_owned(),
            )),
        },
        archive.manifest(),
        &config,
        Some(&archive),
        &gents_pack::pack::PackGraphInputs {
            dependencies: &[],
            requested: &PackInferenceBindings::new(),
            environment: &|_| None,
            overrides: &Default::default(),
        },
        InstalledVia::Operator,
    )
    .await
    .expect("prepare the job fixture install");
    install_prepared_document_pack(
        &gents_pack::pack::InstallTarget {
            access,
            owner: &did,
            plugin_home: Some(&gents_pack::plugin::store::StoreWriter::Offline(
                plugin_home.to_owned(),
            )),
        },
        &archive,
        &[],
        &prepared,
        false,
        InstalledVia::Operator,
        gents_pack::pack::DocumentGraphPolicy {
            drift: DriftPolicy::Refuse,
            profiles: &gents_inference::resolved::ProviderProfilePolicy,
        },
    )
    .await
    .expect("install the job fixture pack");

    configure_agent_tools_as(
        access,
        &did,
        &child_did,
        None,
        Tools {
            tools_id: format!("{CHILD}:tools"),
            node_did: did.clone(),
            agents: Some(AgentTools {
                target_ids: Vec::new(),
                enabled: Some(false),
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    configure_agent_tools_as(
        access,
        &did,
        &parent_did,
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
            gents_store::collection::Collection::AgentTarget,
            serde_json::to_value(agent_target(&did, CHILD, &did, &child_did))
                .expect("serialize the child target"),
        )],
    )
    .await;
    JobActors { fixture, child_did }
}

pub(crate) struct JobHarness {
    pub db: TestStore,
    pub actors: JobActors,
    pub plugin_home: PathBuf,
    pub runtime: AcceptedTurnRuntime,
    pub session_id: String,
}

impl JobHarness {
    pub fn agent_access(&self) -> ConfigAccess {
        ConfigAccess::local_managed_agent(
            self.db.node.clone(),
            ::identity::Did::new(self.actors.fixture.agent.did()).unwrap(),
            self.actors.fixture.agent.as_ref(),
        )
        .expect("managed job Agent access")
    }
}

async fn boot_runtime(
    db: &TestStore,
    plugin_home: &Path,
    prepared: PreparedAcceptedTurn,
) -> AcceptedTurnRuntime {
    let identity: std::sync::Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.backend_admission()),
            plugin_home: Some(plugin_home.to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .expect("build the plugin job runtime");
    boot_prepared_accepted_turn(db, prepared, agent).await
}

async fn bind_stream_liveness_timeout_secs(
    db: &TestStore,
    fixture: &ManagedAgentFixture,
    agent_dids: &[&str],
    liveness_secs: i64,
) {
    use gents_documents::{
        apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
        DesiredStateApplyPlan,
    };
    use gents_store::collection::Collection;

    let owner = db.node_identity.did();
    for agent_did in agent_dids {
        let execution_id = format!("{agent_did}:liveness-execution");
        fixture
            .access
            .transact_signed_conflict_rebuild("test.job_liveness_execution", |txn| {
                let execution_id = &execution_id;
                Box::pin(async move {
                    let (_, agent) =
                        read_desired_state_record_in_txn(txn, Collection::Agent, owner, agent_did)
                            .await?
                            .expect("managed job Agent must exist");
                    let profile_id = agent["inference_profile_id"]
                        .as_str()
                        .expect("managed Agent inference profile");
                    let (_, mut profile) = read_desired_state_record_in_txn(
                        txn,
                        Collection::InferenceProfile,
                        owner,
                        profile_id,
                    )
                    .await?
                    .expect("managed Agent inference profile document must exist");
                    profile["execution_id"] = json!(execution_id);
                    let execution = json!({
                        "node_did": owner,
                        "execution_id": execution_id,
                        "stream_liveness_timeout_secs": liveness_secs,
                    });
                    let plan = DesiredStateApplyPlan::new(vec![
                        DesiredStateApplyDocument {
                            collection: Collection::InferenceExecution,
                            add: execution.clone(),
                            update: execution,
                        },
                        DesiredStateApplyDocument {
                            collection: Collection::InferenceProfile,
                            add: profile.clone(),
                            update: profile,
                        },
                    ])?;
                    apply_desired_state_plan(
                        txn,
                        &plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .expect("bind job agent stream liveness timeout");
    }
}

pub(crate) async fn boot_job_harness(
    db: TestStore,
    plugin_home: PathBuf,
    session_id: &str,
    accepted_chunks: Vec<StreamChunk>,
    child_plans: Vec<StreamPlan>,
) -> JobHarness {
    boot_job_harness_with_liveness(
        db,
        plugin_home,
        session_id,
        accepted_chunks,
        child_plans,
        None,
    )
    .await
}

pub(crate) async fn boot_job_harness_with_liveness(
    db: TestStore,
    plugin_home: PathBuf,
    session_id: &str,
    accepted_chunks: Vec<StreamChunk>,
    child_plans: Vec<StreamPlan>,
    stream_liveness_timeout_secs: Option<i64>,
) -> JobHarness {
    let actors = install_job_fixture(&db, &plugin_home, job_settings()).await;
    // A child inherits the pin's cwd, and an empty one cannot be resolved.
    let cwd = db.path().join("job_cwd");
    std::fs::create_dir_all(&cwd).expect("create the job working folder");
    let cwd = cwd
        .canonicalize()
        .expect("resolve the job working folder")
        .to_string_lossy()
        .into_owned();
    let prepared = prepare_managed_accepted_turn(
        &db,
        &actors.fixture,
        AcceptedTurnSpec {
            backend_id: BACKEND_ID,
            model: MODEL,
            parent_agent_did: &actors.fixture.binding.agent_did,
            configured_agent_dids: &[&actors.fixture.binding.agent_did, &actors.child_did],
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
    if let Some(liveness_secs) = stream_liveness_timeout_secs {
        bind_stream_liveness_timeout_secs(
            &db,
            &actors.fixture,
            &[&actors.fixture.binding.agent_did, &actors.child_did],
            liveness_secs,
        )
        .await;
    }
    let runtime = boot_runtime(&db, &plugin_home, prepared).await;
    JobHarness {
        db,
        actors,
        plugin_home,
        runtime,
        session_id: session_id.into(),
    }
}

/// Reads every child `AgentRequest` row for `row_doc_id` sorted by `_docID`.
pub(crate) async fn job_child_states(
    access: &ConfigAccess,
    row_doc_id: &str,
) -> Vec<(String, String)> {
    let response = access
        .clone()
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(row_doc_id),
        ))
        .await
        .expect("read the job children");
    let mut states: Vec<(String, String)> = response["data"]["AgentRequest"]
        .as_array()
        .expect("children array")
        .iter()
        .map(|row| {
            (
                row["_docID"].as_str().expect("child doc id").to_string(),
                row["lifecycle_state"]
                    .as_str()
                    .expect("child state")
                    .to_string(),
            )
        })
        .collect();
    states.sort();
    states
}

/// Boots a second runtime on the store and plugin home a killed worker left
/// behind, returning the durable child request states observed before the
/// recovery runtime starts. It installs nothing and writes no request.
pub(crate) async fn reboot_job_harness_observing_children(
    data_path: &Path,
    plugin_home: PathBuf,
    session_id: &str,
    row_doc_id: &str,
    plans: Vec<StreamPlan>,
) -> (JobHarness, Vec<(String, String)>) {
    let db = trial_store_at(data_path).await;
    let user = crate::support::load_managed_fixture_user(&db).await;
    let access = db.client_access(&user);
    // Snapshot crashed child states after the worker is dead and before
    // startup recovery runs (modeled in PluginJobCrashObservation.tla).
    let pre_reboot_children = job_child_states(&access, row_doc_id).await;
    let response = access.execute("{ DefaultAgentSelection { node_did default_agent_did } Agent { node_did agent_did creation_nonce display_name } }").await.expect("read retained job identities");
    let rows = response["data"]["Agent"]
        .as_array()
        .expect("retained job Agents");
    let parent_did = response["data"]["DefaultAgentSelection"]
        .as_array()
        .expect("retained default agent selection")
        .iter()
        .find(|selection| selection["node_did"] == db.node_identity.did())
        .and_then(|selection| selection["default_agent_did"].as_str())
        .expect("retained default Agent");
    let parent = rows
        .iter()
        .find(|row| row["agent_did"] == parent_did)
        .expect("retained parent Agent");
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: parent_did.into(),
        creation_nonce: parent["creation_nonce"]
            .as_str()
            .expect("parent creation nonce")
            .into(),
    };
    let agent = gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .expect("resolve retained parent Agent");
    let child_did = rows
        .iter()
        .find(|row| row["node_did"] == db.node_identity.did() && row["display_name"] == "Job child")
        .and_then(|row| row["agent_did"].as_str())
        .expect("retained child Agent")
        .to_owned();
    let actors = JobActors {
        fixture: ManagedAgentFixture {
            user,
            access,
            agent,
            binding,
        },
        child_did,
    };
    let backend =
        ScriptedBackend::start_with_plans(MODEL, plans).expect("start the restart backend");
    for agent_did in [&actors.fixture.binding.agent_did, &actors.child_did] {
        bind_selected_managed_agent_backend(
            &db,
            &actors.fixture,
            agent_did,
            agent_did == &actors.fixture.binding.agent_did,
            BACKEND_ID,
            backend.endpoint(),
            MODEL,
        )
        .await;
    }
    let runtime = boot_runtime(&db, &plugin_home, PreparedAcceptedTurn { backend }).await;
    (
        JobHarness {
            db,
            actors,
            plugin_home,
            runtime,
            session_id: session_id.into(),
        },
        pre_reboot_children,
    )
}

fn address(h: &JobHarness, tool_call_id: &str) -> JobKey {
    JobKey::Address {
        session_id: h.session_id.clone(),
        tool_call_id: tool_call_id.into(),
    }
}

pub(crate) async fn job_row(h: &JobHarness, tool_call_id: &str) -> String {
    load_job_view(&h.actors.fixture.access, None, address(h, tool_call_id))
        .await
        .expect("read the job view")
        .row_doc_id
}

/// Fails once `AgentToolCall` has not changed for [`STALL_CEILING`].
pub(crate) async fn wait_job(
    h: &JobHarness,
    tool_call_id: &str,
    until: impl Fn(&JobView) -> bool,
) -> JobView {
    let access = h.actors.fixture.access.clone();
    use gents_store::test_support::snapshot_reader::SnapshotReader;
    let mut changes = access.subscribe_changes("AgentToolCall").await;
    loop {
        let last = load_job_view(&access, None, address(h, tool_call_id)).await;
        match &last {
            Ok(view) if until(view) => return last.unwrap(),
            Ok(_) | Err(JobReadError::NotYetObservable | JobReadError::NotAJob) => {}
            Err(error) => panic!("job {tool_call_id} is unreadable: {error:?}"),
        }
        tokio::time::timeout(STALL_CEILING, changes.recv())
            .await
            .unwrap_or_else(|_| {
                panic!("job {tool_call_id} made no progress for {STALL_CEILING:?}; last read: {last:?}")
            });
    }
}
