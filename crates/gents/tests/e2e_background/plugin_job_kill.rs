//! Stopping a plugin job interrupts its live children and ends its row now.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gents::background_tools::ProcessControlScope;
use gents::document_config::{AgentTools, IntegrationTools, PluginToolRef, Tools};
use gents::llm::message::{AssistantContent, Message, UserContent};
use gents::plugin_job::control::control;
use gents::plugin_job::job::{
    JobChildState, JobControlKind, JobControlOutcome, JobControlRequest, JobSpawn, JobState,
    PluginJobSettings,
};
use gents::tool_control::{
    cancel_background_tool_call, cancel_session_background_process, CancelBackgroundToolCallOutcome,
};
use gents::{
    BackgroundExecutionRegistry, ConfigAccess, DocumentRuntimeOptions, Gents, NodeIdentity,
};
use serde_json::{json, Value};

use crate::plugin_job_support::{
    effects, guest_pin, install_job_fixture, job_call, job_row, job_settings, result, spawn,
    wait_job, JobHarness, PlanEntry, CHILD, MODEL, PARENT, PLUGIN,
};
use crate::support::accepted_turn::{
    boot_accepted_turn_with_backend_capacity, boot_prepared_accepted_turn,
    enqueue_local_accepted_request, prepare_accepted_turn, AcceptedTurnSpec,
};
use crate::support::enrollment::{authorize_enrollment_peer, wait_for_peer_identity};
use crate::support::fixtures::{
    agent_target, bind_agent_backend, configure_agent_tools, configure_child_agent,
};
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::p2p_waits::wait_for_connected_peer;
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::{test_db, test_p2p_db, TestDb};

const STOP_PROMPT: &str = "plugin-job-stop-prompt";
const TOOL_CALL: &str = "job-k";

fn entry(when: Value, output: gents::plugin_job::job::JobStepOutput) -> PlanEntry {
    PlanEntry {
        when,
        output,
        control: None,
    }
}

/// Step 0 pins the job and spawns slot `a` and slot `b`; both are held, and
/// the holding entries keep the job alive while either child is live.
fn two_child_plan(prompt_a: &str, prompt_b: &str) -> Vec<PlanEntry> {
    two_child_plan_on(prompt_a, prompt_b, CHILD)
}

/// As [`two_child_plan`], with slot `b` spawned on `target_b`.
fn two_child_plan_on(prompt_a: &str, prompt_b: &str, target_b: &str) -> Vec<PlanEntry> {
    vec![
        entry(
            json!({"step": 0}),
            effects(
                Some(guest_pin("job-k-handle")),
                vec![
                    spawn("a", prompt_a),
                    JobSpawn {
                        target: target_b.into(),
                        ..spawn("b", prompt_b)
                    },
                ],
            ),
        ),
        entry(
            json!({"children": {"a": "live"}}),
            effects(None, Vec::new()),
        ),
        entry(
            json!({"children": {"b": "live"}}),
            effects(None, Vec::new()),
        ),
    ]
}

fn held_child(prompt: &str) -> StreamPlan {
    StreamPlan::current_authored_user(
        prompt,
        vec![StreamResponse::Stream(StreamScript::paused_before(
            prompt,
            vec![StreamChunk::text("child done")],
        ))],
    )
}

