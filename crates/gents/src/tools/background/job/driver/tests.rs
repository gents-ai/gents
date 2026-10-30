use super::*;
use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::tools::background::job::records::read_progress;
use crate::tools::call::lifecycle::admission_fixture::{
    published_admission, PublishedAdmission, PublishedAdmissionOptions,
};
use crate::tools::call::lifecycle::AwaitMode;
use gents_protocol::job::{
    JobControlKind, JobControlOutcome, JobFailure, JobPauseReason, JobPin, JobProgress, JobState,
    JobStepInput, JobSuspend, JobSuspendKind,
};
use gents_protocol::plugin::{PluginExecutionReceipt, PluginExecutionVerdict};
use serde_json::{json, Value};

use super::super::admit::Refusal;
use super::super::admit::StepCall;
use super::super::control::control_outcome;
use super::super::input::{build_step_input, StepKind};
use super::super::records::read_step_state;
use super::super::records::write_step_state_in_txn;
use super::settle::terminal;
use super::step::{step_on, STEP_TRANSACTION_RUNS};
use crate::plugin::data::{DataCaller, StampContext};
use crate::plugin::job_calls::JobCaller;
use crate::plugin::store::InstalledPlugin;
use crate::tools::background::status::ProcessControlScope;
use crate::tools::call::lifecycle::ToolCallLifecycle;
use std::path::Path;

const HOST_REFUSALS: [Refusal; 6] = [
    Refusal::Stalled,
    Refusal::PolicyDenied,
    Refusal::NotAvailable,
    Refusal::EngineChanged,
    Refusal::DepthExceeded,
    Refusal::StepFailed,
];

fn evidence(input: &str) -> PluginExecutionReceipt {
    PluginExecutionReceipt {
        coordinate: "fixture/engine".into(),
        artifact_digest: "sha256:engine".into(),
        input_digest: input.into(),
        output_digest: Some("sha256:output".into()),
        authority: None,
        limits: None,
        verdict: PluginExecutionVerdict::Success,
    }
}

/// A running background job row with its invocation receipt published and
/// no stored progress.
async fn bare_job_row(name: &str) -> PublishedAdmission {
    let mut admission = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        job_pin: true,
        ..Default::default()
    })
    .await
    .unwrap();
    assert!(admission
        .tool
        .publish_background_receipt("started")
        .await
        .unwrap());
    admission
}

/// A running job row with its stored progress at revision 1 and the last
/// step's evidence staged.
async fn job_row(name: &str) -> PublishedAdmission {
    let mut admission = bare_job_row(name).await;
    store_running_progress(&admission).await;
    admission.tool.stage_plugin_receipt(Some(evidence("step")));
    admission
}

async fn finish(admission: PublishedAdmission) {
    admission.node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}

async fn stored_row(admission: &PublishedAdmission) -> Value {
    let response = ConfigAccess::local(admission.node.clone())
        .execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
            escape_graphql_string(admission.tool.doc_id().unwrap()),
        ))
        .await
        .unwrap();
    response["data"]["AgentToolCall"][0].clone()
}

/// The row's closed canonical tool output.
async fn stored_output(admission: &PublishedAdmission) -> String {
    let tool = &admission.tool;
    match crate::tools::background::status::observe_canonical_tool_output_with_access(
        &ConfigAccess::local(admission.node.clone()),
        tool.doc_id().unwrap(),
        tool.request_doc_id().unwrap(),
        tool.session_id(),
        tool.node_did(),
        tool.requester_did(),
    )
    .await
    .unwrap()
    {
        crate::tools::background::status::CanonicalToolOutputObservation::Closed(output) => output,
        crate::tools::background::status::CanonicalToolOutputObservation::Open(output) => {
            panic!("job row output is still open: {output}")
        }
    }
}

