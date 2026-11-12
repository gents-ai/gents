use std::path::Path;
use std::time::Duration;

use gents::DocumentRuntimeOptions;
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::job::{JobChildState, JobState};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_toolcalls::background::job::view::{load_job_view, JobKey};
use serde_json::{json, Value};

use crate::support::agent_requests::canonical_now;
use crate::support::live_inference::{
    bind_target, boot_live_agent_with_options, live_target, wait_for_request_terminal,
    wait_for_rows, InferenceTarget, LiveUser,
};
use crate::support::{test_db, ManagedAgentFixture};

pub(super) async fn submit_with_cwd(
    access: &ConfigAccess,
    fixture: &ManagedAgentFixture,
    node_did: &str,
    session: &str,
    request_id: &str,
    content: String,
    cwd: &Path,
) -> String {
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        node_did,
        fixture.user.did(),
        &fixture.binding.agent_did,
        session,
        content,
        "interactive",
        canonical_now(),
        AgentRequestAdmissionRecord::client(fixture.user.did()),
    );
    request.input.cwd = Some(cwd.to_str().unwrap().to_owned());
    request.input = gents_session::request::lifecycle::prepare_user_message_input(
        access,
        node_did,
        session,
        request.input,
        Default::default(),
    )
    .await
    .unwrap();
    request.session_doc_id = Some(
        gents_session::request::lifecycle::reserve_goal_session(access, &request)
            .await
            .unwrap(),
    );
    request.agent_doc_id = Some(
        access
            .transact_readonly("test.live_job.agent", |txn| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        gents_documents::read_desired_state_record_in_txn(
                            txn,
                            Collection::Agent,
                            node_did,
                            &fixture.binding.agent_did,
                        )
                        .await?
                        .unwrap()
                        .0,
                    )
                })
            })
            .await
            .unwrap(),
    );
    gents_session::request::admission::sign_agent_request_create(&fixture.user, &mut request)
        .await
        .unwrap();
    gents_session::request::lifecycle::submit_client_request(access, &request)
        .await
        .unwrap();
    let rows = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(request_id),
        ))
        .await
        .unwrap();
    rows["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn select_profiles(live: &LiveUser, target: &InferenceTarget, agents: &[&str]) {
    let mut documents = Vec::new();
    for agent in agents {
        let value = live
            .access
            .transact_readonly("test.live_job.profile", |txn| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        gents_documents::read_desired_state_record_in_txn(
                            txn,
                            Collection::Agent,
                            &live.node_did,
                            agent,
                        )
                        .await?
                        .unwrap()
                        .1,
                    )
                })
            })
            .await
            .unwrap();
        let mut profile = target.profile(&live.node_did);
        profile.profile_id = value["inference_profile_id"].as_str().unwrap().to_owned();
        documents.push((
            Collection::InferenceProfile,
            serde_json::to_value(profile).unwrap(),
        ));
    }
    if let Some(sampling) = target.sampling(&live.node_did) {
        documents.push((
            Collection::InferenceSampling,
            serde_json::to_value(sampling).unwrap(),
        ));
    }
    live.apply("test.live_job.profiles", documents).await;
}

async fn tool_result(
    access: &ConfigAccess,
    node: &str,
    user: &str,
    session: &str,
    doc: &str,
) -> Value {
    let message = gents_session::tool_call::query::result::load_tool_call_result(
        access,
        doc,
        node,
        session,
        Some(user),
    )
    .await
    .unwrap();
    serde_json::from_str(
        &gents_session::tool_call::query::result::render_tool_result(&message).unwrap(),
    )
    .unwrap()
}

pub(super) struct LiveJobHarness {
    pub db: crate::support::TestDb,
    pub actors: super::plugin_job_support::JobActors,
    pub runtime: crate::support::interrupt::BootedAgent,
    pub live: LiveUser,
    pub session_id: String,
    pub request_id: String,
    pub parent_doc_id: String,
    pub row: Value,
    pub plugin_home: tempfile::TempDir,
    pub cwd: std::path::PathBuf,
}