/// `boot_job_harness` with room for the stopping turn: two held children use
/// the default worker slots, so a later accepted turn would wait behind them.
async fn boot_wide_job(
    db: TestDb,
    plugin_home: PathBuf,
    session_id: &str,
    accepted_chunks: Vec<StreamChunk>,
    child_plans: Vec<StreamPlan>,
) -> JobHarness {
    install_job_fixture(&db, &plugin_home, job_settings()).await;
    let cwd = db.data_path().join("job_cwd");
    std::fs::create_dir_all(&cwd).expect("create the job working folder");
    let cwd = cwd
        .canonicalize()
        .expect("resolve the job working folder")
        .to_string_lossy()
        .into_owned();
    let runtime = boot_accepted_turn_with_backend_capacity(
        &db,
        AcceptedTurnSpec {
            backend_id: "plugin-job-backend",
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
        DocumentRuntimeOptions {
            plugin_home: Some(plugin_home.clone()),
            ..Default::default()
        },
        4,
    )
    .await;
    JobHarness {
        db,
        plugin_home,
        runtime,
        session_id: session_id.into(),
    }
}

struct LiveJob {
    h: JobHarness,
    row_doc_id: String,
    session_id: String,
    _plugin_home: tempfile::TempDir,
}

/// Boots a harness whose job holds two live children.
async fn live_job(name: &str) -> LiveJob {
    let session_id = format!("{name}-session");
    let prompt_a = format!("{name} child a");
    let prompt_b = format!("{name} child b");
    let plugin_home = tempfile::tempdir().expect("plugin home");
    let h = boot_wide_job(
        test_db(name).await,
        plugin_home.path().to_path_buf(),
        &session_id,
        vec![job_call(TOOL_CALL, &two_child_plan(&prompt_a, &prompt_b))],
        vec![held_child(&prompt_a), held_child(&prompt_b)],
    )
    .await;
    wait_job(
        &h,
        TOOL_CALL,
        |view| {
            view.children.len() == 2
                && view
                    .children
                    .iter()
                    .all(|child| child.state == JobChildState::Live)
        },
        Duration::from_secs(30),
    )
    .await;
    let row_doc_id = job_row(&h, TOOL_CALL).await;
    LiveJob {
        h,
        row_doc_id,
        session_id,
        _plugin_home: plugin_home,
    }
}

async fn execute(h: &JobHarness, query: String) -> Value {
    ConfigAccess::local(h.db.node.clone())
        .execute(&query)
        .await
        .expect("read the store")
}

/// The job's child requests as (`node_did`, `interrupt_requested_at`).
async fn children(h: &JobHarness, row_doc_id: &str) -> Vec<(String, Option<String>)> {
    let response = execute(
        h,
        format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{}" }} }}) {{ node_did interrupt_requested_at }} }}"#,
            gents::graphql::escape_graphql_string(row_doc_id),
        ),
    )
    .await;
    response["data"]["AgentRequest"]
        .as_array()
        .expect("children array")
        .iter()
        .map(|row| {
            (
                row["node_did"].as_str().expect("child node").to_string(),
                row["interrupt_requested_at"].as_str().map(str::to_string),
            )
        })
        .collect()
}

async fn row_state(h: &JobHarness, row_doc_id: &str) -> String {
    let response = execute(
        h,
        format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state }} }}"#,
            gents::graphql::escape_graphql_string(row_doc_id),
        ),
    )
    .await;
    response["data"]["AgentToolCall"][0]["lifecycle_state"]
        .as_str()
        .expect("row state")
        .to_string()
}