async fn stored_progress(admission: &PublishedAdmission) -> JobProgress {
    read_progress(
        &ConfigAccess::local(admission.node.clone()),
        admission.tool.doc_id().unwrap(),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn settled(name: &str, settlement: Settlement) -> (PublishedAdmission, Value) {
    let mut admission = job_row(name).await;
    settle_plugin_job(&mut admission.tool, settlement)
        .await
        .unwrap();
    let row = stored_row(&admission).await;
    (admission, row)
}

fn payload(output: &str) -> Value {
    serde_json::from_str(output).unwrap()
}

#[tokio::test]
async fn result_settles_completed() {
    let (admission, row) =
        settled("driver-result", Settlement::Result(json!({"findings": 3}))).await;
    assert_eq!(row["lifecycle_state"], "completed");
    assert_eq!(
        payload(&stored_output(&admission).await),
        json!({"findings": 3})
    );
    finish(admission).await;
}

#[tokio::test]
async fn fail_settles_failed_with_the_guest_code() {
    let (admission, row) = settled(
        "driver-fail",
        Settlement::Fail(JobFailure {
            code: "audit_rejected".to_owned(),
            message: "the audit found a blocker".to_owned(),
        }),
    )
    .await;
    assert_eq!(row["lifecycle_state"], "failed");
    let output = payload(&stored_output(&admission).await);
    assert_eq!(output["status"], "failed");
    assert_eq!(output["code"], "audit_rejected");
    finish(admission).await;
}

#[tokio::test]
async fn suspend_pause_settles_completed_paused_with_pause_reason() {
    let (admission, row) = settled(
        "driver-pause",
        Settlement::Suspend(JobSuspend {
            kind: JobSuspendKind::Pause,
            reason: Some(JobPauseReason::BackOff),
            message: "waiting for the rate limit".to_owned(),
            question: None,
        }),
    )
    .await;
    assert_eq!(row["lifecycle_state"], "completed");
    assert_eq!(
        payload(&stored_output(&admission).await)["status"],
        "paused"
    );
    let progress = stored_progress(&admission).await;
    assert_eq!(progress.state, JobState::Paused);
    assert_eq!(progress.pause_reason, Some(JobPauseReason::BackOff));
    assert_eq!(progress.revision, 2);
    assert_eq!(progress.spawned, 2);
    assert_eq!(progress.observed_terminal_children, 1);
    assert_eq!(progress.last_event, "scanning");
    finish(admission).await;
}

#[tokio::test]
async fn pause_or_await_without_stored_progress_fails_and_leaves_the_row_running() {
    for settlement in [
        Settlement::Control(JobControlKind::Pause),
        Settlement::Suspend(JobSuspend {
            kind: JobSuspendKind::AwaitUser,
            reason: None,
            message: "needs a decision".to_owned(),
            question: Some("ship it?".to_owned()),
        }),
    ] {
        let mut admission = bare_job_row("driver-no-progress").await;
        let error = settle_plugin_job(&mut admission.tool, settlement.clone())
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("no stored progress"),
            "{settlement:?}: {error:#}"
        );
        assert_eq!(
            stored_row(&admission).await["lifecycle_state"],
            "running",
            "{settlement:?}"
        );
        finish(admission).await;
    }
}

#[tokio::test]
async fn await_user_settles_completed_awaiting_user_with_the_question() {
    let (admission, row) = settled(
        "driver-await",
        Settlement::Suspend(JobSuspend {
            kind: JobSuspendKind::AwaitUser,
            reason: None,
            message: "needs a decision".to_owned(),
            question: Some("ship it?".to_owned()),
        }),
    )
    .await;
    assert_eq!(row["lifecycle_state"], "completed");
    let output = payload(&stored_output(&admission).await);
    assert_eq!(output["status"], "awaiting_user");
    assert_eq!(output["question"], "ship it?");
    assert_eq!(
        stored_progress(&admission).await.state,
        JobState::AwaitingUser
    );
    finish(admission).await;
}

#[tokio::test]
async fn host_refusals_settle_failed_with_their_plugin_job_code() {
    for refusal in HOST_REFUSALS {
        let name = format!("driver-refused-{}", refusal.code().unwrap());
        let (admission, row) = settled(&name, Settlement::Refused(refusal)).await;
        assert_eq!(row["lifecycle_state"], "failed", "{refusal:?}");
        let output = payload(&stored_output(&admission).await);
        assert_eq!(output["status"], "failed", "{refusal:?}");
        assert_eq!(output["code"], refusal.code().unwrap(), "{refusal:?}");
        finish(admission).await;
    }
}

#[tokio::test]
async fn max_wall_secs_settles_timed_out() {
    let (admission, row) =
        settled("driver-timed-out", Settlement::Refused(Refusal::TimedOut)).await;
    assert_eq!(row["lifecycle_state"], "timedOut");
    assert_eq!(
        payload(&stored_output(&admission).await)["code"],
        "plugin_job_timed_out"
    );
    finish(admission).await;
}

/// The terminal stamp never fails a host kill: a row whose guest progress
/// sits just under the cap still settles.
#[tokio::test]
async fn a_host_kill_settles_a_row_whose_progress_sits_just_under_the_cap() {
    use super::super::records::{write_progress_in_txn, HostProgressFacts};
    use gents_protocol::job::{JobPhase, JobPhaseState, JOB_PROGRESS_MAX_BYTES};
    let mut admission = job_row("driver-full-progress").await;
    let mut full = stored_progress(&admission).await;
    full.last_event = String::new();
    let phase = JobPhase {
        title: "p".repeat(200),
        state: JobPhaseState::Done,
    };
    let bytes = |progress: &JobProgress| {
        serde_json::to_vec(&JobProgress {
            revision: progress.revision + 1,
            ..progress.clone()
        })
        .unwrap()
        .len()
    };
    while bytes(&full) + 250 < JOB_PROGRESS_MAX_BYTES {
        full.phases.push(phase.clone());
    }
    full.title
        .push_str(&"t".repeat(JOB_PROGRESS_MAX_BYTES - bytes(&full) - 4));
    let doc_id = admission.tool.doc_id().unwrap().to_owned();
    ConfigAccess::transact_local_write(&admission.node, None, "test.driver.full", |txn| {
        let (doc_id, full) = (doc_id.clone(), full.clone());
        Box::pin(async move {
            let host = HostProgressFacts {
                spawned: full.spawned,
                budget: full.budget.clone(),
                observed_terminal_children: full.observed_terminal_children,
                state: full.state,
                pause_reason: None,
            };
            write_progress_in_txn(txn, &doc_id, full.revision, full, host).await
        })
    })
    .await
    .unwrap();
    settle_plugin_job(&mut admission.tool, Settlement::Refused(Refusal::TimedOut))
        .await
        .unwrap();
    assert_eq!(stored_row(&admission).await["lifecycle_state"], "timedOut");
    finish(admission).await;
}

#[tokio::test]
async fn stop_control_settles_cancelled_and_pause_control_settles_paused_by_user() {
    let (stopped, row) = settled(
        "driver-control-stop",
        Settlement::Control(JobControlKind::Stop),
    )
    .await;
    assert_eq!(row["lifecycle_state"], "cancelled");
    finish(stopped).await;

    let (paused, row) = settled(
        "driver-control-pause",
        Settlement::Control(JobControlKind::Pause),
    )
    .await;
    assert_eq!(row["lifecycle_state"], "completed");
    assert_eq!(payload(&stored_output(&paused).await)["status"], "paused");
    let progress = stored_progress(&paused).await;
    assert_eq!(progress.state, JobState::Paused);
    assert_eq!(progress.pause_reason, Some(JobPauseReason::User));
    finish(paused).await;
}

#[test]
fn host_code_payloads_name_their_next_call() {
    for refusal in HOST_REFUSALS.into_iter().chain([Refusal::TimedOut]) {
        let terminal = terminal(Settlement::Refused(refusal), "jobs", "session-a:call-1").unwrap();
        assert_eq!(terminal.output["code"], refusal.code().unwrap());
        let next = terminal.output["next"].as_str().unwrap();
        assert!(next.starts_with("Next: "), "{refusal:?}: {next}");
        assert!(next.contains("jobs"), "{refusal:?}: {next}");
    }
    assert_eq!(
        terminal(Settlement::Refused(Refusal::Terminal), "jobs", "s:c"),
        None
    );
}

#[tokio::test]
async fn settled_job_row_holds_exactly_one_receipt() {
    let (admission, row) = settled("driver-receipt", Settlement::Result(json!("done"))).await;
    let stored: PluginExecutionReceipt =
        serde_json::from_value(row["plugin_execution_receipt"].clone()).unwrap();
    assert_eq!(stored, evidence("step"));

    let mut late = crate::tools::call::lifecycle::ToolCallLifecycle::load_by_doc_id(
        admission.node.clone(),
        admission.tool.doc_id().unwrap(),
        admission.tool.node_did(),
        admission.tool.session_id(),
        admission.tool.requester_did(),
    )
    .await
    .unwrap()
    .unwrap();
    // A late settler adopts the durable terminal and writes no second receipt.
    late.stage_plugin_receipt(Some(evidence("another step")));
    settle_plugin_job(&mut late, Settlement::Result(json!("again")))
        .await
        .unwrap();
    let row = stored_row(&admission).await;
    assert_eq!(row["lifecycle_state"], "completed");
    assert_eq!(
        serde_json::from_value::<PluginExecutionReceipt>(row["plugin_execution_receipt"].clone())
            .unwrap(),
        evidence("step")
    );
    finish(admission).await;
}

#[tokio::test]
async fn running_job_row_holds_no_receipt() {
    let (admission, row) = settled("driver-running", Settlement::Refused(Refusal::Terminal)).await;
    assert_eq!(row["lifecycle_state"], "running");
    assert!(row["plugin_execution_receipt"].is_null());
    finish(admission).await;
}

#[tokio::test]
async fn non_inline_settlement_appends_one_continuation_completion() {
    let (admission, _) = settled(
        "driver-completion",
        Settlement::Fail(JobFailure {
            code: "audit_rejected".to_owned(),
            message: "the audit found a blocker".to_owned(),
        }),
    )
    .await;
    let access = ConfigAccess::local(admission.node.clone());
    let key = crate::tools::background::completion::background_completion_notification_message_key(
        admission.tool.doc_id().unwrap(),
        "tool",
    );
    let notices = access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&key),
        ))
        .await
        .unwrap();
    assert_eq!(notices["data"]["AgentMessage"].as_array().unwrap().len(), 1);
    let requests = access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ request_id request_hop }} }}"#,
                escape_graphql_string(admission.tool.session_id()),
            ))
            .await
            .unwrap();
    let requests = requests["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(requests.len(), 2, "the caller's request and one wake");
    assert!(
        requests
            .iter()
            .all(|request| request["request_hop"] == requests[0]["request_hop"]),
        "a Continuation wake keeps the caller's hop: {requests:?}"
    );
    finish(admission).await;
}