pub(super) async fn completed_live_job(name: &str, value: Value) -> LiveJobHarness {
    use super::plugin_job_support::{
        effects, guest_pin, install_job_fixture, job_settings, result, spawn, PlanEntry,
    };
    let target = live_target().await;
    let db = test_db(name).await;
    let plugin_home = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .unwrap()
        .unwrap();
    let actors = install_job_fixture(&db, plugin_home.path(), job_settings()).await;
    let live = LiveUser::install(&db, &target).await;
    select_profiles(
        &live,
        &target,
        &[&actors.fixture.binding.agent_did, &actors.child_did],
    )
    .await;
    let cwd = tokio::fs::canonicalize(plugin_home.path()).await.unwrap();
    let tools = live
        .access
        .transact_readonly("test.live_job.tools", |txn| {
            let node_did = &live.node_did;
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents_documents::read_desired_state_record_in_txn(
                        txn,
                        Collection::Tools,
                        node_did,
                        "plugin-job-parent:tools",
                    )
                    .await?
                    .unwrap()
                    .1,
                )
            })
        })
        .await
        .unwrap();
    let mut tools: gents_documents::Tools = serde_json::from_value(tools).unwrap();
    tools.host = Some(gents_documents::HostTools {
        root: Some(cwd.to_str().unwrap().into()),
        ..Default::default()
    });
    crate::support::fixtures::configure_agent_tools_as(
        &live.access, &live.node_did, &actors.fixture.binding.agent_did,
        Some("Follow the user's requested job call. Acknowledge its receipt and completion without starting another job.".into()),
        tools, Vec::new(),
    ).await;
    gents_tools::surface::set_workspace_root_with_readers(
        &live.access,
        &live.node_did,
        cwd.to_str().unwrap(),
        None,
        true,
        &[
            actors.fixture.binding.agent_did.clone(),
            actors.child_did.clone(),
        ],
    )
    .await
    .unwrap();
    let plan = tokio::task::spawn_blocking(move || {
        vec![
            PlanEntry {
                when: json!({"step":0}),
                output: effects(
                    Some(guest_pin("live-job-handle")),
                    vec![spawn(
                        "child",
                        "Acknowledge this request briefly. Do not use tools.",
                    )],
                ),
                control: None,
            },
            PlanEntry {
                when: json!({"children":{"child":"completed"}}),
                output: result(value),
                control: None,
            },
            PlanEntry {
                when: json!({"children":{"child":"live"}}),
                output: effects(None, Vec::new()),
                control: None,
            },
        ]
        .iter()
        .map(PlanEntry::to_json)
        .collect::<Vec<_>>()
    })
    .await
    .unwrap();
    let (runtime, _) = boot_live_agent_with_options(
        &db,
        db.node_identity.clone(),
        DocumentRuntimeOptions {
            plugin_home: Some(plugin_home.path().to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let session_id = format!("{name}-session");
    let request_id = format!("{name}-parent");
    let parent_doc_id = submit_with_cwd(&live.access, &actors.fixture, &live.node_did, &session_id, &request_id,
        format!("Call job_fixture once with exactly these arguments: {}. It will run in the background. Acknowledge after the receipt and acknowledge its eventual completion without starting another job.", json!({"args":{"plan":plan}})), &cwd,
    ).await;
    let query = format!(
        r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "job_fixture"}}}}) {{_docID tool_call_id lifecycle_state plugin_execution_receipt}} }}"#,
        escape_graphql_string(&request_id),
    );
    let rows = wait_for_rows(
        &live.access,
        "AgentToolCall",
        &query,
        Duration::from_secs(120),
        |data| {
            data["AgentToolCall"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["lifecycle_state"] == "completed"))
        },
    )
    .await;
    let row = rows["AgentToolCall"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["lifecycle_state"] == "completed")
        .unwrap()
        .clone();
    assert_eq!(
        wait_for_request_terminal(&live.access, &request_id, Duration::from_secs(120)).await,
        "completed"
    );
    LiveJobHarness {
        db,
        actors,
        runtime,
        live,
        session_id,
        request_id,
        parent_doc_id,
        row,
        plugin_home,
        cwd,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_plugin_job_adopts_a_child_and_completes() {
    let h = completed_live_job("live-plugin-job", json!({"status":"done"})).await;
    assert!(!h.row["plugin_execution_receipt"].is_null());
    let view = load_job_view(
        &h.live.access,
        None,
        JobKey::Address {
            session_id: h.session_id.clone(),
            tool_call_id: h.row["tool_call_id"].as_str().unwrap().into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(view.state, JobState::Completed);
    let child = view
        .children
        .iter()
        .find(|child| child.slot == "child")
        .expect("the job adopted its requested child");
    assert_eq!(child.state, JobChildState::Completed);
    assert_eq!(
        wait_for_request_terminal(&h.live.access, &child.request_id, Duration::from_secs(120))
            .await,
        "completed"
    );
    h.runtime.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_waited_process_defers_goal_until_its_completion_wake() {
    let target = live_target().await;
    let db = test_db("live-goal-process-wake").await;
    let actors = bind_target(&db, &target).await;
    let root = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .unwrap()
        .unwrap();
    let root_path = tokio::fs::canonicalize(root.path()).await.unwrap();
    let fifo = root_path.join("release");
    let make_fifo = fifo.clone();
    let mut release = tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::OpenOptionsExt;
        let status = std::process::Command::new("mkfifo")
            .arg(&make_fifo)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&make_fifo)
            .unwrap()
    })
    .await
    .unwrap();
    crate::support::fixtures::configure_agent_tools_as(
        &actors.fixture.access, actors.node_did(), actors.agent_did(),
        Some("Call update_goal with status complete only when the current user message explicitly says 'On this durable Goal continuation'. For a background completion notification acknowledge briefly without tools. Follow all other current user instructions.".into()),
        gents_documents::Tools {
            tools_id: "live-goal-process-tools".into(), node_did: actors.node_did().into(),
            host: Some(gents_documents::HostTools {
                root: Some(root_path.to_str().unwrap().into()),
                bash: Some(gents_documents::BashTools {
                    background_enabled: true, mode: gents_documents::BashMode::ReadOnly,
                    read_only_commands: Some(vec!["sh".into()]), ..Default::default()
                }), ..Default::default()
            }),
            built_ins: Some(gents_documents::BuiltInTools {enable_goal_tools: Some(true), ..Default::default()}),
            ..Default::default()
        }, Vec::new(),
    ).await;
    let (runtime, _) = boot_live_agent_with_options(
        &db,
        db.node_identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: gents_tools::surface::ToolCeiling::readonly_at(&root_path),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    const SESSION: &str = "live-goal-process-session";
    const REQUEST: &str = "live-goal-process-parent";
    let arguments = json!({"tool_name":"bash", "args":{"command":"sh", "args":["-c","IFS= read -r line < \"$1\"; printf 'background process completed'","live-goal-process",fifo]}});
    submit_with_cwd(&actors.fixture.access, &actors.fixture, actors.node_did(), SESSION, REQUEST,
        format!("Call spawn_process once with exactly {arguments}. Use its returned tool_call_id to call wait_process with timeout_secs 1. When that wait returns still running, acknowledge briefly and end this turn. Do not wait again and do not call update_goal."), &root_path,
    ).await;
    assert_eq!(
        wait_for_request_terminal(&actors.fixture.access, REQUEST, Duration::from_secs(120)).await,
        "completed"
    );
    let calls = actors.fixture.access.execute(&format!(r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{REQUEST}"}}}}) {{_docID tool_call_id tool_name lifecycle_state await_mode}} }}"#)).await.unwrap();
    let calls = calls["data"]["AgentToolCall"].as_array().unwrap();
    let process = calls
        .iter()
        .find(|row| row["tool_name"] == "bash")
        .expect("the requested process was spawned");
    assert_eq!(process["lifecycle_state"], "running");
    let process_doc = process["_docID"].as_str().unwrap();
    let waited = calls
        .iter()
        .find(|row| row["tool_name"] == "wait_process")
        .expect("the parent waited on its process");
    let wait_result = tool_result(
        &actors.fixture.access,
        actors.node_did(),
        actors.fixture.user.did(),
        SESSION,
        waited["_docID"].as_str().unwrap(),
    )
    .await;
    assert_eq!(wait_result["status"], "running");
    let goal = gents_automation::goal::seed_goal_from_access(&actors.agent_access, actors.node_did(), SESSION,
        Some("On this durable Goal continuation, call update_goal with status complete and a brief reason, then acknowledge completion."), Some(GoalStatus::Active), None, None,
    ).await.unwrap();
    let goal_query = format!(
        r#"{{ Goal(filter: {{goal_id: {{_eq: "{}"}}}}) {{last_continued_from_request_id status}} }}"#,
        escape_graphql_string(&goal.goal_id)
    );
    wait_for_rows(
        &actors.fixture.access,
        "Goal",
        &goal_query,
        Duration::from_secs(30),
        |data| {
            data["Goal"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["last_continued_from_request_id"] == REQUEST)
            })
        },
    )
    .await;
    let children_query = format!(
        r#"{{ AgentRequest(filter: {{caused_by_trigger_kind: {{_eq: "goal"}}, caused_by_trigger_id: {{_eq: "{}"}}}}) {{_docID request_id caused_by_parent_request_doc_id lifecycle_state}} }}"#,
        escape_graphql_string(&goal.goal_id)
    );
    let before = actors
        .fixture
        .access
        .execute(&children_query)
        .await
        .unwrap();
    assert!(before["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    let _release = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        release.write_all(b"release\n").unwrap();
        release
    })
    .await
    .unwrap();
    let notification_query = format!(
        r#"{{ AgentMessage(filter: {{session_id: {{_eq: "{SESSION}"}}}}) {{publication request_doc_id}} }}"#
    );
    let delivered = wait_for_rows(
        &actors.fixture.access,
        "AgentMessage",
        &notification_query,
        Duration::from_secs(120),
        |data| {
            data["AgentMessage"].as_array().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["publication"]["kind"] == "tool_delivery"
                        && row["publication"]["tool_call_doc_id"] == process_doc
                })
            })
        },
    )
    .await;
    let wake = delivered["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["publication"]["tool_call_doc_id"] == process_doc)
        .unwrap()["request_doc_id"]
        .as_str()
        .unwrap();
    let children = wait_for_rows(
        &actors.fixture.access,
        "AgentRequest",
        &children_query,
        Duration::from_secs(120),
        |data| {
            data["AgentRequest"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["caused_by_parent_request_doc_id"] == wake)
            })
        },
    )
    .await;
    let child = children["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["caused_by_parent_request_doc_id"] == wake)
        .unwrap();
    assert_eq!(
        wait_for_request_terminal(
            &actors.fixture.access,
            child["request_id"].as_str().unwrap(),
            Duration::from_secs(120)
        )
        .await,
        "completed"
    );
    let goal = actors.fixture.access.execute(&goal_query).await.unwrap();
    assert_eq!(goal["data"]["Goal"][0]["status"], "complete");
    runtime.shutdown().await;
}