/// How many messages of the session carry the completion notification of
/// `tool_call_id`.
async fn completion_notifications(h: &JobHarness, tool_call_id: &str) -> usize {
    let marker = format!(r#"<tool-completion tool_call_id="{tool_call_id}""#);
    let response = execute(
        h,
        format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID node_did requester_did }} }}"#,
            gents::graphql::escape_graphql_string(&h.session_id),
        ),
    )
    .await;
    let mut count = 0;
    for header in response["data"]["AgentMessage"]
        .as_array()
        .expect("message headers")
    {
        let (_, message) = gents::session::load_canonical_message_from_node(
            h.db.node.as_ref(),
            header["_docID"].as_str().expect("message doc id"),
            header["node_did"].as_str().expect("message node"),
            header["requester_did"].as_str(),
        )
        .await
        .expect("reconstruct canonical message");
        let texts: Vec<String> = match message {
            Message::User { content } => content
                .into_iter()
                .filter_map(|item| match item {
                    UserContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect(),
            Message::Assistant { content, .. } => content
                .into_iter()
                .filter_map(|item| match item {
                    AssistantContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        count += texts.iter().filter(|text| text.contains(&marker)).count();
    }
    count
}

/// The row ended cancelled, every child was interrupted, and the completion
/// reached the parent session once.
async fn assert_stopped(job: &LiveJob, route: &str) {
    let h = &job.h;
    let view = wait_job(
        h,
        TOOL_CALL,
        |view| view.state == JobState::Cancelled,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(view.children.len(), 2, "{route}: {view:?}");
    assert_eq!(row_state(h, &job.row_doc_id).await, "cancelled", "{route}");
    let children = children(h, &job.row_doc_id).await;
    assert_eq!(children.len(), 2, "{route}: {children:?}");
    assert!(
        children
            .iter()
            .all(|(_, interrupted)| interrupted.is_some()),
        "{route}: every live child is interrupted: {children:?}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while completion_notifications(h, TOOL_CALL).await == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{route}: no completion notification was delivered"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Longer than one reconciler tick, so a second delivery would show.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(
        completion_notifications(h, TOOL_CALL).await,
        1,
        "{route}: the completion is delivered exactly once"
    );
}

fn parent_scope(h: &JobHarness) -> ProcessControlScope {
    let did = h.db.node_identity.did().to_string();
    ProcessControlScope {
        request_id: format!("{}-parent", h.session_id),
        session_id: h.session_id.clone(),
        node_did: did.clone(),
        requester_did: Some(did),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_cancel_client_cancel_and_non_job_stop_each_end_the_row_cancelled() {
    // Operator cancel.
    let job = live_job("plugin-job-kill-operator").await;
    let outcome = cancel_background_tool_call(
        job.h.db.node.clone(),
        &BackgroundExecutionRegistry::default(),
        &job.session_id,
        TOOL_CALL,
    )
    .await
    .expect("operator cancel");
    assert!(
        matches!(outcome, CancelBackgroundToolCallOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert_stopped(&job, "operator").await;
    job.h.runtime.shutdown().await;

    // Client cancel.
    let job = live_job("plugin-job-kill-client").await;
    let did = job.h.db.node_identity.did().to_string();
    let outcome = cancel_session_background_process(
        job.h.db.node.clone(),
        &BackgroundExecutionRegistry::default(),
        &did,
        Some(&did),
        &job.session_id,
        TOOL_CALL,
    )
    .await
    .expect("client cancel");
    assert!(
        matches!(outcome, CancelBackgroundToolCallOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert_stopped(&job, "client").await;
    job.h.runtime.shutdown().await;

    // A later accepted turn stops the row with a non-job call of the plugin.
    let job = live_job("plugin-job-kill-non-job").await;
    let backend = &job.h.runtime.backend;
    backend.enable_dynamic_followups(STOP_PROMPT);
    backend.enqueue_response(
        STOP_PROMPT,
        StreamResponse::streams(
            STOP_PROMPT,
            vec![job_call(
                "stop-1",
                &[PlanEntry {
                    control: Some(JobControlRequest {
                        row_doc_id: job.row_doc_id.clone(),
                        kind: JobControlKind::Stop,
                    }),
                    ..entry(json!({"step": 0}), result(json!("stopped")))
                }],
            )],
        ),
    );
    backend.enqueue_response(
        STOP_PROMPT,
        StreamResponse::completes(STOP_PROMPT, ["stop sent"]),
    );
    enqueue_local_accepted_request(
        &job.h.db,
        PARENT,
        "stop-request",
        &job.session_id,
        STOP_PROMPT,
    )
    .await;
    assert_stopped(&job, "non-job stop").await;
    job.h.runtime.shutdown().await;

    // The control owner itself reports both killed children.
    let job = live_job("plugin-job-kill-owner").await;
    let outcome = control(
        &ConfigAccess::local(job.h.db.node.clone()),
        &parent_scope(&job.h),
        TOOL_CALL,
        JobControlKind::Stop,
    )
    .await;
    assert!(
        matches!(
            outcome,
            Ok(JobControlOutcome::Applied {
                killed_children: 2,
                ..
            })
        ),
        "{outcome:?}"
    );
    let replay = control(
        &ConfigAccess::local(job.h.db.node.clone()),
        &parent_scope(&job.h),
        TOOL_CALL,
        JobControlKind::Stop,
    )
    .await
    .expect("replay stop");
    assert_eq!(
        replay,
        JobControlOutcome::AlreadyTerminal {
            state: JobState::Cancelled
        }
    );
    assert_stopped(&job, "owner stop and replay").await;
    job.h.runtime.shutdown().await;
}

const PEER_CHILD: &str = "peer-child";
const NETWORK_ID: &str = "net-plugin-job-kill";
const NETWORK_NAME: &str = "Plugin Job Kill Net";

/// Node A runs a job whose slot `a` is a local child and whose slot `b` is a
/// child on node B, with both nodes paired. Returns A's harness, B's runtime
/// and B's store.
async fn paired_job(name: &str) -> (JobHarness, BootedAgent, TestDb, tempfile::TempDir) {
    let session_id = format!("{name}-session");
    let prompt_a = format!("{name} child a");
    let prompt_b = format!("{name} child b");
    let db_a = test_p2p_db(&format!("{name}-a")).await;
    let db_b = test_p2p_db(&format!("{name}-b")).await;
    let identity_a: Arc<dyn NodeIdentity> = db_a.node_identity.clone();
    let identity_b: Arc<dyn NodeIdentity> = db_b.node_identity.clone();
    let did_a = identity_a.did().to_string();
    let did_b = identity_b.did().to_string();
    let plugin_home = tempfile::tempdir().expect("plugin home");

    // Node B hosts the peer child's agent behind a held backend.
    gents::ensure_node(&db_b.node, &did_b)
        .await
        .expect("create node B's node document");
    configure_child_agent(
        &db_b.node,
        &did_b,
        PEER_CHILD,
        &format!("{PEER_CHILD}:tools"),
        Vec::new(),
        false,
    )
    .await;
    let backend_b = MockStreamingBackend::start_with_plans(MODEL, vec![held_child(&prompt_b)])
        .expect("start node B's backend");
    bind_agent_backend(
        &db_b.node,
        &did_b,
        PEER_CHILD,
        "plugin-job-peer-backend",
        backend_b.endpoint(),
        MODEL,
    )
    .await;

    // Node A lists both targets for the job and for its parent agent.
    let settings = PluginJobSettings {
        agent_targets: vec![CHILD.into(), PEER_CHILD.into()],
        ..job_settings()
    };
    install_job_fixture(&db_a, plugin_home.path(), settings.clone()).await;
    configure_agent_tools(
        &db_a.node,
        &did_a,
        PARENT,
        None,
        Tools {
            tools_id: format!("{PARENT}:tools"),
            node_did: did_a.clone(),
            agents: Some(AgentTools {
                target_ids: vec![CHILD.into(), PEER_CHILD.into()],
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
        vec![
            (
                gents::Collection::AgentTarget,
                serde_json::to_value(agent_target(&did_a, CHILD, &did_a, CHILD))
                    .expect("serialize the local target"),
            ),
            (
                gents::Collection::AgentTarget,
                serde_json::to_value(agent_target(&did_a, PEER_CHILD, &did_b, PEER_CHILD))
                    .expect("serialize the peer target"),
            ),
        ],
    )
    .await;

    // Both nodes enroll each other.
    let (peer_a, addr_a) = wait_for_peer_identity(db_a.node.as_ref()).await;
    let (peer_b, addr_b) = wait_for_peer_identity(db_b.node.as_ref()).await;
    authorize_enrollment_peer(
        db_a.node.clone(),
        NETWORK_ID,
        NETWORK_NAME,
        identity_a.clone(),
        identity_b.clone(),
        &peer_b,
        &addr_b,
    )
    .await;
    authorize_enrollment_peer(
        db_b.node.clone(),
        NETWORK_ID,
        NETWORK_NAME,
        identity_b.clone(),
        identity_a.clone(),
        &peer_a,
        &addr_a,
    )
    .await;

    let agent_b = Gents::from_default_agent_documents(
        db_b.node.clone(),
        identity_b,
        DocumentRuntimeOptions::default(),
    )
    .await
    .expect("build node B's runtime");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent_b.run(shutdown_rx));
    wait_for_runtime_ready(db_b.node.as_ref(), &did_b).await;
    let runtime_b = BootedAgent::new(shutdown_tx, handle, did_b);

    let cwd = db_a.data_path().join("job_cwd");
    std::fs::create_dir_all(&cwd).expect("create the job working folder");
    let cwd = cwd
        .canonicalize()
        .expect("resolve the job working folder")
        .to_string_lossy()
        .into_owned();
    let prepared = prepare_accepted_turn(
        &db_a,
        AcceptedTurnSpec {
            backend_id: "plugin-job-backend",
            model: MODEL,
            parent_agent_id: PARENT,
            configured_agent_ids: &[PARENT, CHILD],
            request_id: &format!("{session_id}-parent"),
            session_id: &session_id,
            prompt: &format!("{session_id} start"),
            accepted_chunks: vec![job_call(
                TOOL_CALL,
                &two_child_plan_on(&prompt_a, &prompt_b, PEER_CHILD),
            )],
            child_plans: vec![held_child(&prompt_a)],
            valid_until: None,
            request_hop: None,
            request_setup: Some(Box::new(move |request| request.input.cwd = Some(cwd))),
        },
    )
    .await;
    let agent_a = Gents::from_default_agent_documents(
        db_a.node.clone(),
        identity_a,
        DocumentRuntimeOptions {
            plugin_home: Some(plugin_home.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
    .expect("build node A's runtime");
    let runtime = boot_prepared_accepted_turn(&db_a, prepared, agent_a).await;
    wait_for_connected_peer(db_a.node.as_ref()).await;
    wait_for_connected_peer(db_b.node.as_ref()).await;
    let h = JobHarness {
        db: db_a,
        plugin_home: plugin_home.path().to_path_buf(),
        runtime,
        session_id,
    };
    wait_job(
        &h,
        TOOL_CALL,
        |view| {
            view.children.len() == 2
                && view
                    .children
                    .iter()
                    .all(|child| child.state == JobChildState::Live)
        },
        Duration::from_secs(30),
    )
    .await;
    (h, runtime_b, db_b, plugin_home)
}

/// Stops the job and checks that the row ends now with both children
/// interrupted: the local one and the one addressed to node B.
async fn stop_paired_job(name: &str, peer_reachable: bool) {
    let (h, runtime_b, db_b, _plugin_home) = paired_job(name).await;
    let did_a = h.db.node_identity.did().to_string();
    let did_b = db_b.node_identity.did().to_string();
    let row_doc_id = job_row(&h, TOOL_CALL).await;
    if !peer_reachable {
        runtime_b.shutdown().await;
        drop(db_b);
    }

    let outcome = control(
        &ConfigAccess::local(h.db.node.clone()),
        &parent_scope(&h),
        TOOL_CALL,
        JobControlKind::Stop,
    )
    .await;
    assert!(
        matches!(
            outcome,
            Ok(JobControlOutcome::Applied {
                killed_children: 2,
                ..
            })
        ),
        "{name}: {outcome:?}"
    );
    assert_eq!(row_state(&h, &row_doc_id).await, "cancelled", "{name}");
    let mut children = children(&h, &row_doc_id).await;
    children.sort();
    let owners: Vec<&str> = children.iter().map(|(node, _)| node.as_str()).collect();
    let mut expected = vec![did_a.as_str(), did_b.as_str()];
    expected.sort();
    assert_eq!(owners, expected, "{name}: {children:?}");
    assert!(
        children
            .iter()
            .all(|(_, interrupted)| interrupted.is_some()),
        "{name}: the local and the peer child are both interrupted: {children:?}"
    );
    h.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_with_local_and_peer_children_interrupts_both_and_ends_the_row_now() {
    stop_paired_job("plugin-job-kill-peer", true).await;
    stop_paired_job("plugin-job-kill-peer-down", false).await;
}