async fn persist_step_state(admission: &PublishedAdmission, evidence: &PluginExecutionReceipt) {
    let doc_id = admission.tool.doc_id().unwrap().to_owned();
    ConfigAccess::transact_local_write(&admission.node, None, "test.driver.step_state", |txn| {
        let doc_id = doc_id.clone();
        Box::pin(async move {
            super::super::records::write_step_state_in_txn(txn, &doc_id, evidence, None, None).await
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn settle_of_a_lifecycle_loaded_by_doc_id_publishes_the_persisted_evidence() {
    let admission = bare_job_row("driver-persisted-evidence").await;
    store_running_progress(&admission).await;
    persist_step_state(&admission, &evidence("persisted")).await;
    let mut loaded = crate::tools::call::lifecycle::ToolCallLifecycle::load_by_doc_id(
        admission.node.clone(),
        admission.tool.doc_id().unwrap(),
        admission.tool.node_did(),
        admission.tool.session_id(),
        admission.tool.requester_did(),
    )
    .await
    .unwrap()
    .unwrap();
    settle_plugin_job(&mut loaded, Settlement::Refused(Refusal::Stalled))
        .await
        .unwrap();
    let row = stored_row(&admission).await;
    assert_eq!(row["lifecycle_state"], "failed");
    let stored: PluginExecutionReceipt =
        serde_json::from_value(row["plugin_execution_receipt"].clone()).unwrap();
    assert_eq!(
        stored,
        PluginExecutionReceipt {
            output_digest: None,
            verdict: PluginExecutionVerdict::ExecutionError,
            ..evidence("persisted")
        }
    );
    finish(admission).await;
}

#[tokio::test]
async fn staged_evidence_wins_over_the_persisted_one() {
    let mut admission = job_row("driver-staged-wins").await;
    persist_step_state(&admission, &evidence("persisted")).await;
    admission
        .tool
        .stage_plugin_receipt(Some(evidence("staged")));
    settle_plugin_job(&mut admission.tool, Settlement::Result(json!("ok")))
        .await
        .unwrap();
    let row = stored_row(&admission).await;
    assert_eq!(
        serde_json::from_value::<PluginExecutionReceipt>(row["plugin_execution_receipt"].clone())
            .unwrap(),
        evidence("staged")
    );
    finish(admission).await;
}

use crate::tools::background::job::input::tests::{live_settings, new_job, request, resume_block};
use crate::tools::background::job::keys::{job_session_id, job_spawn_key};
use crate::tools::background::job::records::{write_progress_in_txn, HostProgressFacts};
use gents_protocol::job::{JobBudget, JobPolicy};

fn running_progress() -> JobProgress {
    JobProgress {
        title: "audit".to_owned(),
        state: JobState::Running,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 0,
        budget: JobBudget {
            total: 0,
            spent: 0,
            remaining: 0,
        },
        last_event: "scanning".to_owned(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 0,
    }
}

#[tokio::test]
async fn read_step_row_carries_the_pin_progress_and_derived_children() {
    let live = live_settings(JobPolicy::default());
    let mut job = new_job(Some(&live), None).await;
    let row = job
        .row(
            "workflow",
            "call-1",
            json!({"argv": ["run"], "args": {"n": 1}, "agent_budget": 7}),
        )
        .await;
    let pin = job.pin(&row, None);
    job.write_pin(&row, &pin).await;
    job.write_progress(&row, JobState::Running, 1).await;
    request(
        &job.node,
        &row,
        "child-1",
        &job_session_id(&row, "slot-a", 0),
        &job_spawn_key(&row, "slot-a", 0),
        "agent-a",
        "processing",
        Some("Alpha"),
        "2026-01-01T00:00:00Z",
        None,
    )
    .await;
    let step = read_step_row(&job.access, &row).await.unwrap();
    let progress = read_progress(&job.access, &row).await.unwrap().unwrap();
    assert_eq!(step.pin, Some(pin.clone()));
    assert_eq!(step.root, pin.chain_root_doc_id);
    assert_eq!(step.root, row);
    assert_eq!(step.row_doc_id, row);
    assert_eq!(step.caller_session, "session-1");
    assert_eq!(step.caller_cwd, pin.cwd);
    assert_eq!(step.handle, pin.handle);
    assert_eq!(step.caller_hop, 0);
    assert_eq!(step.call.argv, ["run"]);
    assert_eq!(step.call.args.as_deref(), Some(r#"{"n":1}"#));
    assert_eq!(step.call.agent_budget, Some(7));
    assert_eq!(step.root_call, None);
    assert_eq!(step.spent, progress.spawned);
    assert_eq!(step.spent, 1);
    assert_eq!(step.observed_terminal, progress.observed_terminal_children);
    assert_eq!(step.state, JobState::Running);
    let derived = crate::tools::background::job::input::derive_children(
        &job.access,
        std::slice::from_ref(&row),
        pin.ceiling_at_pin.result_bytes,
    )
    .await
    .unwrap();
    assert_eq!(derived.len(), 1);
    assert_eq!(
        step.children,
        derived
            .into_iter()
            .map(|(child, _)| child)
            .collect::<Vec<_>>()
    );
    assert_eq!(step.resume, None);

    let block = resume_block("prior");
    job.write_step_state(&row, Some(&block)).await;
    let step = read_step_row(&job.access, &row).await.unwrap();
    assert_eq!(step.resume, Some(block));
    job.node.shutdown().await;
}

#[tokio::test]
async fn read_step_row_in_txn_sees_the_transactions_own_writes() {
    let live = live_settings(JobPolicy::default());
    let mut job = new_job(Some(&live), None).await;
    let row = job
        .row("workflow", "call-1", json!({"argv": ["run"]}))
        .await;
    job.write_pin(&row, &job.pin(&row, None)).await;
    job.write_progress(&row, JobState::Running, 1).await;
    let row = row.as_str();
    let spent =
        ConfigAccess::transact_local_write(&job.node, None, "test.driver.own_writes", |txn| {
            Box::pin(async move {
                let host = HostProgressFacts {
                    spawned: 3,
                    budget: JobBudget {
                        total: 4,
                        spent: 3,
                        remaining: 1,
                    },
                    observed_terminal_children: 0,
                    state: JobState::Running,
                    pause_reason: None,
                };
                write_progress_in_txn(txn, row, 1, running_progress(), host).await?;
                Ok::<_, anyhow::Error>(read_step_row_in_txn(txn, row).await?.spent)
            })
        })
        .await
        .unwrap();
    assert_eq!(spent, 3);
    job.node.shutdown().await;
}

use crate::pack::{write_home_install, HomePackInstall, PackKind};
use crate::plugin::store;
use crate::plugin::tests::constant_output_wat;
use crate::plugin::tests::executor::installed_plugin;
use crate::tools::background::job::input::tests::{insert, node_did, Job};
use gents_protocol::job::{
    DynamicMode, JobChildState, JobOrigin, PluginJobDeclaration, PluginJobSettings,
};

/// A running job row of `acme/engine` (dynamic origin, under a policy that
/// runs dynamic code, progress at revision 1) whose installed guest returns
/// one output on every step.
pub(crate) struct Stepper {
    pub(crate) job: Job,
    pub(crate) row: String,
    installed: InstalledPlugin,
}

fn step_settings(targets: &[&str]) -> PluginJobSettings {
    let ids: Vec<String> = targets.iter().map(|target| (*target).to_owned()).collect();
    let mut settings = live_settings(JobPolicy {
        dynamic: DynamicMode::Run,
        dynamic_targets: ids.clone(),
        ..Default::default()
    });
    settings.agent_targets = ids;
    settings
}

/// The inference chain the job fixture's Agent names, so its Tools can be
/// rewritten under reference validation.
async fn write_inference(job: &Job) {
    use crate::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let documents = [
        (
            crate::Collection::InferenceProfile,
            json!({"node_did": node_did(), "profile_id": "inference", "backend_id": "backend",
                       "model_name": "test-model"}),
        ),
        (
            crate::Collection::InferenceBackend,
            json!({"node_did": node_did(), "backend_id": "backend", "name": "Test inference",
                       "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                       "auth": {"kind": "unauthenticated"}}),
        ),
    ]
    .into_iter()
    .map(|(collection, value)| DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    })
    .collect();
    let plan = DesiredStateApplyPlan::new(documents).unwrap();
    ConfigAccess::transact_local_write(&job.node, None, "test.driver.inference", |txn| {
        let plan = &plan;
        Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
}

pub(crate) async fn write_step_tools(job: &Job, settings: Option<&PluginJobSettings>) {
    let tools: crate::documents::Tools = serde_json::from_value(json!({
        "tools_id": "tools",
        "node_did": node_did(),
        "agents": {"enabled": true, "target_ids": ["t-a", "t-b"]},
        "integrations": {"plugins": [{"plugin": "acme/engine", "job": settings}]},
    }))
    .unwrap();
    crate::config_client::write_tools_document(&job.access, &tools)
        .await
        .unwrap();
}

/// Installs `output` as the guest of `acme/engine` under the job's home,
/// recorded as an operator install.
fn install_engine(job: &Job, output: &Value) -> InstalledPlugin {
    install_engine_wat(job, &constant_output_wat(output.to_string().as_bytes()))
}

fn install_engine_wat(job: &Job, wat: &str) -> InstalledPlugin {
    let (source, record) = installed_plugin(wat, None);
    let hex = record.digest.strip_prefix("sha256:").unwrap();
    let home = &job.runtime.home;
    store::store_bytes(home, hex, &store::read_bytes(source.path(), hex).unwrap()).unwrap();
    let record = InstalledPlugin {
        namespace: "acme".into(),
        name: "engine".into(),
        owner_pack_coordinate: Some("acme/engine".into()),
        declaration: crate::pack::PackPlugin {
            job: PluginJobDeclaration(true),
            ..record.declaration.clone()
        },
        ..record
    };
    store::write_record(home, &record).unwrap();
    write_home_install(
        home,
        &HomePackInstall {
            coordinate: "acme/engine".into(),
            version: "1.0.0".into(),
            digest: record.digest.clone(),
            kind: PackKind::Plugins,
            assets: String::new(),
            dependency_assets: vec![],
            plugins: vec![],
            ux: Vec::new(),
            installed_at: "2026-01-01T00:00:00Z".into(),
            installed_via: Some(crate::pack::InstalledVia::Operator),
        },
    )
    .unwrap();
    record
}

pub(crate) async fn start_running(job: &Job, row: &str) {
    let mut lifecycle =
        ToolCallLifecycle::load_by_doc_id(job.node.clone(), row, node_did(), "session-1", None)
            .await
            .unwrap()
            .unwrap();
    lifecycle.start_running().await.unwrap();
}

/// A running pinned job row of `job` (its own chain root), progress at revision 1.
pub(crate) async fn pinned_job_row(job: &mut Job, call_id: &str) -> String {
    let row = job.row("workflow", call_id, json!({"argv": ["run"]})).await;
    job.write_pin(&row, &job.pin(&row, None)).await;
    job.write_progress(&row, JobState::Running, 1).await;
    start_running(job, &row).await;
    row
}

/// Ends `row` with `settlement` through the production settle owner.
pub(crate) async fn settle_row(job: &Job, row: &str, settlement: Settlement) {
    let mut lifecycle = load_lifecycle(&job.access, row).await.unwrap();
    settle_plugin_job(&mut lifecycle, settlement).await.unwrap();
}

#[tokio::test]
async fn read_step_row_state_follows_the_row_lifecycle_first() {
    let mut job = new_job(Some(&live_settings(JobPolicy::default())), None).await;
    let cases = [
        (
            Some(Settlement::Fail(JobFailure {
                code: "engine_boom".into(),
                message: "boom".into(),
            })),
            JobState::Failed,
        ),
        (
            Some(Settlement::Control(JobControlKind::Stop)),
            JobState::Cancelled,
        ),
        (
            Some(Settlement::Refused(Refusal::TimedOut)),
            JobState::TimedOut,
        ),
        (Some(Settlement::Result(json!("done"))), JobState::Completed),
        (
            Some(Settlement::Control(JobControlKind::Pause)),
            JobState::Paused,
        ),
        (
            Some(Settlement::Suspend(JobSuspend {
                kind: JobSuspendKind::AwaitUser,
                reason: None,
                message: "needs a decision".into(),
                question: Some("ship it?".into()),
            })),
            JobState::AwaitingUser,
        ),
        (None, JobState::Running),
    ];
    for (index, (settlement, expected)) in cases.into_iter().enumerate() {
        let row = pinned_job_row(&mut job, &format!("call-{}", index + 1)).await;
        if let Some(settlement) = settlement {
            settle_row(&job, &row, settlement).await;
        }
        let stored = read_step_row(&job.access, &row).await.unwrap();
        assert_eq!(stored.state, expected, "case {}", index + 1);
    }
    job.node.shutdown().await;
}

#[tokio::test]
async fn control_outcome_of_a_settled_row_read_back_is_already_terminal() {
    let mut job = new_job(Some(&live_settings(JobPolicy::default())), None).await;
    let row = pinned_job_row(&mut job, "call-1").await;
    settle_row(&job, &row, Settlement::Result(json!("done"))).await;
    let stored = read_step_row(&job.access, &row).await.unwrap();
    assert_eq!(
        control_outcome(&stored, JobControlKind::Stop, &|_| {
            KillObservation::CausedTerminal
        }),
        JobControlOutcome::AlreadyTerminal {
            state: JobState::Completed
        }
    );
    job.node.shutdown().await;
}

pub(crate) async fn stepper(output: Value) -> Stepper {
    stepper_wat(&constant_output_wat(output.to_string().as_bytes())).await
}

async fn stepper_wat(wat: &str) -> Stepper {
    let settings = step_settings(&["t-a", "t-b"]);
    let mut job = new_job(Some(&settings), None).await;
    write_inference(&job).await;
    for agent in ["a", "b"] {
        insert(
            &job.node,
            "Agent",
            json!({"agent_id": agent, "node_did": node_did(), "context_id": "context",
                       "inference_profile_id": "inference", "enabled": true}),
        )
        .await;
        insert(
            &job.node,
            "AgentTarget",
            json!({"target_id": format!("t-{agent}"), "node_did": node_did(),
                       "target_node_did": node_did(), "agent_id": agent, "name": agent}),
        )
        .await;
    }
    write_step_tools(&job, Some(&settings)).await;
    let installed = install_engine_wat(&job, wat);
    let row = job
        .row("workflow", "call-1", json!({"argv": ["run"]}))
        .await;
    let pin = JobPin {
        origin: JobOrigin::Dynamic,
        engine_digest: installed.digest.clone(),
        source_digest: "sha256:script".to_owned(),
        adopted_at: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap(),
        agent_budget: 4,
        ceiling_at_pin: crate::tools::background::job::settings::effective_ceiling(Some(&settings)),
        ..job.pin(&row, None)
    };
    job.write_pin(&row, &pin).await;
    job.write_progress(&row, JobState::Running, 1).await;
    start_running(&job, &row).await;
    Stepper {
        job,
        row,
        installed,
    }
}

impl Stepper {
    pub(crate) async fn row_json(&self, row: &str) -> Value {
        self.job
                .access
                .execute(&format!(
                    r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
                    escape_graphql_string(row),
                ))
                .await
                .unwrap()["data"]["AgentToolCall"][0]
                .clone()
    }

    async fn lifecycle(&self, row: &str) -> ToolCallLifecycle {
        load_lifecycle(&self.job.access, row).await.unwrap()
    }

    /// The row's closed canonical output.
    pub(crate) async fn output(&self, row: &str) -> Value {
        let tool = self.lifecycle(row).await;
        match crate::tools::background::status::observe_canonical_tool_output_with_access(
            &self.job.access,
            tool.doc_id().unwrap(),
            tool.request_doc_id().unwrap(),
            tool.session_id(),
            tool.node_did(),
            tool.requester_did(),
        )
        .await
        .unwrap()
        {
            crate::tools::background::status::CanonicalToolOutputObservation::Closed(output) => {
                serde_json::from_str(&output).unwrap()
            }
            crate::tools::background::status::CanonicalToolOutputObservation::Open(output) => {
                panic!("job row output is still open: {output}")
            }
        }
    }

    async fn run(&self, row: &str) -> StepOutcome {
        run_step(&self.job.runtime, row).await.unwrap()
    }

    pub(crate) async fn progress(&self, row: &str) -> JobProgress {
        read_progress(&self.job.access, row).await.unwrap().unwrap()
    }

    async fn state(
        &self,
        row: &str,
    ) -> Option<crate::tools::background::job::records::JobStepState> {
        read_step_state(&self.job.access, row).await.unwrap()
    }

    /// Bumps the row's progress to the next revision, as a concurrent
    /// writer between a step's input and its transaction would.
    async fn bump(&self, row: &str) {
        let progress = self.progress(row).await;
        let host = HostProgressFacts {
            spawned: progress.spawned,
            budget: progress.budget.clone(),
            observed_terminal_children: progress.observed_terminal_children,
            state: JobState::Running,
            pause_reason: None,
        };
        let revision = progress.revision;
        ConfigAccess::transact_local_write(&self.job.node, None, "test.driver.bump", |txn| {
            let (progress, host) = (progress.clone(), host.clone());
            Box::pin(async move { write_progress_in_txn(txn, row, revision, progress, host).await })
        })
        .await
        .unwrap();
    }

    /// One step run from `input`, built before the row moved on.
    async fn step_from(&self, row: &str, input: JobStepInput, revision: u64) -> StepOutcome {
        let mut lifecycle = self.lifecycle(row).await;
        step_on(
            &self.job.runtime,
            &mut lifecycle,
            &self.installed,
            input,
            revision,
        )
        .await
        .unwrap()
    }

    async fn input(&self, row: &str) -> (JobStepInput, u64) {
        build_step_input(&self.job.access, &self.job.runtime, row, StepKind::Later)
            .await
            .unwrap()
    }

    /// The receipt the guest's own call on `input` leaves.
    async fn receipt_of(&self, input: &JobStepInput) -> PluginExecutionReceipt {
        let caller = &input.caller;
        let jobs = JobCaller::new(
            self.job.node.clone(),
            ProcessControlScope {
                request_id: caller.request_id.clone(),
                session_id: caller.session_id.clone(),
                node_did: caller.node_did.clone(),
                requester_did: None,
            },
            StampContext::JobStep {
                node_did: caller.node_did.clone(),
                session_id: caller.session_id.clone(),
                request_doc_id: caller.request_doc_id.clone(),
                tool_call_doc_id: input.job.row_doc_id.clone(),
                job_row_doc_id: input.job.row_doc_id.clone(),
                chain_root_doc_id: input.job.chain_root_doc_id.clone(),
            },
        )
        .with_children(input.job.chain.clone(), input.ceiling.result_bytes);
        let data = DataCaller::new(self.job.node.clone(), &caller.node_did).unwrap();
        self.job
            .runtime
            .plugins
            .call_job_step_with_receipt(
                &self.installed,
                serde_json::to_value(input).unwrap(),
                Some(data),
                jobs,
            )
            .await
            .1
    }

    pub(crate) async fn close(self) {
        self.job.node.shutdown().await;
    }
}

fn stored_receipt(row: &Value) -> PluginExecutionReceipt {
    serde_json::from_value(row["plugin_execution_receipt"].clone()).unwrap()
}

struct LiveTools;

impl gents_loop::tool_effects::ToolEffectDispatcher for LiveTools {
    fn call<'a>(
        &'a self,
        _ordinal: u32,
        _name: &'a str,
        _arguments: &'a str,
    ) -> gents_loop::tool::BoxFuture<
        'a,
        Result<
            crate::tools::call::lifecycle::ToolOutcome,
            gents_loop::tool_effects::ToolEffectError,
        >,
    > {
        Box::pin(async { unreachable!("a job step reached a tool dispatcher") })
    }
}

#[tokio::test]
async fn job_step_runs_without_a_tool_dispatcher() {
    let step = stepper(json!({"result": "done"})).await;
    let live =
        || Some(Arc::new(LiveTools) as Arc<dyn gents_loop::tool_effects::ToolEffectDispatcher>);
    // A granted ordinary call under the same live dispatcher is offered
    // tools, so the step's `false` below is the job entry's, not an
    // absent dispatcher's.
    let (input, _) = step.input(&step.row).await;
    let ordinary = gents_loop::tool_effects::scope(
        live(),
        step.job.runtime.plugins.call_data_bound_with_receipt(
            &step.installed,
            serde_json::to_value(&input).unwrap(),
            None,
            true,
        ),
    )
    .await
    .1;
    assert!(ordinary.authority.unwrap().host_tools);
    assert_eq!(
        gents_loop::tool_effects::scope(live(), step.run(&step.row)).await,
        StepOutcome::Settled
    );
    let evidence = step.state(&step.row).await.unwrap().evidence.unwrap();
    let authority = evidence.authority.unwrap();
    assert!(!authority.host_tools);
    assert_eq!(evidence.verdict, PluginExecutionVerdict::Success);
    step.close().await;
}

#[tokio::test]
async fn failed_slot_shows_state_failed_while_the_row_keeps_running() {
    let step = stepper(json!({"effects": {}})).await;
    request(
        &step.job.node,
        &step.row,
        "child-1",
        &job_session_id(&step.row, "slot-a", 0),
        &job_spawn_key(&step.row, "slot-a", 0),
        "a",
        "failed",
        Some("Alpha"),
        "2026-01-01T00:00:00Z",
        Some("2026-01-01T00:01:00Z"),
    )
    .await;
    let (input, _) = step.input(&step.row).await;
    assert_eq!(input.children["slot-a"].state, JobChildState::Failed);
    assert_eq!(step.run(&step.row).await, StepOutcome::Committed);
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
    assert_eq!(step.progress(&step.row).await.observed_terminal_children, 1);
    step.close().await;
}

#[tokio::test]
async fn budget_exceeded_pause_settles_paused() {
    let step = stepper(json!({"suspend": {
        "kind": "pause", "reason": "budget_exceeded", "message": "out of budget"
    }}))
    .await;
    assert_eq!(step.run(&step.row).await, StepOutcome::Settled);
    assert_eq!(
        step.row_json(&step.row).await["lifecycle_state"],
        "completed"
    );
    assert_eq!(step.output(&step.row).await["status"], "paused");
    let progress = step.progress(&step.row).await;
    assert_eq!(progress.state, JobState::Paused);
    assert_eq!(progress.pause_reason, Some(JobPauseReason::BudgetExceeded));
    step.close().await;
}

#[tokio::test]
async fn engine_change_fails_the_job_closed() {
    let step = stepper(json!({"result": "done"})).await;
    install_engine(&step.job, &json!({"result": "changed"}));
    assert_eq!(
        step.run(&step.row).await,
        StepOutcome::Refused(Refusal::EngineChanged)
    );
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
    assert_eq!(
        step.output(&step.row).await["code"],
        "plugin_job_engine_changed"
    );
    step.close().await;
}

#[tokio::test]
async fn narrowed_live_ceiling_is_reread_inside_the_step_transaction() {
    let step = stepper(json!({"effects": {"spawn": [{
        "slot": "slot-a", "attempt": 0, "target": "t-a", "prompt": "audit", "title": "Alpha"
    }]}}))
    .await;
    let (input, revision) = step.input(&step.row).await;
    assert_eq!(input.ceiling.targets, ["t-a", "t-b"]);
    write_step_tools(&step.job, Some(&step_settings(&["t-b"]))).await;
    assert_eq!(
        step.step_from(&step.row, input, revision).await,
        StepOutcome::Refused(Refusal::PolicyDenied)
    );
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
    assert_eq!(
        step.output(&step.row).await["code"],
        "plugin_job_policy_denied"
    );
    step.close().await;
}

#[tokio::test]
async fn stale_step_is_discarded_and_restepped_from_fresh_input() {
    let step = stepper(json!({"result": "done"})).await;
    let (input, revision) = step.input(&step.row).await;
    step.bump(&step.row).await;
    assert_eq!(
        step.step_from(&step.row, input, revision).await,
        StepOutcome::Stale
    );
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
    assert_eq!(step.progress(&step.row).await.revision, 2);
    assert_eq!(step.state(&step.row).await, None);

    assert_eq!(step.run(&step.row).await, StepOutcome::Settled);
    assert_eq!(
        step.row_json(&step.row).await["lifecycle_state"],
        "completed"
    );
    assert_eq!(step.progress(&step.row).await.revision, 3);
    step.close().await;
}

#[tokio::test]
async fn terminal_with_a_live_child_fails_closed() {
    let step = stepper(json!({"result": "done"})).await;
    request(
        &step.job.node,
        &step.row,
        "child-1",
        &job_session_id(&step.row, "slot-a", 0),
        &job_spawn_key(&step.row, "slot-a", 0),
        "a",
        "processing",
        Some("Alpha"),
        "2026-01-01T00:00:00Z",
        None,
    )
    .await;
    assert_eq!(
        step.run(&step.row).await,
        StepOutcome::Refused(Refusal::PolicyDenied)
    );
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
    assert_eq!(
        step.output(&step.row).await["code"],
        "plugin_job_policy_denied"
    );
    step.close().await;
}

#[tokio::test]
async fn stall_fails_closed() {
    let step = stepper(json!({"effects": {}})).await;
    let (input, _) = step.input(&step.row).await;
    let own = step.receipt_of(&input).await;
    let earlier = evidence("persisted");
    ConfigAccess::transact_local_write(&step.job.node, None, "test.driver.earlier", |txn| {
        let (row, earlier) = (step.row.clone(), earlier.clone());
        Box::pin(async move { write_step_state_in_txn(txn, &row, &earlier, None, None).await })
    })
    .await
    .unwrap();
    assert_eq!(
        step.run(&step.row).await,
        StepOutcome::Refused(Refusal::Stalled)
    );
    let row = step.row_json(&step.row).await;
    assert_eq!(row["lifecycle_state"], "failed");
    assert_eq!(step.output(&step.row).await["code"], "plugin_job_stalled");
    let receipt = stored_receipt(&row);
    assert_eq!(receipt.input_digest, own.input_digest);
    assert_ne!(receipt.input_digest, earlier.input_digest);
    step.close().await;
}

#[tokio::test]
async fn result_with_scratch_completes_with_the_file_present() {
    let step = stepper(json!({
        "result": "done",
        "scratch": [{"name": "notes.md", "content": "hello"}],
    }))
    .await;
    let (input, _) = step.input(&step.row).await;
    assert_eq!(step.run(&step.row).await, StepOutcome::Settled);
    assert_eq!(
        step.row_json(&step.row).await["lifecycle_state"],
        "completed"
    );
    let file = Path::new(&input.caller.cwd)
        .join(".gents/workflows/runs")
        .join(&input.caller.tool_call_id)
        .join("notes.md");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello");
    std::fs::remove_file(&file).unwrap();
    assert_eq!(
        step.run(&step.row).await,
        StepOutcome::Refused(Refusal::Terminal)
    );
    assert!(
        !file.exists(),
        "a stale kick of a terminal row must not invoke the guest and recreate scratch"
    );
    step.close().await;
}

#[tokio::test]
async fn committed_step_persists_its_evidence_and_a_stale_step_does_not() {
    let step = stepper(json!({"effects": {"progress": {
        "title": "audit", "state": "running", "phases": [], "active": [], "spawned": 0,
        "budget": {"total": 4, "spent": 1, "remaining": 3}, "last_event": "again",
        "observed_terminal_children": 0
    }}}))
    .await;
    let block = resume_block("prior");
    step.job.write_step_state(&step.row, Some(&block)).await;
    // One observed-but-unaccounted terminal child, so the no-op step is not a stall.
    request(
        &step.job.node,
        &step.row,
        "child-1",
        &job_session_id(&step.row, "slot-a", 0),
        &job_spawn_key(&step.row, "slot-a", 0),
        "a",
        "completed",
        Some("Alpha"),
        "2026-01-01T00:00:00Z",
        Some("2026-01-01T00:01:00Z"),
    )
    .await;
    let (input, _) = step.input(&step.row).await;
    let expected = step.receipt_of(&input).await;
    assert_eq!(step.run(&step.row).await, StepOutcome::Committed);
    let first = step.state(&step.row).await.unwrap();
    assert_eq!(first.evidence, Some(expected));
    assert_eq!(first.resume, Some(block));
    assert!(step.row_json(&step.row).await["plugin_execution_receipt"].is_null());

    let (input, revision) = step.input(&step.row).await;
    step.bump(&step.row).await;
    assert_eq!(
        step.step_from(&step.row, input, revision).await,
        StepOutcome::Stale
    );
    assert_eq!(
        step.state(&step.row).await.unwrap().evidence,
        first.evidence
    );
    step.close().await;
}

#[tokio::test]
async fn terminal_step_settles_inside_the_step_transaction() {
    let step = stepper(json!({"result": "done"})).await;
    let before = step.progress(&step.row).await.revision;
    crate::tools::call::lifecycle::delivery::fail_next_terminal_for_test(&step.row);
    assert_eq!(step.run(&step.row).await, StepOutcome::Settled);
    // The conflict at the terminal compare re-ran the step transaction
    // whole, so the terminal and the progress write commit together; a
    // terminal settled after the step committed would leave one run.
    let runs = STEP_TRANSACTION_RUNS
        .lock()
        .unwrap()
        .iter()
        .filter(|row| **row == step.row)
        .count();
    assert_eq!(runs, 2);
    let row = step.row_json(&step.row).await;
    assert_eq!(row["lifecycle_state"], "completed");
    assert_eq!(step.progress(&step.row).await.revision, before + 1);
    let evidence = step.state(&step.row).await.unwrap().evidence.unwrap();
    assert_eq!(stored_receipt(&row), evidence);
    step.close().await;
}

use crate::tools::background::job::admit::StepChild;
use crate::tools::background::job::control::KillAction;

fn failing_row(consecutive_failures: u32) -> StepRow {
    StepRow {
        root: "root".to_owned(),
        row_doc_id: "root".to_owned(),
        caller_session: "session".to_owned(),
        caller_cwd: "/work".to_owned(),
        handle: "handle".to_owned(),
        caller_hop: 0,
        call: StepCall {
            argv: vec!["run".to_owned()],
            args: None,
            script: None,
            agent_budget: None,
        },
        root_call: None,
        resume: None,
        pin: None,
        children: vec![StepChild {
            retry_key: job_spawn_key("root", "a", 1),
            slot: "a".to_owned(),
            attempt: 1,
            target: ("node".to_owned(), "general".to_owned()),
            state: JobChildState::Live,
            messages: 0,
        }],
        spent: 1,
        observed_terminal: 0,
        consecutive_failures,
        state: JobState::Running,
    }
}

#[test]
fn step_failure_decision_is_lean_on_invocation_failure() {
    let observe = |_: &str| KillObservation::CausedLiveLocal;
    let row = failing_row(1);
    let (second, refusal) = step_failure_decision(&row, &observe);
    assert_eq!(
        second.row,
        StepRow {
            consecutive_failures: 2,
            ..row
        }
    );
    assert!(second.killed.is_empty());
    assert_eq!(refusal, None);

    let row = failing_row(2);
    let (third, refusal) = step_failure_decision(&row, &observe);
    assert_eq!(
        third.row,
        StepRow {
            consecutive_failures: 3,
            state: JobState::Failed,
            ..row.clone()
        }
    );
    let stopped = kill_decision(&row, KillCause::Stop, &observe).killed;
    assert_eq!(
        stopped,
        [(job_spawn_key("root", "a", 1), KillAction::InterruptCaused)]
    );
    assert_eq!(third.killed, stopped);
    assert_eq!(refusal, Some(Refusal::StepFailed));
    assert_eq!(refusal.unwrap().code(), Some("plugin_job_step_failed"));
}

/// Waits until `driver` has no row in flight.
async fn idle(driver: &JobDriver) {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while !driver.in_flight.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the driver goes idle");
}

/// A child request of `step`'s row that is still processing.
async fn live_child(step: &Stepper, node_did: &str) -> String {
    insert(
        &step.job.node,
        "AgentRequest",
        json!({
            "request_id": "child-1",
            "purpose": "normal",
            "node_did": node_did,
            "requester_did": null,
            "session_id": job_session_id(&step.row, "slot-a", 0),
            "agent_id": "a",
            "lifecycle_state": "processing",
            "retry_key": job_spawn_key(&step.row, "slot-a", 0),
            "created_at": "2026-01-01T00:00:00Z",
            "caused_by_parent_tool_call_doc_id": step.row,
            "caused_by_parent_tool_call_id": "call",
            "caused_by_parent_request_id": "parent",
            "caused_by_parent_request_doc_id": "parent-doc",
        }),
    )
    .await
}

#[tokio::test]
async fn recovery_hold_and_deferred_step_keep_runtime_retirement_busy() {
    use crate::runtime_service::{RetirementError, RuntimeServiceAdmission};
    use gents_protocol::serve_lifecycle::replacement::RetirementAuthority;

    let step = stepper(json!({"effects": {}})).await;
    live_child(&step, "did-owner").await;
    let admission = RuntimeServiceAdmission::default();
    let driver = JobDriver::with_service_admission(step.job.runtime.clone(), admission.clone());
    let authority = || RetirementAuthority {
        operator_authenticated: true,
        observed_build_matches: true,
    };
    let held = driver.permits.acquire_many(4).await.unwrap();
    driver
        .hold_steps(&step.row, async {
            assert_eq!(admission.retire(authority()), Err(RetirementError::Busy));
            driver.kick(&step.row);
        })
        .await
        .unwrap();
    assert!(driver.is_stepping(&step.row));
    assert_eq!(admission.retire(authority()), Err(RetirementError::Busy));
    drop(held);
    idle(&driver).await;
    assert_eq!(step.progress(&step.row).await.revision, 2);
    admission.retire(authority()).unwrap();
    assert!(driver
        .hold_steps(&step.row, async {
            panic!("retiring runtime admitted recovery")
        })
        .await
        .is_none());
    step.close().await;
}

#[tokio::test]
async fn two_wakes_during_a_step_produce_exactly_one_rerun() {
    let step = stepper(json!({"effects": {}})).await;
    live_child(&step, "did-owner").await;
    let driver = JobDriver::new(step.job.runtime.clone());
    // The first kick puts the row in flight; the other two arrive while
    // its step is in flight.
    for _ in 0..3 {
        driver.kick(&step.row);
    }
    idle(&driver).await;
    assert_eq!(step.progress(&step.row).await.revision, 3, "two steps");
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
    step.close().await;
}

#[tokio::test]
async fn concurrent_steps_are_bounded_by_the_semaphore() {
    let step = stepper(json!({"effects": {}})).await;
    live_child(&step, "did-owner").await;
    let driver = JobDriver::new(step.job.runtime.clone());
    assert_eq!(driver.permits.available_permits(), 4);
    let held = driver.permits.acquire_many(4).await.unwrap();
    driver.kick(&step.row);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        step.progress(&step.row).await.revision,
        1,
        "no permit, no step"
    );
    assert!(driver.in_flight.lock().unwrap().contains_key(&step.row));
    drop(held);
    idle(&driver).await;
    assert_eq!(step.progress(&step.row).await.revision, 2);
    step.close().await;
}

#[tokio::test]
async fn three_consecutive_step_failures_settle_step_failed_with_the_next_call() {
    let step = stepper(json!("not a step output")).await;
    let earlier = evidence("earlier");
    ConfigAccess::transact_local_write(&step.job.node, None, "test.driver.earlier", |txn| {
        let (row, earlier) = (step.row.clone(), earlier.clone());
        Box::pin(async move { write_step_state_in_txn(txn, &row, &earlier, None, None).await })
    })
    .await
    .unwrap();
    let driver = JobDriver::new(step.job.runtime.clone());
    driver.kick(&step.row);
    idle(&driver).await;
    let row = step.row_json(&step.row).await;
    assert_eq!(row["lifecycle_state"], "failed");
    let output = step.output(&step.row).await;
    assert_eq!(output["code"], "plugin_job_step_failed");
    let next = output["next"].as_str().unwrap();
    assert!(next.contains("gents job show"), "{next}");
    let message = output["message"].as_str().unwrap();
    assert!(
        message.starts_with("three consecutive job steps failed; last: ")
            && message.contains("malformed step output"),
        "the failure must name the last step error: {message}"
    );
    let progress = step.progress(&step.row).await;
    assert_eq!(progress.state, JobState::Failed, "{progress:?}");
    assert!(
        message.starts_with(progress.last_event.as_str()),
        "the last event is the failure message (cut to the record cap): {progress:?}"
    );
    assert_eq!(
        stored_receipt(&row),
        PluginExecutionReceipt {
            output_digest: None,
            verdict: PluginExecutionVerdict::ExecutionError,
            ..earlier
        }
    );
    assert_eq!(
        progress.revision, 2,
        "the failed steps committed nothing; the terminal stamp is the only write"
    );
    step.close().await;
}

#[tokio::test]
async fn two_failures_then_a_success_do_not_settle() {
    let step = stepper(json!({"effects": {}})).await;
    live_child(&step, "did-owner").await;
    let driver = JobDriver::new(step.job.runtime.clone());
    let trap = StepFailure::Trap("boom".to_owned());
    for _ in 0..2 {
        assert_eq!(
            driver.step_failed(&step.row, &trap).await.unwrap(),
            FailureVerdict::Retry
        );
    }
    driver.kick(&step.row);
    idle(&driver).await;
    assert_eq!(step.progress(&step.row).await.revision, 2, "the step ran");
    assert_eq!(
        driver.step_failed(&step.row, &trap).await.unwrap(),
        FailureVerdict::Retry,
        "the success reset the count"
    );
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
    step.close().await;
}

#[tokio::test]
async fn step_failures_without_a_remembered_row_read_it_and_settle_on_the_third() {
    let step = stepper(json!({"effects": {}})).await;
    let child = live_child(&step, node_did()).await;
    let driver = JobDriver::new(step.job.runtime.clone());
    let before = read_step_row(&step.job.access, &step.row).await.unwrap();
    let observed = observe_live_children(&step.job.access, &before, node_did())
        .await
        .unwrap();
    let expected = kill_decision(&before, KillCause::Stop, &|key: &str| observed[key]).killed;
    assert_eq!(
        expected,
        [(
            job_spawn_key(&step.row, "slot-a", 0),
            KillAction::InterruptCaused
        )]
    );
    let trap = StepFailure::Trap("before the input".to_owned());
    for verdict in [
        FailureVerdict::Retry,
        FailureVerdict::Retry,
        FailureVerdict::Settle,
    ] {
        assert_eq!(driver.step_failed(&step.row, &trap).await.unwrap(), verdict);
    }
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
    assert_eq!(
        step.output(&step.row).await["code"],
        "plugin_job_step_failed"
    );
    assert!(
        crate::request::interrupt::fetch_interrupt_requested_at_by_doc_id(&step.job.node, &child)
            .await
            .unwrap()
            .is_some(),
        "the live child is interrupted"
    );
    step.close().await;
}

#[tokio::test]
async fn a_guest_trap_is_a_step_failure() {
    let step = stepper_wat(
        r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#,
    )
    .await;
    let error = run_step(&step.job.runtime, &step.row).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<StepFailure>(),
            Some(StepFailure::Trap(_))
        ),
        "{error:#}"
    );
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "running");
    step.close().await;
}

#[tokio::test]
async fn a_step_call_error_that_is_not_a_trap_counts_towards_step_failed() {
    let step = stepper_wat(r#"(module (memory (export "memory") 1))"#).await;
    let error = run_step(&step.job.runtime, &step.row).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<StepFailure>(),
            Some(StepFailure::NotRun(_))
        ),
        "the call error is not a trap: {error:#}"
    );
    let driver = JobDriver::new(step.job.runtime.clone());
    let sweep = crate::tools::background::job::recovery::redrive_running_jobs;
    // The sweep re-steps the row, which has not stepped since adoption.
    sweep(&driver).await.unwrap();
    idle(&driver).await;
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
    assert_eq!(
        step.output(&step.row).await["code"],
        "plugin_job_step_failed"
    );
    assert_eq!(
        sweep(&driver).await.unwrap(),
        0,
        "a failed row is not re-stepped"
    );
    step.close().await;
}

/// Lean `guest_state_opaque`: the state a step carries is stored beside
/// its records and echoed to the next step's input, untouched by the host.
#[tokio::test]
async fn a_steps_guest_state_is_stored_and_echoed_to_the_next_step() {
    let step = stepper(json!({"effects": {
        "spawn": [{"slot": "1:a", "attempt": 0, "target": "t-a", "prompt": "p", "title": "a"}],
        "state": {"phase": "scan", "paths": ["src/lib.rs"]},
    }}))
    .await;
    assert_eq!(step.run(&step.row).await, StepOutcome::Committed);
    let stored = step.state(&step.row).await.unwrap();
    assert_eq!(
        stored.guest,
        Some(json!({"phase": "scan", "paths": ["src/lib.rs"]}))
    );
    let (input, _) = step.input(&step.row).await;
    assert_eq!(
        input.state,
        Some(json!({"phase": "scan", "paths": ["src/lib.rs"]}))
    );
    step.close().await;
}

/// A step that carries no state keeps the stored one.
#[tokio::test]
async fn a_step_without_state_keeps_the_stored_one() {
    let step = stepper(json!({"effects": {
        "spawn": [{"slot": "1:a", "attempt": 0, "target": "t-a", "prompt": "p", "title": "a"}],
    }}))
    .await;
    step.job
        .write_guest_state(&step.row, &json!({"phase": "recon"}))
        .await;
    assert_eq!(step.run(&step.row).await, StepOutcome::Committed);
    assert_eq!(
        step.state(&step.row).await.unwrap().guest,
        Some(json!({"phase": "recon"}))
    );
    step.close().await;
}

/// A state over `JOB_GUEST_STATE_MAX_BYTES` is a malformed output: nothing
/// is stored, and the driver counts it until the row fails closed.
#[tokio::test]
async fn a_guest_state_over_the_cap_fails_the_step_and_stores_nothing() {
    let big = "x".repeat(gents_protocol::job::JOB_GUEST_STATE_MAX_BYTES + 1);
    let step = stepper(json!({"effects": {
        "spawn": [{"slot": "1:a", "attempt": 0, "target": "t-a", "prompt": "p", "title": "a"}],
        "state": {"blob": big},
    }}))
    .await;
    let outcome = run_step(&step.job.runtime, &step.row).await;
    assert!(
        matches!(
            outcome
                .as_ref()
                .map_err(|error| error.downcast_ref::<StepFailure>()),
            Err(Some(StepFailure::MalformedOutput(_)))
        ),
        "an over-cap state is a malformed output: {outcome:?}"
    );
    assert_eq!(
        step.state(&step.row).await.and_then(|state| state.guest),
        None
    );
    let driver = JobDriver::new(step.job.runtime.clone());
    driver.kick(&step.row);
    idle(&driver).await;
    assert_eq!(step.row_json(&step.row).await["lifecycle_state"], "failed");
    assert_eq!(
        step.output(&step.row).await["code"],
        "plugin_job_step_failed"
    );
    step.close().await;
}
