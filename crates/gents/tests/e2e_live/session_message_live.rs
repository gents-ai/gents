use gents_protocol::tool_names::{
    AGENT_INTERRUPT_TOOL_NAME, AGENT_MESSAGE_TOOL_NAME, AGENT_NEW_TOOL_NAME,
};
use rapidhash::{HashSetExt, RapidHashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use anyhow::Result;
use gents::{DocumentRuntimeOptions, Gents};
use gents_automation::goal::seed_goal_from_access;
use gents_documents::{AgentTargetDocument, BashMode};
use gents_documents::{AgentTools, BashTools, HostTools, Tools};
use gents_observe::observe::run_timeline_fetch::load_run_timeline_rows;
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::graphql::escape_graphql_string;
use gents_store::{collection::Collection, identity::NodeIdentity};
use gents_tools::surface::ToolCeiling;
use serde::Deserialize;

use crate::support::fixtures::{agent_target, configure_agent_tools};
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::live_inference::{
    live_target, terminal_assistant_answer, wait_for_assistant_answer, wait_for_request_terminal,
    InferenceTarget, LiveAgentSpec, LiveUser,
};
use crate::support::{
    first_optional_row,
    snapshots::{fetch_runtime_snapshot, SnapshotChanges, SnapshotReader},
    test_db, TestDb,
};

const RESEARCHER_AGENT_NAME: &str = "live-researcher";
const BACKGROUND_WORKER_AGENT_NAME: &str = "live-background-worker";
const RESEARCHER_TARGET_NAME: &str = "researcher";
const BACKGROUND_WORKER_TARGET_NAME: &str = "background-worker";

static LIVE_TRACE_INIT: Once = Once::new();

fn init_live_test_tracing() {
    LIVE_TRACE_INIT.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("gents=debug"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    });
}

fn completion_marker(tool_call_id: &str, tool_name: &str) -> String {
    format!(r#"<tool-completion tool_call_id="{tool_call_id}" tool_name="{tool_name}""#)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_local_create_session() -> Result<()> {
    let target = live_target().await;

    let db = test_db("session-message-live-local").await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let node_did = identity.did().to_string();
    let profile_id = format!("{node_did}:orchestrator-inference");
    let live = LiveUser::install(&db, &target).await;
    let orchestrator_agent_did = configure_agent(
        &live,
        "Orchestrator",
        &node_did,
        &target,
        &profile_id,
        ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    let researcher_agent_did = configure_agent(
        &live,
        RESEARCHER_AGENT_NAME,
        &node_did,
        &target,
        &profile_id,
        "You answer the user's question concisely and factually in one short sentence.",
        Some("Researches factual questions and returns a concise factual answer."),
        false,
    )
    .await;
    authorize_session_targets(
        &db,
        &node_did,
        &orchestrator_agent_did,
        vec![AgentTargetDocument {
            description: Some("Researches factual questions.".to_string()),
            ..agent_target(
                &node_did,
                RESEARCHER_TARGET_NAME,
                node_did.clone(),
                researcher_agent_did.as_str(),
            )
        }],
    )
    .await;

    let agent = boot_document_agent(&db, identity).await?;

    let request_id = "req-live-local-create-session";
    let session_id = "session-live-local-create-session";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        request_id,
        session_id,
        "Use your research agent to find the capital of France, then tell me the answer.",
    )
    .await;

    wait_for_background_tool_call(
        &live,
        request_id,
        session_id,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(180),
    )
    .await;
    let Some(caused) = wait_for_caused_request(&live, request_id, Duration::from_secs(120)).await
    else {
        dump_session_diagnostics(&live, session_id).await;
        panic!("agent_new must cause an AgentRequest linked to the orchestrator request");
    };
    tracing::info!("[live-local] caused request = {caused:?}");
    assert_eq!(
        caused.caused_by_parent_request_id.as_deref(),
        Some(request_id)
    );
    let row = fetch_tool_call(
        &live,
        request_id,
        session_id,
        caused
            .caused_by_parent_tool_call_id
            .as_deref()
            .expect("a caused request names its agent_new call"),
    )
    .await
    .expect("the caused request must name an agent_new call of the orchestrator request");
    assert_eq!(caused.agent_did, researcher_agent_did.as_str());
    assert_eq!(caused.node_did, node_did);
    // A caused request's requester is the Agent whose tool call caused it.
    assert_eq!(
        caused.requester_did.as_deref(),
        Some(orchestrator_agent_did.as_str())
    );
    assert_eq!(caused.request_hop, Some(1));
    assert_ne!(
        caused.session_id, session_id,
        "agent_new must start a new session"
    );

    let caused_terminal =
        wait_for_request_terminal(&live, &caused.request_id, Duration::from_secs(180)).await;
    assert_eq!(caused_terminal, "completed");
    let caused_answer =
        wait_for_assistant_answer(&live, &caused.request_id, Duration::from_secs(30)).await;
    tracing::info!("[live-local] started session answer = {caused_answer:?}");
    assert!(
        !caused_answer.trim().is_empty(),
        "the started session must produce a non-empty assistant response"
    );
    if !caused_answer.to_lowercase().contains("paris") {
        tracing::warn!("[live-local] SOFT-WARN: answer did not contain 'Paris': {caused_answer:?}");
    }

    let settled = wait_for_tool_call_state(
        &live,
        request_id,
        session_id,
        &row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(
        settled.child_request_id.as_deref(),
        Some(caused.request_id.as_str()),
        "the run timeline must link the agent_new row to the request it caused"
    );
    let messages = load_session_messages(&live, request_id, session_id).await;
    let receipt = session_receipt(&messages, &caused.request_id)
        .unwrap_or_else(|| panic!("agent_new receipt missing; transcript={messages:#?}"));
    assert_eq!(receipt["session_id"], caused.session_id.as_str());
    assert_eq!(receipt["tool_call_id"], row.tool_call_id.as_str());
    assert_eq!(receipt["await_mode"], "background");
    wait_for_message_containing(
        &live,
        request_id,
        session_id,
        &completion_marker(&row.tool_call_id, AGENT_NEW_TOOL_NAME),
        Duration::from_secs(60),
    )
    .await;
    let wake =
        wait_for_background_wake(&live, session_id, request_id, Duration::from_secs(60)).await;
    let wake_state =
        wait_for_request_terminal(&live, &wake.request_id, Duration::from_secs(180)).await;
    assert_eq!(wake_state, "completed");

    let parent_terminal =
        wait_for_request_terminal(&live, request_id, Duration::from_secs(60)).await;
    assert_eq!(parent_terminal, "completed");

    agent.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_standard_backgrounding_uses_real_inference() -> Result<()> {
    init_live_test_tracing();

    let target = live_target().await;

    let workspace = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .expect("create temporary workspace task")
        .expect("backgrounding live workspace");
    let child_release = workspace.path().join("release-child");
    let tool_release = workspace.path().join("release-tool");
    let managed_child_release = workspace.path().join("release-managed-child");
    let managed_tool_release = workspace.path().join("release-managed-tool");
    let blocked_command = |started: &str, release: &Path, done: &str| {
        serde_json::json!({
            "command": format!(
                "printf {started}; printf {started} > '{}'; while [ ! -f '{}' ]; do sleep 0.2; done; printf {done}",
                started_path(release).display(),
                release.display()
            ),
            "args": [],
            "timeout_secs": 180
        })
    };
    let child_tool_args = blocked_command(
        "CHILD_BACKGROUND_STARTED",
        &child_release,
        "CHILD_BACKGROUND_DONE",
    );
    let native_tool_args = blocked_command(
        "NATIVE_BACKGROUND_STARTED",
        &tool_release,
        "NATIVE_BACKGROUND_DONE",
    );
    let managed_child_tool_args = blocked_command(
        "CHILD_MANAGED_STARTED",
        &managed_child_release,
        "CHILD_MANAGED_DONE",
    );
    let managed_native_tool_args = blocked_command(
        "NATIVE_MANAGED_STARTED",
        &managed_tool_release,
        "NATIVE_MANAGED_DONE",
    );

    let parent_system_prompt = "Follow the user's requested tool and arguments. Do not await a background job unless the user explicitly requests wait_process. After the requested tool returns, acknowledge briefly and call no other tool. When a background completion notification arrives, acknowledge it without calling tools.";
    let child_system_prompt = format!(
        r#"You are the deterministic background worker in an integration test.
When the latest request is exactly RUN_CHILD_BACKGROUND_JOB, call bash_unrestricted exactly once with these arguments: {child_tool_args}
Wait for that foreground tool call to finish, then reply exactly CHILD_BACKGROUND_DONE. Do not call any other tool.
When the latest request is exactly RUN_MANAGED_CHILD_BACKGROUND_JOB, call bash_unrestricted exactly once with these arguments: {managed_child_tool_args}
Wait for that foreground tool call to finish, then reply exactly CHILD_MANAGED_STARTED CHILD_MANAGED_DONE. Do not call any other tool.
If you receive a message STEERING_NOTE, do not call any tool for it; append STEERING_ACK to your final reply."#
    );

    let db = test_db("backgrounding-live-standard-path").await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let node_did = identity.did().to_string();
    let profile_id = format!("{node_did}:orchestrator-inference");
    let live = LiveUser::install(&db, &target).await;
    let orchestrator_agent_did = configure_agent(
        &live,
        "Orchestrator",
        &node_did,
        &target,
        &profile_id,
        parent_system_prompt,
        None,
        true,
    )
    .await;
    let background_worker_agent_did = configure_agent(
        &live,
        BACKGROUND_WORKER_AGENT_NAME,
        &node_did,
        &target,
        &profile_id,
        &child_system_prompt,
        Some("Runs a deliberately blocked background integration-test job."),
        false,
    )
    .await;
    configure_standard_backgrounding_tools(
        &db,
        &node_did,
        &orchestrator_agent_did,
        &background_worker_agent_did,
        workspace.path(),
    )
    .await;

    gents_inference::backends::registry::probe_and_promote_enabled_backends(&db.node).await;
    let loaded_agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::readwrite(workspace.path()).with_command_timeout_secs(180),
            ..Default::default()
        },
    )
    .await?;

    let agent = boot_loaded_document_agent(&db, loaded_agent).await;
    let agent_request_id = "req-live-standard-background-agent";
    let agent_session_id = "session-live-standard-background-agent";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        agent_request_id,
        agent_session_id,
        "RUN_BACKGROUND_AGENT: invoke agent_new now for background-worker with prompt RUN_CHILD_BACKGROUND_JOB. Do not answer until its running receipt arrives.",
    )
    .await;

    let session_row = wait_for_background_tool_call(
        &live,
        agent_request_id,
        agent_session_id,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(180),
    )
    .await;
    let parent_state =
        wait_for_request_terminal(&live, agent_request_id, Duration::from_secs(180)).await;
    assert_eq!(parent_state, "completed");
    let parent_answer =
        wait_for_assistant_answer(&live, agent_request_id, Duration::from_secs(30)).await;
    assert!(
        !parent_answer.trim().is_empty(),
        "parent did not persist a reply after the agent_new receipt: {parent_answer:?}"
    );
    let caused = wait_for_caused_request(&live, agent_request_id, Duration::from_secs(60))
        .await
        .expect("agent_new must cause a request");
    assert_eq!(caused.agent_did, background_worker_agent_did.as_str());
    assert_eq!(
        caused.caused_by_parent_tool_call_id.as_deref(),
        Some(session_row.tool_call_id.as_str())
    );
    let caused_state = caused
        .lifecycle_state
        .as_ref()
        .expect("caused request lifecycle")
        .as_str()
        .to_owned();
    assert!(
        !is_terminal(&caused_state),
        "parent blocked on the started session; it was already {caused_state}"
    );
    assert!(
        fetch_runtime_snapshot(&db.node, &node_did)
            .await
            .is_some_and(|snapshot| snapshot.process_state == "ready"),
        "runtime must remain ready while the started session runs; caused={caused:?}"
    );
    let running_row = fetch_tool_call(
        &live,
        agent_request_id,
        agent_session_id,
        &session_row.tool_call_id,
    )
    .await
    .expect("agent_new row after parent completion");
    assert_eq!(
        running_row.lifecycle_state, "running",
        "the agent_new row must stay running until its caused request terminalizes"
    );
    let messages = load_session_messages(&live, agent_request_id, agent_session_id).await;
    let receipt = session_receipt(&messages, &caused.request_id)
        .unwrap_or_else(|| panic!("agent_new receipt missing; transcript={messages:#?}"));
    assert_eq!(receipt["status"], "running");
    assert_eq!(receipt["session_id"], caused.session_id.as_str());

    tokio::fs::write(&child_release, b"release")
        .await
        .expect("release started session");
    let caused_terminal =
        wait_for_request_terminal(&live, &caused.request_id, Duration::from_secs(180)).await;
    assert_eq!(caused_terminal, "completed");

    let caused_answer = terminal_assistant_answer(&live, &caused.request_id).await;
    assert!(
        !caused_answer.trim().is_empty(),
        "completed session lacks its selected canonical terminal output: {caused_answer:?}"
    );
    wait_for_tool_call_state(
        &live,
        agent_request_id,
        agent_session_id,
        &session_row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    wait_for_completion_notification(&live, &session_row.doc_id, Duration::from_secs(60)).await;
    assert_wake_observed(&live, agent_session_id, agent_request_id).await;
    let tool_request_id = "req-live-standard-background-tool";
    let tool_session_id = "session-live-standard-background-tool";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        tool_request_id,
        tool_session_id,
        &format!("Call spawn_process with tool_name bash_unrestricted and args {native_tool_args}. After its running receipt, acknowledge briefly without waiting or calling other tools."),
    )
    .await;

    let background_tool = wait_for_background_tool_call(
        &live,
        tool_request_id,
        tool_session_id,
        "bash_unrestricted",
        Duration::from_secs(180),
    )
    .await;
    assert!(
        background_tool.child_request_id.is_none(),
        "native background tool must use the childless lane"
    );
    let tool_parent_state =
        wait_for_request_terminal(&live, tool_request_id, Duration::from_secs(180)).await;
    assert_eq!(tool_parent_state, "completed");
    let tool_parent_answer =
        wait_for_assistant_answer(&live, tool_request_id, Duration::from_secs(30)).await;
    assert!(
        !tool_parent_answer.trim().is_empty(),
        "parent did not persist a reply after the background process receipt: {tool_parent_answer:?}"
    );
    let still_running = fetch_tool_call(
        &live,
        tool_request_id,
        tool_session_id,
        &background_tool.tool_call_id,
    )
    .await
    .expect("background tool after parent completion");
    assert_eq!(
        still_running.lifecycle_state, "running",
        "parent blocked on the native background tool"
    );

    tokio::fs::write(&tool_release, b"release")
        .await
        .expect("release native background tool");
    let completed_tool = wait_for_tool_call_state(
        &live,
        tool_request_id,
        tool_session_id,
        &background_tool.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    let tool_result = completed_tool.result.as_deref().unwrap_or_default();
    assert!(
        tool_result.contains("NATIVE_BACKGROUND_STARTED")
            && tool_result.contains("NATIVE_BACKGROUND_DONE"),
        "native background result was not durably persisted: {tool_result:?}"
    );
    wait_for_completion_notification(&live, &background_tool.doc_id, Duration::from_secs(60)).await;
    assert_wake_observed(&live, tool_session_id, tool_request_id).await;

    assert_not_started(&managed_child_release).await;
    let managed_agent_session_id = "session-live-managed-background-agent";
    let managed_create_request_id = "req-live-managed-background-agent-create";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        managed_create_request_id,
        managed_agent_session_id,
        "MANAGE_BACKGROUND_AGENT_CREATE: Call agent_new exactly once now with agent background-worker and prompt RUN_MANAGED_CHILD_BACKGROUND_JOB. Do not call any other tool.",
    )
    .await;
    let managed_row = wait_for_background_tool_call(
        &live,
        managed_create_request_id,
        managed_agent_session_id,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&live, managed_create_request_id, Duration::from_secs(180)).await,
        "completed"
    );
    let managed_caused =
        wait_for_caused_request(&live, managed_create_request_id, Duration::from_secs(60))
            .await
            .expect("managed agent_new must cause a request");
    wait_for_model_tool_call(
        &live,
        &managed_caused.request_id,
        &managed_caused.session_id,
        "bash_unrestricted",
        Duration::from_secs(180),
    )
    .await;
    wait_for_started_marker(&live, "CHILD_MANAGED_STARTED", Duration::from_secs(120)).await;

    let managed_list_request_id = "req-live-managed-background-agent-list";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        managed_list_request_id,
        managed_agent_session_id,
        "MANAGE_BACKGROUND_AGENT_LIST: Call list_processes exactly once now. Do not call any other tool.",
    )
    .await;
    wait_for_tool_result_containing(
        &live,
        managed_list_request_id,
        managed_agent_session_id,
        &[managed_row.tool_call_id.as_str(), AGENT_NEW_TOOL_NAME],
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&live, managed_list_request_id, Duration::from_secs(180)).await,
        "completed"
    );

    let managed_state = fetch_request_lifecycle(&live, &managed_caused.request_id)
        .await
        .expect("managed caused lifecycle");
    assert!(
        !is_terminal(&managed_state),
        "the started session finished ({managed_state}) before it could be steered: the live model did not run the blocking command"
    );
    let managed_message_request_id = "req-live-managed-background-agent-message";
    let managed_message_prompt = format!(
        "MANAGE_BACKGROUND_AGENT_MESSAGE: Call agent_message exactly once now with session_id {:?} and message \"STEERING_NOTE\". Do not call any other tool.",
        managed_caused.session_id
    );
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        managed_message_request_id,
        managed_agent_session_id,
        &managed_message_prompt,
    )
    .await;
    let message_row = wait_for_background_tool_call(
        &live,
        managed_message_request_id,
        managed_agent_session_id,
        AGENT_MESSAGE_TOOL_NAME,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&live, managed_message_request_id, Duration::from_secs(180))
            .await,
        "completed"
    );
    let messages =
        load_session_messages(&live, managed_message_request_id, managed_agent_session_id).await;
    let message_receipt = session_receipts(&messages)
        .into_iter()
        .find(|receipt| receipt["tool_call_id"] == message_row.tool_call_id.as_str())
        .unwrap_or_else(|| panic!("agent_message receipt missing; transcript={messages:#?}"));
    assert_eq!(
        message_receipt["session_id"],
        managed_caused.session_id.as_str()
    );
    assert_eq!(
        message_receipt["delivery"], "queue",
        "a message to a busy started session must be queued behind its active turn"
    );
    assert!(
        !is_terminal(
            &fetch_request_lifecycle(&live, &managed_caused.request_id)
                .await
                .expect("managed caused lifecycle")
        ),
        "agent_message must have been exercised against a live session"
    );
    assert_eq!(
        fetch_tool_call(
            &live,
            managed_create_request_id,
            managed_agent_session_id,
            &managed_row.tool_call_id,
        )
        .await
        .expect("managed agent_new row")
        .lifecycle_state,
        "running"
    );

    tokio::fs::write(&managed_child_release, b"release")
        .await
        .expect("release managed session");
    assert_eq!(
        wait_for_request_terminal(&live, &managed_caused.request_id, Duration::from_secs(240))
            .await,
        "completed"
    );
    let managed_answer = terminal_assistant_answer(&live, &managed_caused.request_id).await;
    assert!(
        !managed_answer.trim().is_empty(),
        "managed session lacks its terminal output: {managed_answer:?}"
    );

    wait_for_tool_call_state(
        &live,
        managed_create_request_id,
        managed_agent_session_id,
        &managed_row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    wait_for_tool_call_settled(
        &live,
        managed_message_request_id,
        managed_agent_session_id,
        &message_row.tool_call_id,
        Duration::from_secs(180),
    )
    .await;
    wait_for_completion_notification(&live, &managed_row.doc_id, Duration::from_secs(60)).await;
    wait_for_completion_notification(&live, &message_row.doc_id, Duration::from_secs(60)).await;

    let managed_tool_spawn_prompt = format!(
        "MANAGE_BACKGROUND_TOOL_SPAWN: Call spawn_process exactly once now with tool_name bash_unrestricted and args exactly {managed_native_tool_args}. Do not call any other tool."
    );
    let managed_tool_request_id = "req-live-managed-background-tool-spawn";
    let managed_tool_session_id = "session-live-managed-background-tool";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        managed_tool_request_id,
        managed_tool_session_id,
        &managed_tool_spawn_prompt,
    )
    .await;

    let managed_tool = wait_for_background_tool_call(
        &live,
        managed_tool_request_id,
        managed_tool_session_id,
        "bash_unrestricted",
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&live, managed_tool_request_id, Duration::from_secs(180),).await,
        "completed"
    );
    let managed_process_handle = managed_tool.tool_call_id.clone();

    let managed_tool_list_request_id = "req-live-managed-background-tool-list";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        managed_tool_list_request_id,
        managed_tool_session_id,
        "MANAGE_BACKGROUND_TOOL_LIST: Call list_processes exactly once now. Do not call any other tool.",
    )
    .await;
    wait_for_tool_result_containing(
        &live,
        managed_tool_list_request_id,
        managed_tool_session_id,
        &[managed_process_handle.as_str(), "bash_unrestricted"],
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            &live,
            managed_tool_list_request_id,
            Duration::from_secs(180)
        )
        .await,
        "completed"
    );

    let managed_tool_read_request_id = "req-live-managed-background-tool-read-running";
    let managed_tool_read_prompt = format!(
        "MANAGE_BACKGROUND_TOOL_READ_RUNNING: Call read_process exactly once now with tool_call_id {managed_process_handle:?} and offset 0. Do not call wait_process or any other tool."
    );
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        managed_tool_read_request_id,
        managed_tool_session_id,
        &managed_tool_read_prompt,
    )
    .await;
    wait_for_model_tool_call(
        &live,
        managed_tool_read_request_id,
        managed_tool_session_id,
        "read_process",
        Duration::from_secs(180),
    )
    .await;
    wait_for_tool_result_containing(
        &live,
        managed_tool_read_request_id,
        managed_tool_session_id,
        &["NATIVE_MANAGED_STARTED"],
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            &live,
            managed_tool_read_request_id,
            Duration::from_secs(180)
        )
        .await,
        "completed"
    );

    let managed_tool_wait_request_id = "req-live-managed-background-tool-wait";
    let managed_tool_wait_prompt = format!(
        "MANAGE_BACKGROUND_TOOL_WAIT: Call wait_process exactly once now with tool_call_id {managed_process_handle:?}. Do not call any other tool."
    );
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        managed_tool_wait_request_id,
        managed_tool_session_id,
        &managed_tool_wait_prompt,
    )
    .await;
    wait_for_model_tool_call(
        &live,
        managed_tool_wait_request_id,
        managed_tool_session_id,
        "wait_process",
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        fetch_tool_call(
            &live,
            managed_tool_request_id,
            managed_tool_session_id,
            &managed_tool.tool_call_id,
        )
        .await
        .expect("managed native tool during read_process")
        .lifecycle_state,
        "running",
        "read_process must observe output before the native process exits"
    );
    tokio::fs::write(&managed_tool_release, b"release")
        .await
        .expect("release managed native background tool");

    let managed_tool_wait_state = wait_for_request_terminal(
        &live,
        managed_tool_wait_request_id,
        Duration::from_secs(240),
    )
    .await;
    assert_eq!(managed_tool_wait_state, "completed");

    let managed_tool_terminal_read_request_id = "req-live-managed-background-tool-read-terminal";
    let managed_tool_terminal_read_prompt = format!(
        "MANAGE_BACKGROUND_TOOL_READ_TERMINAL: Call read_process exactly once now with tool_call_id {managed_process_handle:?} and offset 0. Do not call any other tool."
    );
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        managed_tool_terminal_read_request_id,
        managed_tool_session_id,
        &managed_tool_terminal_read_prompt,
    )
    .await;
    wait_for_model_tool_call(
        &live,
        managed_tool_terminal_read_request_id,
        managed_tool_session_id,
        "read_process",
        Duration::from_secs(180),
    )
    .await;
    wait_for_tool_result_containing(
        &live,
        managed_tool_terminal_read_request_id,
        managed_tool_session_id,
        &["NATIVE_MANAGED_DONE"],
        Duration::from_secs(60),
    )
    .await;
    let managed_tool_state = wait_for_request_terminal(
        &live,
        managed_tool_terminal_read_request_id,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(managed_tool_state, "completed");
    let managed_tool_answer = wait_for_assistant_answer(
        &live,
        managed_tool_terminal_read_request_id,
        Duration::from_secs(30),
    )
    .await;
    assert!(
        !managed_tool_answer.trim().is_empty(),
        "the inspection request did not persist a reply: {managed_tool_answer:?}"
    );

    agent.shutdown().await;
    Ok(())
}

const ALPHA_AGENT_NAME: &str = "live-alpha";
const BETA_AGENT_NAME: &str = "live-beta";
const BLOCKER_AGENT_NAME: &str = "live-blocker";
const RELAY_AGENT_NAME: &str = "live-relay";

fn code_word(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        uuid::Uuid::new_v4().simple().to_string()[..8].to_uppercase()
    )
}

fn code_worker_prompt(name: &str, first: &str, second: &str) -> String {
    format!(
        "You are agent {name}. Your code word is {first}. Your second code word is {second}. \
When asked for your code word, reply with only {first}. When asked for your second code word, \
reply with only {second}. Never call any tool."
    )
}

const DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are an orchestrator in an integration \
test. Follow the latest user instruction exactly, calling only the tools it names. Never answer \
a code-word question yourself. When background completion notifications arrive, do not call any \
tool: reply with one short sentence that repeats, verbatim, every code word reported in all the \
notifications you have received so far in this conversation.";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_fan_out_list_and_continue() -> Result<()> {
    init_live_test_tracing();
    let target = live_target().await;

    let alpha_first = code_word("ALPHA");
    let alpha_second = code_word("ALPHATWO");
    let beta_first = code_word("BETA");

    let db = test_db("session-message-live-fan-out").await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let node_did = identity.did().to_string();
    let profile_id = format!("{node_did}:orchestrator-inference");
    let live = LiveUser::install(&db, &target).await;
    let orchestrator_agent_did = configure_agent(
        &live,
        "Orchestrator",
        &node_did,
        &target,
        &profile_id,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    let alpha_agent_did = configure_agent(
        &live,
        ALPHA_AGENT_NAME,
        &node_did,
        &target,
        &profile_id,
        &code_worker_prompt("alpha", &alpha_first, &alpha_second),
        Some("Knows a code word."),
        false,
    )
    .await;
    let beta_agent_did = configure_agent(
        &live,
        BETA_AGENT_NAME,
        &node_did,
        &target,
        &profile_id,
        &code_worker_prompt("beta", &beta_first, &code_word("BETATWO")),
        Some("Knows a code word."),
        false,
    )
    .await;
    authorize_session_targets(
        &db,
        &node_did,
        &orchestrator_agent_did,
        vec![
            agent_target(
                &node_did,
                "alpha",
                node_did.clone(),
                alpha_agent_did.as_str(),
            ),
            agent_target(&node_did, "beta", node_did.clone(), beta_agent_did.as_str()),
        ],
    )
    .await;
    let agent = boot_document_agent(&db, identity).await?;

    let session_id = "session-live-fan-out";
    let fan_out_request_id = "req-live-fan-out";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        fan_out_request_id,
        session_id,
        "Call agent_new twice in this turn: once with agent \"alpha\" and prompt \"What is your code word?\", and once with agent \"beta\" and prompt \"What is your code word?\". After both running receipts arrive, reply exactly STARTED_BOTH and call no other tool.",
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&live, fan_out_request_id, Duration::from_secs(240)).await,
        "completed"
    );
    let caused =
        wait_for_caused_requests(&live, fan_out_request_id, 2, Duration::from_secs(60)).await;
    let rows = session_tool_rows(&live, session_id, AGENT_NEW_TOOL_NAME).await;
    assert_eq!(
        caused.len(),
        2,
        "each agent_new call must cause exactly one request: {caused:?}"
    );
    let alpha = caused
        .iter()
        .find(|row| row.agent_did == alpha_agent_did.as_str())
        .unwrap_or_else(|| panic!("no alpha session was started; caused={caused:?}"))
        .clone();
    let beta = caused
        .iter()
        .find(|row| row.agent_did == beta_agent_did.as_str())
        .unwrap_or_else(|| panic!("no beta session was started; caused={caused:?}"))
        .clone();
    for started in [&alpha, &beta] {
        assert_eq!(started.request_hop, Some(1));
        // agent_new admits the child as a client request of the calling Agent.
        assert_eq!(started.admission_kind.as_deref(), Some("client"));
        assert_ne!(started.session_id, session_id);
        let row = rows
            .iter()
            .find(|row| {
                Some(row.tool_call_id.as_str()) == started.caused_by_parent_tool_call_id.as_deref()
            })
            .unwrap_or_else(|| panic!("caused request names no agent_new row: {started:?}"));
        assert_eq!(row.await_mode.as_deref(), Some("background"));
    }
    assert_ne!(alpha.session_id, beta.session_id);
    for started in [&alpha, &beta] {
        assert_eq!(
            wait_for_request_terminal(&live, &started.request_id, Duration::from_secs(240)).await,
            "completed"
        );
        wait_for_completion_notification(
            &live,
            &rows
                .iter()
                .find(|row| {
                    Some(row.tool_call_id.as_str())
                        == started.caused_by_parent_tool_call_id.as_deref()
                })
                .expect("the child is linked to its tool")
                .doc_id,
            Duration::from_secs(60),
        )
        .await;
    }
    wait_for_session_quiescent(&live, session_id, Duration::from_secs(240)).await;
    let delivered = rows
        .iter()
        .map(|row| row.doc_id.as_str())
        .collect::<Vec<_>>();
    let wakes = assert_deliveries_bound_to_completed_wakes(&live, session_id, &delivered).await;
    tracing::info!(
        parent_session = session_id,
        alpha_session = %alpha.session_id,
        beta_session = %beta.session_id,
        wakes = ?wakes,
        "[live-fan-out] each result reached the parent through a completed wake"
    );
    let list_request_id = "req-live-fan-out-list";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        list_request_id,
        session_id,
        "Call agent_list exactly once and agent_session_list exactly once now, then reply exactly LISTED and call no other tool.",
    )
    .await;
    let listed = wait_for_json_tool_result(
        &live,
        list_request_id,
        session_id,
        |value| value.get("sessions").is_some(),
        Duration::from_secs(240),
    )
    .await;
    for started in [&alpha, &beta] {
        let entry = listed["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["session_id"] == started.session_id.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "agent_session_list omitted started session {}: {listed}",
                    started.session_id
                )
            });
        assert_eq!(entry["relationship"], "started_by_you");
        assert_eq!(entry["node_did"], node_did.as_str());
        assert_eq!(entry["can_message"], true);
        assert_eq!(entry["can_interrupt"], true);
        assert_eq!(entry["status"], "idle");
    }
    let targets = wait_for_json_tool_result(
        &live,
        list_request_id,
        session_id,
        |value| value.get("agents").is_some(),
        Duration::from_secs(240),
    )
    .await;
    let listed_agents = targets["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["agent"].as_str())
        .collect::<RapidHashSet<_>>();
    assert_eq!(listed_agents, RapidHashSet::from_iter(["alpha", "beta"]));
    assert_eq!(
        wait_for_request_terminal(&live, list_request_id, Duration::from_secs(180)).await,
        "completed"
    );
    wait_for_session_quiescent(&live, session_id, Duration::from_secs(120)).await;
    let message_request_id = "req-live-fan-out-message";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        message_request_id,
        session_id,
        &format!(
            "Call agent_message exactly once now with session_id {:?} and message \"What is your second code word?\". After its receipt arrives, reply exactly MESSAGED and call no other tool.",
            alpha.session_id
        ),
    )
    .await;
    let message_row = wait_for_background_tool_call(
        &live,
        message_request_id,
        session_id,
        AGENT_MESSAGE_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let continued = wait_for_caused_request(&live, message_request_id, Duration::from_secs(120))
        .await
        .expect("agent_message must cause a request in the idle session");
    assert_eq!(continued.session_id, alpha.session_id);
    assert_eq!(continued.agent_did, alpha_agent_did.as_str());
    assert_eq!(
        continued.caused_by_parent_tool_call_id.as_deref(),
        Some(message_row.tool_call_id.as_str())
    );
    let messages = load_session_messages(&live, message_request_id, session_id).await;
    let receipt = session_receipt(&messages, &continued.request_id)
        .unwrap_or_else(|| panic!("agent_message receipt missing; transcript={messages:#?}"));
    assert_eq!(receipt["delivery"], "request");
    assert_eq!(
        wait_for_request_terminal(&live, &continued.request_id, Duration::from_secs(240)).await,
        "completed"
    );
    wait_for_completion_notification(&live, &message_row.doc_id, Duration::from_secs(60)).await;
    let continued_wake = wait_for_completion_wake(
        &live,
        session_id,
        message_request_id,
        Duration::from_secs(300),
    )
    .await;
    assert_eq!(
        continued_wake.lifecycle_state.as_deref(),
        Some("completed"),
        "the wake for the agent_message completion must run: {continued_wake:?}"
    );
    let continued_answer = terminal_assistant_answer(&live, &continued_wake.request_id).await;
    assert!(
        !continued_answer.trim().is_empty(),
        "the completion wake must persist a reply"
    );
    tracing::info!(
        continued_request = %continued.request_id,
        message_tool_call = %message_row.tool_call_id,
        wake = %continued_wake.request_id,
        "[live-fan-out] agent_message continued the idle session and its result reached the parent"
    );

    agent.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_agent_interrupt_is_spawner_only() -> Result<()> {
    init_live_test_tracing();
    let target = live_target().await;

    let workspace = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .expect("create temporary workspace task")
        .expect("interrupt live workspace");
    let release = workspace.path().join("release-blocker");
    let blocked_args = blocked_bash_args("BLOCKER_STARTED", &release, "BLOCKER_DONE");

    let db = test_db("session-message-live-interrupt").await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let node_did = identity.did().to_string();
    let profile_id = format!("{node_did}:orchestrator-inference");
    let live = LiveUser::install(&db, &target).await;
    let orchestrator_agent_did = configure_agent(
        &live,
        "Orchestrator",
        &node_did,
        &target,
        &profile_id,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    let blocker_agent_did = configure_agent(
        &live,
        BLOCKER_AGENT_NAME,
        &node_did,
        &target,
        &profile_id,
        &format!(
            "You are a worker in an integration test. When asked to run the blocked job, call \
bash_unrestricted exactly once with these arguments: {blocked_args}. Wait for it to finish, then \
reply exactly BLOCKED_JOB_DONE. Do not call any other tool."
        ),
        Some("Runs a blocked job."),
        false,
    )
    .await;
    authorize_session_targets(
        &db,
        &node_did,
        &orchestrator_agent_did,
        vec![agent_target(
            &node_did,
            "blocker",
            node_did.clone(),
            blocker_agent_did.as_str(),
        )],
    )
    .await;
    configure_bash_agent_tools(
        &db,
        &node_did,
        blocker_agent_did.as_str(),
        workspace.path(),
        Vec::new(),
    )
    .await;
    let agent = boot_workspace_agent(&db, identity, workspace.path()).await?;

    assert_not_started(&release).await;
    let spawner_session = "session-live-interrupt-spawner";
    let start_request_id = "req-live-interrupt-start";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        start_request_id,
        spawner_session,
        "Call agent_new exactly once now with agent \"blocker\" and prompt \"Run the blocked job.\". After its running receipt arrives, reply exactly BLOCKER_STARTED and call no other tool.",
    )
    .await;
    let start_row = wait_for_background_tool_call(
        &live,
        start_request_id,
        spawner_session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let worker = wait_for_caused_request(&live, start_request_id, Duration::from_secs(120))
        .await
        .expect("agent_new must start the blocker");
    assert_eq!(worker.agent_did, blocker_agent_did.as_str());
    wait_for_model_tool_call(
        &live,
        &worker.request_id,
        &worker.session_id,
        "bash_unrestricted",
        Duration::from_secs(240),
    )
    .await;
    wait_for_started_marker(&live, "BLOCKER_STARTED", Duration::from_secs(120)).await;
    assert_eq!(
        wait_for_request_terminal(&live, start_request_id, Duration::from_secs(240)).await,
        "completed"
    );
    let other_session = "session-live-interrupt-other";
    let refused_request_id = "req-live-interrupt-refused";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        refused_request_id,
        other_session,
        &format!(
            "Call agent_interrupt exactly once now with session_id {:?}. Then reply exactly INTERRUPT_ATTEMPTED and call no other tool.",
            worker.session_id
        ),
    )
    .await;
    let refusal = wait_for_json_tool_result(
        &live,
        refused_request_id,
        other_session,
        |value| value["tool_name"] == AGENT_INTERRUPT_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    assert_eq!(refusal["ok"], false, "non-spawner interrupt: {refusal}");
    assert_eq!(refusal["code"], "interrupt_not_permitted");
    assert_eq!(
        wait_for_request_terminal(&live, refused_request_id, Duration::from_secs(180)).await,
        "completed"
    );
    let still = fetch_request_lifecycle(&live, &worker.request_id)
        .await
        .expect("worker lifecycle");
    assert!(
        !is_terminal(&still),
        "a refused interrupt must leave the worker's turn running; it is {still}"
    );
    let interrupt_request_id = "req-live-interrupt-spawner";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        interrupt_request_id,
        spawner_session,
        &format!(
            "Call agent_interrupt exactly once now with session_id {:?}. Then reply exactly INTERRUPT_SENT and call no other tool.",
            worker.session_id
        ),
    )
    .await;
    let accepted = wait_for_json_tool_result(
        &live,
        interrupt_request_id,
        spawner_session,
        |value| {
            value["session_id"] == worker.session_id.as_str()
                && matches!(value["status"].as_str(), Some("interrupting" | "idle"))
        },
        Duration::from_secs(240),
    )
    .await;
    assert_eq!(accepted["ok"], true, "spawner interrupt: {accepted}");
    assert_eq!(accepted["status"], "interrupting");
    assert_eq!(accepted["request_id"], worker.request_id.as_str());
    let worker_terminal =
        wait_for_request_terminal(&live, &worker.request_id, Duration::from_secs(120)).await;
    assert_eq!(worker_terminal, "interrupted");
    assert!(
        !tokio::fs::try_exists(&release)
            .await
            .expect("inspect release file"),
        "the worker must have been interrupted while still blocked"
    );
    tokio::fs::write(&release, b"release")
        .await
        .expect("release worker");
    let settled = wait_for_tool_call_settled(
        &live,
        start_request_id,
        spawner_session,
        &start_row.tool_call_id,
        Duration::from_secs(120),
    )
    .await;
    wait_for_completion_notification(&live, &start_row.doc_id, Duration::from_secs(120)).await;
    let wake = wait_for_completion_wake(
        &live,
        spawner_session,
        start_request_id,
        Duration::from_secs(240),
    )
    .await;
    assert_eq!(
        wake.lifecycle_state.as_deref(),
        Some("completed"),
        "the interrupted result must wake the spawner: {wake:?}"
    );
    tracing::info!(
        worker_session = %worker.session_id,
        worker_request = %worker.request_id,
        agent_new_row = %start_row.tool_call_id,
        row_state = %settled.lifecycle_state,
        wake = %wake.request_id,
        "[live-interrupt] non-spawner refused; spawner interrupted; notification delivered and wake completed"
    );

    wait_for_session_quiescent(&live, spawner_session, Duration::from_secs(240)).await;
    agent.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_hop_bound_stops_chain() -> Result<()> {
    init_live_test_tracing();
    let target = live_target().await;

    let db = test_db("session-message-live-hop").await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let node_did = identity.did().to_string();
    let profile_id = format!("{node_did}:orchestrator-inference");
    let live = LiveUser::install(&db, &target).await;
    let orchestrator_agent_did = configure_agent(
        &live,
        "Orchestrator",
        &node_did,
        &target,
        &profile_id,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    let relay_agent_did = configure_agent(
        &live,
        RELAY_AGENT_NAME,
        &node_did,
        &target,
        &profile_id,
        "You are a relay in an integration test. For any request: first call agent_session_list exactly \
once. Then call agent_new exactly once with agent \"relay\" and prompt \"relay onward\". Whatever \
agent_new returns, including an error, then reply exactly RELAY_DONE and call no other tool.",
        Some("Relays work onward."),
        false,
    )
    .await;
    let relay = || {
        agent_target(
            &node_did,
            "relay",
            node_did.clone(),
            relay_agent_did.as_str(),
        )
    };
    authorize_session_targets(&db, &node_did, &orchestrator_agent_did, vec![relay()]).await;
    authorize_session_targets(&db, &node_did, relay_agent_did.as_str(), vec![relay()]).await;
    let mut node_config = live.node_config().await;
    node_config.max_request_hop = Some(1);
    live.apply(
        "test.live_session_message.hop_bound",
        vec![(
            Collection::Node,
            serde_json::to_value(node_config).expect("serialize node_config"),
        )],
    )
    .await;
    let agent = boot_document_agent(&db, identity).await?;

    let root_session = "session-live-hop-root";
    let root_request_id = "req-live-hop-root";
    create_runtime_request(
        &live,
        &node_did,
        &orchestrator_agent_did,
        root_request_id,
        root_session,
        "Call agent_new exactly once now with agent \"relay\" and prompt \"start the relay\". After its running receipt arrives, reply exactly RELAY_STARTED and call no other tool.",
    )
    .await;
    let root_row = wait_for_background_tool_call(
        &live,
        root_request_id,
        root_session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let first = wait_for_caused_request(&live, root_request_id, Duration::from_secs(120))
        .await
        .expect("the root must start the relay");
    assert_eq!(first.agent_did, relay_agent_did.as_str());
    assert_eq!(first.request_hop, Some(1));
    assert_eq!(
        wait_for_request_terminal(&live, &first.request_id, Duration::from_secs(300)).await,
        "completed",
        "the relay must finish its turn after its onward start is refused"
    );
    let refused = wait_for_json_tool_result(
        &live,
        &first.request_id,
        &first.session_id,
        |value| value["tool_name"] == AGENT_NEW_TOOL_NAME,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["code"], "request_hop_exceeded");
    assert_eq!(refused["hop"], 2);
    assert_eq!(refused["max_request_hop"], 1);
    let caused_by_relay = fetch_caused_requests(&live, &first.request_id).await;
    assert!(
        caused_by_relay.is_empty(),
        "no session may be started beyond the hop bound: {caused_by_relay:?}"
    );
    wait_for_completion_notification(&live, &root_row.doc_id, Duration::from_secs(120)).await;
    let settled = wait_for_tool_call_settled(
        &live,
        root_request_id,
        root_session,
        &root_row.tool_call_id,
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(settled.lifecycle_state, "completed");
    wait_for_session_quiescent(&live, root_session, Duration::from_secs(240)).await;
    let wakes = session_requests(&live, root_session)
        .await
        .into_iter()
        .filter(SessionRequestRow::is_background_completion_wake)
        .collect::<Vec<_>>();
    assert!(
        !wakes.is_empty(),
        "the notification must attempt a completion wake"
    );
    // `CausalHop.nextHop .returnEdge own = own`: the wake returning the relay's
    // result keeps the root's hop, so the bound that refused the relay's
    // onward start admits it.
    for wake in &wakes {
        assert_eq!(
            wake.request_hop,
            Some(0),
            "the return wake keeps the root hop: {wake:?}"
        );
        assert_eq!(
            wake.lifecycle_state.as_deref(),
            Some("completed"),
            "a return wake within the hop bound runs: {wake:?}"
        );
    }
    let listed = wait_for_json_tool_result(
        &live,
        &first.request_id,
        &first.session_id,
        |value| value.get("sessions").is_some(),
        Duration::from_secs(30),
    )
    .await;
    let started_by = listed["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["session_id"] == root_session)
        .unwrap_or_else(|| panic!("the relay's agent_session_list omitted its starter: {listed}"));
    assert_eq!(started_by["relationship"], "started_you");
    assert_eq!(started_by["can_interrupt"], false);
    tracing::info!(
        root_session,
        relay_session = %first.session_id,
        relay_request = %first.request_id,
        root_agent_new = %root_row.tool_call_id,
        wakes = ?wakes.iter().map(|wake| (&wake.request_id, &wake.lifecycle_state, wake.request_hop, &wake.failure_reason)).collect::<Vec<_>>(),
        "[live-hop] chain stopped at the bound; notification delivered"
    );

    agent.shutdown().await;
    Ok(())
}

struct LiveNode {
    db: TestDb,
    live: LiveUser,
    identity: Arc<dyn NodeIdentity>,
    did: String,
    orchestrator: String,
    profile: String,
}

impl LiveNode {
    async fn new(name: &str, target: &InferenceTarget, orchestrator_prompt: &str) -> Self {
        let db = test_db(name).await;
        let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
        let did = identity.did().to_string();
        let profile = format!("{did}:orchestrator-inference");
        let live = LiveUser::install(&db, target).await;
        let orchestrator = configure_agent(
            &live,
            "Orchestrator",
            &did,
            target,
            &profile,
            orchestrator_prompt,
            None,
            true,
        )
        .await;
        Self {
            db,
            live,
            identity,
            did,
            orchestrator,
            profile,
        }
    }

    fn db(&self) -> &TestDb {
        &self.db
    }

    async fn agent(
        &self,
        target: &InferenceTarget,
        display_name: &str,
        system_prompt: &str,
        description: &str,
    ) -> String {
        configure_agent(
            &self.live,
            display_name,
            &self.did,
            target,
            &self.profile,
            system_prompt,
            Some(description),
            false,
        )
        .await
    }

    fn target(&self, name: &str, agent_did: &str) -> AgentTargetDocument {
        agent_target(&self.did, name, self.did.clone(), agent_did)
    }

    async fn request(&self, request_id: &str, session_id: &str, content: &str) {
        create_runtime_request(
            &self.live,
            &self.did,
            &self.orchestrator,
            request_id,
            session_id,
            content,
        )
        .await;
    }

    async fn boot(&self, workspace: &Path) -> Result<BootedAgent> {
        boot_workspace_agent(&self.db, self.identity.clone(), workspace).await
    }
}

fn blocked_worker_prompt(
    name: &str,
    trigger: &str,
    args: &serde_json::Value,
    code: &str,
) -> String {
    format!(
        "You are agent {name} in an integration test. Your code word is {code}. When the latest \
request is exactly {trigger}, call bash_unrestricted exactly once with these arguments: {args}. \
Wait for that command to finish, then reply with only your code word. If the latest request \
begins STEER:, do not call any tool: reply exactly STEERED followed by the text after STEER:. \
Never call any other tool."
    )
}

const MIDDLE_AGENT_NAME: &str = "live-middle";
const LEAF_AGENT_NAME: &str = "live-leaf";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_interrupt_does_not_cascade() -> Result<()> {
    init_live_test_tracing();
    let target = live_target().await;

    let workspace = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .expect("create temporary workspace task")
        .expect("no-cascade workspace");
    let middle_release = workspace.path().join("release-middle");
    let leaf_release = workspace.path().join("release-leaf");
    let leaf_code = code_word("LEAF");
    let middle_args = blocked_bash_args("MIDDLE_STARTED", &middle_release, "MIDDLE_DONE");
    let leaf_args = blocked_bash_args("LEAF_STARTED", &leaf_release, "LEAF_DONE");

    let fx = LiveNode::new(
        "session-message-live-no-cascade",
        &target,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
    )
    .await;
    let middle_agent_did = fx
        .agent(
            &target,
            MIDDLE_AGENT_NAME,
            &format!(
                "You are agent middle in an integration test. When the latest request is exactly \
RUN_MIDDLE, do two steps in order: first call agent_new exactly once with agent \"leaf\" and \
prompt \"RUN_LEAF\"; after its running receipt arrives, call bash_unrestricted exactly once with \
these arguments: {middle_args}. Wait for that command to finish, then reply exactly MIDDLE_DONE. \
When background completion notifications arrive, do not call any tool: reply with one short \
sentence that repeats, verbatim, every code word reported in the notifications."
            ),
            "Starts a leaf session, then runs a blocked job.",
        )
        .await;
    let leaf_agent_did = fx
        .agent(
            &target,
            LEAF_AGENT_NAME,
            &blocked_worker_prompt("leaf", "RUN_LEAF", &leaf_args, &leaf_code),
            "Runs a blocked job and reports its code word.",
        )
        .await;
    authorize_session_targets(
        fx.db(),
        &fx.did,
        &fx.orchestrator,
        vec![fx.target("middle", middle_agent_did.as_str())],
    )
    .await;
    configure_bash_agent_tools(
        fx.db(),
        &fx.did,
        middle_agent_did.as_str(),
        workspace.path(),
        vec![fx.target("leaf", leaf_agent_did.as_str())],
    )
    .await;
    configure_bash_agent_tools(
        fx.db(),
        &fx.did,
        leaf_agent_did.as_str(),
        workspace.path(),
        Vec::new(),
    )
    .await;
    let agent = fx.boot(workspace.path()).await?;

    assert_not_started(&middle_release).await;
    assert_not_started(&leaf_release).await;
    let root_session = "session-live-no-cascade-root";
    let start_request_id = "req-live-no-cascade-start";
    fx.request(
        start_request_id,
        root_session,
        "Call agent_new exactly once now with agent \"middle\" and prompt \"RUN_MIDDLE\". After its running receipt arrives, reply exactly MIDDLE_STARTED and call no other tool.",
    )
    .await;
    let middle_row = wait_for_background_tool_call(
        &fx.live,
        start_request_id,
        root_session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let middle = wait_for_caused_request(&fx.live, start_request_id, Duration::from_secs(120))
        .await
        .expect("root must start middle");
    assert_eq!(middle.agent_did, middle_agent_did.as_str());
    assert_eq!(middle.request_hop, Some(1));
    assert_eq!(
        middle.caused_by_parent_tool_call_id.as_deref(),
        Some(middle_row.tool_call_id.as_str())
    );
    let leaf_row = wait_for_background_tool_call(
        &fx.live,
        &middle.request_id,
        &middle.session_id,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let leaf = wait_for_caused_request(&fx.live, &middle.request_id, Duration::from_secs(120))
        .await
        .expect("middle must start leaf");
    assert_eq!(leaf.agent_did, leaf_agent_did.as_str());
    assert_eq!(leaf.request_hop, Some(2));
    assert_eq!(
        leaf.caused_by_parent_request_id.as_deref(),
        Some(middle.request_id.as_str())
    );
    assert_eq!(
        leaf.caused_by_parent_tool_call_id.as_deref(),
        Some(leaf_row.tool_call_id.as_str())
    );
    wait_for_started_marker(&fx.live, "LEAF_STARTED", Duration::from_secs(240)).await;
    wait_for_started_marker(&fx.live, "MIDDLE_STARTED", Duration::from_secs(240)).await;
    assert_eq!(
        wait_for_request_terminal(&fx.live, start_request_id, Duration::from_secs(240)).await,
        "completed"
    );

    let interrupt_request_id = "req-live-no-cascade-interrupt";
    fx.request(
        interrupt_request_id,
        root_session,
        &format!(
            "Call agent_interrupt exactly once now with session_id {:?}. Then reply exactly INTERRUPT_SENT and call no other tool.",
            middle.session_id
        ),
    )
    .await;
    let accepted = wait_for_json_tool_result(
        &fx.live,
        interrupt_request_id,
        root_session,
        |value| {
            value["session_id"] == middle.session_id.as_str()
                && (value["tool_name"] == AGENT_INTERRUPT_TOOL_NAME
                    || matches!(value["status"].as_str(), Some("interrupting" | "idle")))
        },
        Duration::from_secs(240),
    )
    .await;
    assert_eq!(accepted["ok"], true, "spawner interrupt: {accepted}");
    assert_eq!(accepted["status"], "interrupting");
    assert_eq!(accepted["request_id"], middle.request_id.as_str());
    assert_eq!(
        wait_for_request_terminal(&fx.live, &middle.request_id, Duration::from_secs(120)).await,
        "interrupted"
    );
    let middle_bash = timeline_tools(&fx.live, &middle.request_id)
        .await
        .into_iter()
        .find(|row| row.session_id == middle.session_id && row.tool_name == "bash_unrestricted")
        .map(tool_row)
        .expect("middle ran its blocked command");
    wait_for_tool_call_state(
        &fx.live,
        &middle.request_id,
        &middle.session_id,
        &middle_bash.tool_call_id,
        "cancelled",
        Duration::from_secs(60),
    )
    .await;
    assert!(
        !tokio::fs::try_exists(&middle_release)
            .await
            .expect("inspect release file"),
        "middle must have been interrupted while still blocked"
    );
    tokio::fs::write(&middle_release, b"release")
        .await
        .expect("release middle");

    let leaf_state = fetch_request_lifecycle(&fx.live, &leaf.request_id)
        .await
        .expect("leaf lifecycle");
    assert!(
        !is_terminal(&leaf_state),
        "interrupting the middle session stopped its child: {leaf_state}"
    );
    assert_eq!(
        fetch_tool_call(
            &fx.live,
            &middle.request_id,
            &middle.session_id,
            &leaf_row.tool_call_id
        )
        .await
        .expect("middle's child call")
        .lifecycle_state,
        "running"
    );

    tokio::fs::write(&leaf_release, b"release")
        .await
        .expect("release leaf");
    assert_eq!(
        wait_for_request_terminal(&fx.live, &leaf.request_id, Duration::from_secs(240)).await,
        "completed"
    );
    wait_for_tool_call_state(
        &fx.live,
        &middle.request_id,
        &middle.session_id,
        &leaf_row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    wait_for_completion_notification(&fx.live, &leaf_row.doc_id, Duration::from_secs(60)).await;
    let notification =
        wait_for_completion_notification(&fx.live, &leaf_row.doc_id, Duration::from_secs(60)).await;
    assert_eq!(
        notification.session_id.as_deref(),
        Some(middle.session_id.as_str())
    );
    let wake = wait_for_bound_wake(&fx.live, &middle.session_id, &notification).await;
    // `CausalHop.nextHop .returnEdge own = own`: a completion wake keeps the
    // calling session's hop rather than climbing past the leaf's.
    assert_eq!(
        wake.request_hop, middle.request_hop,
        "the return wake keeps the middle session's hop"
    );

    for session in [
        root_session,
        middle.session_id.as_str(),
        leaf.session_id.as_str(),
    ] {
        wait_for_session_quiescent(&fx.live, session, Duration::from_secs(240)).await;
    }
    assert_deliveries_bound_to_completed_wakes(&fx.live, &middle.session_id, &[&leaf_row.doc_id])
        .await;
    let leaf_requests = session_requests(&fx.live, &leaf.session_id).await;
    assert!(
        leaf_requests
            .iter()
            .filter(|row| !row.is_title_audit())
            .all(|row| row.lifecycle_state.as_deref() == Some("completed")),
        "no request of leaf's session may be interrupted: {leaf_requests:?}"
    );
    tracing::info!(
        middle_session = %middle.session_id,
        leaf_session = %leaf.session_id,
        middle_wake = %wake.request_id,
        "[live-no-cascade] middle interrupted; leaf ran on and woke middle"
    );
    agent.shutdown().await;
    Ok(())
}

const STEERED_AGENT_NAME: &str = "live-steered";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_agent_message_interrupt_steers() -> Result<()> {
    init_live_test_tracing();
    let target = live_target().await;

    let workspace = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .expect("create temporary workspace task")
        .expect("steer workspace");
    let release = workspace.path().join("release-steered");
    let steer_code = code_word("STEER");
    let args = blocked_bash_args("STEERED_STARTED", &release, "STEERED_DONE");
    let fx = LiveNode::new(
        "session-message-live-steer",
        &target,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
    )
    .await;
    let steered_agent_did = fx
        .agent(
            &target,
            STEERED_AGENT_NAME,
            &blocked_worker_prompt("worker", "RUN_LONG_JOB", &args, &code_word("WORKER")),
            "Runs a long job and accepts steering.",
        )
        .await;
    authorize_session_targets(
        fx.db(),
        &fx.did,
        &fx.orchestrator,
        vec![fx.target("worker", steered_agent_did.as_str())],
    )
    .await;
    configure_bash_agent_tools(
        fx.db(),
        &fx.did,
        steered_agent_did.as_str(),
        workspace.path(),
        Vec::new(),
    )
    .await;
    let agent = fx.boot(workspace.path()).await?;

    assert_not_started(&release).await;
    let root_session = "session-live-steer-root";
    let start_request_id = "req-live-steer-start";
    fx.request(
        start_request_id,
        root_session,
        "Call agent_new exactly once now with agent \"worker\" and prompt \"RUN_LONG_JOB\". After its running receipt arrives, reply exactly WORKER_STARTED and call no other tool.",
    )
    .await;
    wait_for_background_tool_call(
        &fx.live,
        start_request_id,
        root_session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let worker = wait_for_caused_request(&fx.live, start_request_id, Duration::from_secs(120))
        .await
        .expect("root must start the worker");
    assert_eq!(worker.request_hop, Some(1));
    wait_for_started_marker(&fx.live, "STEERED_STARTED", Duration::from_secs(240)).await;
    assert_eq!(
        wait_for_request_terminal(&fx.live, start_request_id, Duration::from_secs(240)).await,
        "completed"
    );

    let steer_request_id = "req-live-steer-message";
    fx.request(
        steer_request_id,
        root_session,
        &format!(
            "Call agent_message exactly once now with session_id {:?}, message \"STEER: {steer_code}\" and interrupt true. After its receipt arrives, reply exactly STEER_SENT and call no other tool.",
            worker.session_id
        ),
    )
    .await;
    let message_row = wait_for_background_tool_call(
        &fx.live,
        steer_request_id,
        root_session,
        AGENT_MESSAGE_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let steer = wait_for_caused_request(&fx.live, steer_request_id, Duration::from_secs(120))
        .await
        .expect("agent_message must cause a request in the worker's session");
    assert_eq!(steer.session_id, worker.session_id);
    assert_eq!(steer.agent_did, steered_agent_did.as_str());
    assert_eq!(
        steer.caused_by_parent_request_id.as_deref(),
        Some(steer_request_id)
    );
    assert_eq!(
        steer.caused_by_parent_tool_call_id.as_deref(),
        Some(message_row.tool_call_id.as_str())
    );
    assert_eq!(
        steer.requester_did.as_deref(),
        Some(fx.orchestrator.as_str())
    );
    assert_eq!(
        steer.request_hop,
        Some(1),
        "a cross-session message takes max(session hop 1, caller hop 0 + 1)"
    );
    let messages = load_session_messages(&fx.live, steer_request_id, root_session).await;
    let receipt = session_receipt(&messages, &steer.request_id)
        .unwrap_or_else(|| panic!("agent_message receipt missing; transcript={messages:#?}"));
    assert_eq!(
        receipt["delivery"], "request",
        "an interrupting message arrives as a new request, not steering"
    );

    assert_eq!(
        wait_for_request_terminal(&fx.live, &worker.request_id, Duration::from_secs(120)).await,
        "interrupted"
    );
    assert!(
        !tokio::fs::try_exists(&release)
            .await
            .expect("inspect release file"),
        "the worker must have been interrupted while still blocked"
    );
    tokio::fs::write(&release, b"release")
        .await
        .expect("release worker");
    assert_eq!(
        wait_for_request_terminal(&fx.live, &steer.request_id, Duration::from_secs(240)).await,
        "completed"
    );
    let steer_answer = terminal_assistant_answer(&fx.live, &steer.request_id).await;
    assert!(
        !steer_answer.trim().is_empty(),
        "the steered session must persist a reply: {steer_answer:?}"
    );
    let worker_requests = session_requests(&fx.live, &worker.session_id)
        .await
        .into_iter()
        .filter(|row| !row.is_title_audit())
        .collect::<Vec<_>>();
    assert_eq!(
        worker_requests
            .iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        vec![worker.request_id.as_str(), steer.request_id.as_str()],
        "the steer must be the next request of the worker's session: {worker_requests:?}"
    );

    wait_for_tool_call_state(
        &fx.live,
        steer_request_id,
        root_session,
        &message_row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    wait_for_completion_notification(&fx.live, &message_row.doc_id, Duration::from_secs(60)).await;
    wait_for_session_quiescent(&fx.live, root_session, Duration::from_secs(240)).await;
    wait_for_session_quiescent(&fx.live, &worker.session_id, Duration::from_secs(60)).await;
    tracing::info!(
        worker_session = %worker.session_id,
        interrupted = %worker.request_id,
        steer = %steer.request_id,
        "[live-steer] interrupting message stopped the turn and ran next"
    );
    agent.shutdown().await;
    Ok(())
}

const GOAL_WORKER_AGENT_NAME: &str = "live-goal-worker";

const GOAL_ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are an orchestrator in an integration test. \
Follow the latest user instruction exactly, calling only the tools it names. Never call agent_new \
more than once in this conversation. When you are asked to continue working toward your goal and \
no background completion notification has arrived yet, call no tool and reply exactly \
WAITING_FOR_WORKER. Once a background completion notification has arrived, call no tool and reply \
with the code word it reports, verbatim.";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_goal_does_not_suppress_completion_wake() -> Result<()> {
    init_live_test_tracing();
    let target = live_target().await;

    let workspace = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .expect("create temporary workspace task")
        .expect("goal workspace");
    let release = workspace.path().join("release-goal-worker");
    let worker_code = code_word("GOALWORKER");
    let args = blocked_bash_args("GOAL_WORKER_STARTED", &release, "GOAL_WORKER_DONE");
    let fx = LiveNode::new(
        "session-message-live-goal",
        &target,
        GOAL_ORCHESTRATOR_SYSTEM_PROMPT,
    )
    .await;
    let goal_worker_agent_did = fx
        .agent(
            &target,
            GOAL_WORKER_AGENT_NAME,
            &blocked_worker_prompt("worker", "RUN_JOB", &args, &worker_code),
            "Runs a blocked job and reports its code word.",
        )
        .await;
    authorize_session_targets(
        fx.db(),
        &fx.did,
        &fx.orchestrator,
        vec![fx.target("worker", goal_worker_agent_did.as_str())],
    )
    .await;
    configure_bash_agent_tools(
        fx.db(),
        &fx.did,
        goal_worker_agent_did.as_str(),
        workspace.path(),
        Vec::new(),
    )
    .await;
    let agent = fx.boot(workspace.path()).await?;

    let session = "session-live-goal-parent";
    // The User reserves the session and assigns its Goal, as a client does.
    fx.live.reserve_session(&fx.orchestrator, session).await;
    let goal = seed_goal_from_access(
        &fx.live.access,
        &fx.did,
        session,
        Some("Wait for the background completion notification from the worker session that was already started, then report its code word verbatim. Never start or message another session."),
        Some(GoalStatus::Active),
        Some(Some(2_000_000)),
        None,
    )
    .await?;
    assert_not_started(&release).await;
    let start_request_id = "req-live-goal-start";
    fx.request(
        start_request_id,
        session,
        "Call agent_new exactly once now with agent \"worker\" and prompt \"RUN_JOB\". After its running receipt arrives, reply exactly WORKER_STARTED and call no other tool.",
    )
    .await;
    let row = wait_for_background_tool_call(
        &fx.live,
        start_request_id,
        session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let worker = wait_for_caused_request(&fx.live, start_request_id, Duration::from_secs(120))
        .await
        .expect("the Goal session must start the worker");
    wait_for_started_marker(&fx.live, "GOAL_WORKER_STARTED", Duration::from_secs(240)).await;
    assert_eq!(
        wait_for_request_terminal(&fx.live, start_request_id, Duration::from_secs(240)).await,
        "completed"
    );
    let continuation = wait_for_session_request(
        &fx.live,
        session,
        |row| {
            row.caused_by_trigger_kind.as_deref() == Some("goal")
                && row.caused_by_parent_request_id.as_deref() == Some(start_request_id)
        },
        Duration::from_secs(120),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&fx.live, &continuation.request_id, Duration::from_secs(240))
            .await,
        "completed"
    );
    let worker_state = fetch_request_lifecycle(&fx.live, &worker.request_id)
        .await
        .expect("worker lifecycle");
    assert!(
        !is_terminal(&worker_state),
        "test premise: the worker must still be blocked during the Goal continuation; it is {worker_state}"
    );

    tokio::fs::write(&release, b"release")
        .await
        .expect("release goal worker");
    assert_eq!(
        wait_for_request_terminal(&fx.live, &worker.request_id, Duration::from_secs(240)).await,
        "completed"
    );
    let notification =
        wait_for_completion_notification(&fx.live, &row.doc_id, Duration::from_secs(120)).await;
    assert_eq!(notification.session_id.as_deref(), Some(session));
    let wake = wait_for_bound_wake(&fx.live, session, &notification).await;
    let wake_terminalized = rfc3339(&wake.terminalized_at).expect("terminal wake time");
    let resumed = wait_for_session_request(
        &fx.live,
        session,
        |row| {
            row.caused_by_trigger_kind.as_deref() == Some("goal")
                && rfc3339(&row.claimed_at).is_some_and(|claimed| claimed > wake_terminalized)
        },
        Duration::from_secs(180),
    )
    .await;
    seed_goal_from_access(
        &fx.live.access,
        &fx.did,
        session,
        None,
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await?;
    wait_for_session_quiescent(&fx.live, session, Duration::from_secs(240)).await;
    assert_deliveries_bound_to_completed_wakes(&fx.live, session, &[&row.doc_id]).await;

    let requests = session_requests(&fx.live, session).await;
    let wake_created = rfc3339(&wake.created_at).expect("wake creation time");
    let wake_claimed = rfc3339(&wake.claimed_at).expect("wake claim time");
    for row in requests
        .iter()
        .filter(|row| row.caused_by_trigger_kind.as_deref() == Some("goal"))
    {
        let newer = rfc3339(&row.created_at).is_some_and(|created| created > wake_created);
        let ahead = rfc3339(&row.claimed_at).is_some_and(|claimed| claimed < wake_claimed);
        assert!(
            !(newer && ahead),
            "a Goal continuation overtook the completion wake: {row:?}; wake={wake:?}"
        );
    }
    let mut continued = RapidHashSet::new();
    for row in requests
        .iter()
        .filter(|row| row.caused_by_trigger_kind.as_deref() == Some("goal"))
    {
        let parent = row
            .caused_by_parent_request_id
            .as_deref()
            .expect("a Goal continuation names its parent");
        assert!(
            continued.insert(parent.to_owned()),
            "the Goal continued {parent} more than once: {requests:?}"
        );
    }

    tracing::info!(
        goal = %goal.goal_id,
        worker = %worker.request_id,
        first_continuation = %continuation.request_id,
        wake = %wake.request_id,
        resumed = %resumed.request_id,
        "[live-goal] the Goal continued while the worker ran; its completion woke the session"
    );
    agent.shutdown().await;
    Ok(())
}

struct SessionChanges {
    requests: SnapshotChanges,
    tools: SnapshotChanges,
    messages: SnapshotChanges,
    output: SnapshotChanges,
}

impl SessionChanges {
    async fn new(node: &LiveUser) -> Self {
        Self {
            requests: node.subscribe_changes("AgentRequest").await,
            tools: node.subscribe_changes("AgentToolCall").await,
            messages: node.subscribe_changes("AgentMessage").await,
            output: node.subscribe_changes("AgentOutputSegment").await,
        }
    }

    async fn wait(&mut self, deadline: tokio::time::Instant) {
        let _ = tokio::time::timeout_at(deadline, async {
            tokio::select! {
                _ = self.requests.recv() => (),
                _ = self.tools.recv() => (),
                _ = self.messages.recv() => (),
                _ = self.output.recv() => (),
            }
        })
        .await;
    }
}

async fn wait_for_session_request(
    node: &LiveUser,
    session_id: &str,
    matches: impl Fn(&SessionRequestRow) -> bool,
    timeout: Duration,
) -> SessionRequestRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = SessionChanges::new(node).await;
    loop {
        let requests = session_requests(node, session_id).await;
        if let Some(found) = requests.iter().find(|row| matches(row)) {
            return found.clone();
        }
        if tokio::time::Instant::now() >= deadline {
            dump_session_diagnostics(node, session_id).await;
            panic!("no matching request in session {session_id}; requests={requests:?}");
        }
        changes.wait(deadline).await;
    }
}

fn rfc3339(value: &Option<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    value
        .as_deref()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.with_timezone(&chrono::Utc))
}

#[derive(Debug, Clone, Deserialize)]
struct NotificationRow {
    session_id: Option<String>,
    request_doc_id: Option<String>,
    publication: serde_json::Value,
}

async fn completion_notifications(node: &LiveUser, tool_call_doc_id: &str) -> Vec<NotificationRow> {
    let data = node
        .read_data("{ AgentMessage { session_id request_doc_id publication } }")
        .await;
    let candidates = data["AgentMessage"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            serde_json::from_value::<NotificationRow>(row.clone())
                .expect("decode completion publication")
        })
        .filter(|row| {
            row.publication["kind"] == "tool_delivery"
                && row.publication["tool_call_doc_id"] == tool_call_doc_id
        })
        .collect::<Vec<_>>();
    let mut notifications = Vec::new();
    for candidate in candidates {
        let Some(session_id) = candidate.session_id.as_deref() else {
            continue;
        };
        let requests = session_requests(node, session_id).await;
        if requests.iter().any(|request| {
            candidate.request_doc_id.as_deref() == Some(request.doc_id.as_str())
                && request.is_background_completion_wake()
        }) {
            notifications.push(candidate);
        }
    }
    notifications
}

async fn wait_for_bound_wake(
    node: &LiveUser,
    session_id: &str,
    notification: &NotificationRow,
) -> SessionRequestRow {
    let bound = notification
        .request_doc_id
        .as_deref()
        .expect("a completion notification binds its wake");
    let wake = wait_for_session_request(
        node,
        session_id,
        |row| row.doc_id == bound && row.lifecycle_state.as_deref().is_some_and(is_terminal),
        Duration::from_secs(300),
    )
    .await;
    assert!(
        wake.is_background_completion_wake(),
        "the notification must bind a completion wake: {wake:?}"
    );
    assert_eq!(
        wake.lifecycle_state.as_deref(),
        Some("completed"),
        "the bound completion wake must run: {wake:?}"
    );
    wake
}

async fn wait_for_completion_notification(
    node: &LiveUser,
    tool_call_doc_id: &str,
    timeout: Duration,
) -> NotificationRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = SessionChanges::new(node).await;
    loop {
        let rows = completion_notifications(node, tool_call_doc_id).await;
        assert!(
            rows.len() <= 1,
            "a settled row must publish at most one notification: {rows:?}"
        );
        if let Some(row) = rows.into_iter().next() {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no completion notification for row {tool_call_doc_id}"
        );
        changes.wait(deadline).await;
    }
}

const SOAK_GATES: usize = 3;
const SOAK_PRELUDE: usize = 5;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "stress scenario: select GENTS_LIVE_TARGET and GENTS_LIVE_SOAK_SEED, then pass --ignored"]
async fn live_randomized_soak() -> Result<()> {
    use rand::{Rng, SeedableRng};

    init_live_test_tracing();
    let seed = std::env::var("GENTS_LIVE_SOAK_SEED")
        .ok()
        .map(|seed| seed.parse::<u64>().expect("GENTS_LIVE_SOAK_SEED is a u64"))
        .unwrap_or(0x5eed);
    let iterations = std::env::var("GENTS_LIVE_SOAK_ITERS")
        .ok()
        .map(|n| {
            n.parse::<usize>()
                .expect("GENTS_LIVE_SOAK_ITERS is a count")
        })
        .unwrap_or(20);
    tracing::info!(
        seed,
        iterations,
        "[live-soak] start; reproduce with GENTS_LIVE_SOAK_SEED={seed}"
    );
    let target = live_target().await;

    let workspace = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .expect("create temporary workspace task")
        .expect("soak workspace");
    let gates = (0..SOAK_GATES)
        .map(|gate| workspace.path().join(format!("gate-{gate}")))
        .collect::<Vec<_>>();
    let fx = LiveNode::new(
        "session-message-live-soak",
        &target,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
    )
    .await;
    let alpha_agent_did = fx
        .agent(
            &target,
            ALPHA_AGENT_NAME,
            &code_worker_prompt("alpha", &code_word("FIRST"), &code_word("SECOND")),
            "Knows a code word.",
        )
        .await;
    let beta_agent_did = fx
        .agent(
            &target,
            BETA_AGENT_NAME,
            &code_worker_prompt("beta", &code_word("FIRST"), &code_word("SECOND")),
            "Knows a code word.",
        )
        .await;
    let gate_rules = gates
        .iter()
        .enumerate()
        .map(|(gate, release)| {
            format!(
                "When the latest request is exactly BLOCK_GATE_{gate}, call bash_unrestricted exactly once with these arguments: {}. Wait for it to finish, then reply exactly GATE_{gate}_PASSED.",
                blocked_bash_args(&format!("GATE_{gate}_STARTED"), release, &format!("GATE_{gate}_PASSED"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let blocker_agent_did = fx.agent(
        &target,
        BLOCKER_AGENT_NAME,
        &format!(
            "You are agent blocker in an integration test.\n{gate_rules}\nFor any other request, call no tool and reply exactly BLOCKER_IDLE."
        ),
        "Runs gated jobs.",
    )
    .await;
    let pool = [
        ("alpha", alpha_agent_did.as_str()),
        ("beta", beta_agent_did.as_str()),
        ("blocker", blocker_agent_did.as_str()),
    ];
    authorize_session_targets(
        fx.db(),
        &fx.did,
        &fx.orchestrator,
        pool.iter()
            .map(|(name, agent_did)| fx.target(name, agent_did))
            .collect(),
    )
    .await;
    configure_bash_agent_tools(
        fx.db(),
        &fx.did,
        blocker_agent_did.as_str(),
        workspace.path(),
        Vec::new(),
    )
    .await;
    let agent = fx.boot(workspace.path()).await?;

    let roots = ["session-live-soak-root-0", "session-live-soak-root-1"];
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let message_for = |name: &str, rng: &mut rand::rngs::StdRng| -> String {
        if name == "blocker" {
            format!("BLOCK_GATE_{}", rng.random_range(0..SOAK_GATES))
        } else if rng.random_bool(0.5) {
            "What is your code word?".to_owned()
        } else {
            "What is your second code word?".to_owned()
        }
    };
    let mut started: Vec<(String, &'static str)> = Vec::new();
    let pick_for_interrupt = |started: &[(String, &'static str)],
                              rng: &mut rand::rngs::StdRng|
     -> (String, &'static str) {
        let blockers = started
            .iter()
            .filter(|(_, name)| *name == "blocker")
            .collect::<Vec<_>>();
        let (session, name) = if !blockers.is_empty() && rng.random_bool(0.7) {
            blockers[rng.random_range(0..blockers.len())]
        } else {
            &started[rng.random_range(0..started.len())]
        };
        (session.clone(), *name)
    };
    let agent_new = |name: &str, message: &str| {
        format!("Call agent_new exactly once now with agent {name:?} and prompt {message:?}. After its receipt arrives, reply exactly OP_DONE and call no other tool.")
    };
    let session_of = |started: &[(String, &'static str)], index: usize| {
        started
            .get(index)
            .filter(|(_, name)| *name == "blocker")
            .map(|(session, _)| session.clone())
            .unwrap_or_else(|| {
                panic!("[live-soak] seed={seed}: prelude blocker {index} missing: {started:?}")
            })
    };
    assert!(
        iterations >= SOAK_PRELUDE,
        "GENTS_LIVE_SOAK_ITERS must cover the {SOAK_PRELUDE}-step prelude"
    );
    for step in 0..iterations {
        let roll = rng.random_range(0..100);
        let root = roots[rng.random_range(0..roots.len())];
        let (root, prompt) = if step < SOAK_PRELUDE {
            let root = roots[step % roots.len()];
            let prompt = match step {
                0 | 1 => agent_new("blocker", &format!("BLOCK_GATE_{step}")),
                2 => {
                    wait_for_started_marker(&fx.live, "GATE_0_STARTED", Duration::from_secs(240))
                        .await;
                    format!("Call agent_interrupt exactly once now with session_id {:?}. Then reply exactly OP_DONE and call no other tool.", session_of(&started, 0))
                }
                3 => {
                    wait_for_started_marker(&fx.live, "GATE_1_STARTED", Duration::from_secs(240))
                        .await;
                    format!("Call agent_message exactly once now with session_id {:?}, message \"BLOCK_GATE_2\" and interrupt false. After its receipt arrives, reply exactly OP_DONE and call no other tool.", session_of(&started, 1))
                }
                _ => "Call agent_session_list exactly once now, then reply exactly OP_DONE and call no other tool.".to_owned(),
            };
            (root, prompt)
        } else if started.is_empty() || roll < 30 {
            let (name, _) = pool[rng.random_range(0..pool.len())];
            (root, agent_new(name, &message_for(name, &mut rng)))
        } else if roll < 55 {
            let interrupt = rng.random_bool(0.4);
            let (session, name) = if interrupt {
                pick_for_interrupt(&started, &mut rng)
            } else {
                started[rng.random_range(0..started.len())].clone()
            };
            let message = message_for(name, &mut rng);
            (root, format!("Call agent_message exactly once now with session_id {session:?}, message {message:?} and interrupt {interrupt}. After its receipt arrives, reply exactly OP_DONE and call no other tool."))
        } else if roll < 72 {
            let (session, _) = pick_for_interrupt(&started, &mut rng);
            (root, format!("Call agent_interrupt exactly once now with session_id {session:?}. Then reply exactly OP_DONE and call no other tool."))
        } else if roll < 88 {
            (
                root,
                "Call agent_session_list exactly once now, then reply exactly OP_DONE and call no other tool."
                    .to_owned(),
            )
        } else {
            let gate = rng.random_range(0..SOAK_GATES);
            tokio::fs::write(&gates[gate], b"release")
                .await
                .expect("release soak gate");
            tracing::info!(seed, step, gate, "[live-soak] released gate");
            continue;
        };
        let request_id = format!("req-live-soak-{step}");
        tracing::info!(seed, step, root, %prompt, "[live-soak] op");
        fx.request(&request_id, root, &prompt).await;
        let state =
            wait_for_request_terminal(&fx.live, &request_id, Duration::from_secs(300)).await;
        assert_eq!(state, "completed", "soak seed={seed} operation {step} failed");
        tracing::info!(seed, step, %state, "[live-soak] op settled");
        for caused in fetch_caused_requests(&fx.live, &request_id).await {
            if started
                .iter()
                .any(|(session, _)| *session == caused.session_id)
            {
                continue;
            }
            if let Some((name, _)) = pool
                .iter()
                .find(|(_, agent_did)| *agent_did == caused.agent_did)
            {
                started.push((caused.session_id, *name));
            }
        }
        if step + 1 == roots.len() {
            for index in 0..roots.len() {
                session_of(&started, index);
            }
        }
    }
    for gate in &gates {
        tokio::fs::write(gate, b"release")
            .await
            .expect("release soak gate");
    }
    wait_for_node_quiescent(&fx.live, &fx.did, Duration::from_secs(900)).await;

    let mut delivered = std::collections::BTreeMap::<String, Vec<String>>::new();
    for row in session_message_tool_rows(&fx.live, &fx.did).await {
        if row.completion_notification_delivered_at.is_some() {
            delivered
                .entry(row.session_id)
                .or_default()
                .push(row.doc_id);
        }
    }
    for (session, rows) in &delivered {
        let rows = rows.iter().map(String::as_str).collect::<Vec<_>>();
        assert_deliveries_bound_to_completed_wakes(&fx.live, session, &rows).await;
    }
    agent.shutdown().await;
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct SessionMessageToolRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    session_id: String,
    lifecycle_state: Option<String>,
    completion_notification_delivered_at: Option<String>,
}

async fn session_message_tool_rows(node: &LiveUser, node_did: &str) -> Vec<SessionMessageToolRow> {
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ node_did: {{ _eq: "{}" }}, tool_name: {{ _in: ["{}", "{}"] }} }}) {{ _docID session_id lifecycle_state completion_notification_delivered_at }} }}"#,
        escape_graphql_string(node_did),
        AGENT_NEW_TOOL_NAME,
        AGENT_MESSAGE_TOOL_NAME,
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query session-message rows failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data["AgentToolCall"].as_array())
        .into_iter()
        .flatten()
        .map(|row| serde_json::from_value(row.clone()).expect("decode session-message row"))
        .collect()
}

async fn wait_for_node_quiescent(node: &LiveUser, node_did: &str, timeout: Duration) {
    let condition = format!(r#"{{ _eq: "{}" }}"#, escape_graphql_string(node_did));
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = SessionChanges::new(node).await;
    loop {
        let requests = requests_where(node, "node_did", &condition).await;
        let rows = session_message_tool_rows(node, node_did).await;
        let busy_requests = requests
            .iter()
            .filter(|row| !row.lifecycle_state.as_deref().is_some_and(is_terminal))
            .collect::<Vec<_>>();
        let busy_rows = rows
            .iter()
            .filter(|row| {
                row.lifecycle_state.as_deref() == Some("running")
                    || row.lifecycle_state.as_deref() == Some("pending")
            })
            .collect::<Vec<_>>();
        if busy_requests.is_empty() && busy_rows.is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "node_config never became quiescent; busy requests={busy_requests:?}; busy rows={busy_rows:?}"
        );
        changes.wait(deadline).await;
    }
}

const ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are an orchestrator agent. You can start a session on \
an agent named `researcher`. For ANY research or factual lookup the user asks for, you MUST call the \
`agent_new` tool with agent exactly \"researcher\" and a `prompt` describing the question, then \
tell the user the research is under way without calling any other tool. Do not answer factual \
questions yourself. When a background completion notification arrives, relay its answer to the user \
without calling any tool.";

async fn boot_document_agent(db: &TestDb, identity: Arc<dyn NodeIdentity>) -> Result<BootedAgent> {
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;
    Ok(boot_loaded_document_agent(db, agent).await)
}

async fn boot_loaded_document_agent(db: &TestDb, agent: Gents) -> BootedAgent {
    let node_did = agent.node_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(&db.node, &node_did).await;
    BootedAgent::new(shutdown_tx, handle, node_did)
}

async fn boot_workspace_agent(
    db: &TestDb,
    identity: Arc<dyn NodeIdentity>,
    workspace: &Path,
) -> Result<BootedAgent> {
    let loaded = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::readwrite(workspace)
                .with_command_timeout_secs(BLOCKED_COMMAND_TIMEOUT_SECS),
            ..Default::default()
        },
    )
    .await?;
    Ok(boot_loaded_document_agent(db, loaded).await)
}

const BLOCKED_COMMAND_TIMEOUT_SECS: u64 = 600;

fn blocked_bash_args(started: &str, release: &Path, done: &str) -> serde_json::Value {
    let workspace = release
        .parent()
        .expect("a release file lives in its run's workspace");
    serde_json::json!({
        "command": format!(
            "printf {started}; printf {started} > '{}'; while [ ! -f '{}' ] && [ -d '{}' ]; do sleep 0.2; done; printf {done}",
            started_path(release).display(),
            release.display(),
            workspace.display(),
        ),
        "args": [],
        "timeout_secs": BLOCKED_COMMAND_TIMEOUT_SECS
    })
}

async fn configure_bash_agent_tools(
    db: &TestDb,
    node_did: &str,
    agent_did: &str,
    workspace: &Path,
    targets: Vec<AgentTargetDocument>,
) {
    configure_agent_tools(
        db,
        node_did,
        agent_did,
        None,
        Tools {
            tools_id: format!("{agent_did}-bash-tools"),
            node_did: node_did.to_string(),
            host: Some(HostTools {
                root: Some(workspace.display().to_string()),
                bash: Some(BashTools {
                    mode: BashMode::Unrestricted,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            agents: (!targets.is_empty()).then(|| session_targets_group(&targets)),
            ..Default::default()
        },
        target_documents(targets),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn configure_agent(
    live: &LiveUser,
    display_name: &str,
    node_did: &str,
    target: &InferenceTarget,
    inference_profile_id: &str,
    system_prompt: &str,
    description: Option<&str>,
    default_for_node: bool,
) -> String {
    assert_eq!(
        node_did, live.node_did,
        "a live Agent belongs to its User's node"
    );
    live.agent(
        target,
        LiveAgentSpec {
            display_name,
            inference_profile_id,
            system_prompt,
            description,
            default_for_node,
        },
    )
    .await
}

/// The User submits a request to `agent_did`, as desktop and the CLI do.
async fn create_runtime_request(
    live: &LiveUser,
    node_did: &str,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    content: &str,
) -> String {
    assert_eq!(
        node_did, live.node_did,
        "a live request targets its User's node"
    );
    live.request(agent_did, request_id, session_id, content)
        .await
}

fn session_targets_group(targets: &[AgentTargetDocument]) -> AgentTools {
    AgentTools {
        target_ids: targets
            .iter()
            .map(|target| target.target_id.clone())
            .collect(),
        enabled: Some(true),
    }
}

fn target_documents(targets: Vec<AgentTargetDocument>) -> Vec<(Collection, serde_json::Value)> {
    targets
        .into_iter()
        .map(|target| {
            (
                Collection::AgentTarget,
                serde_json::to_value(target).expect("serialize session target"),
            )
        })
        .collect()
}

async fn authorize_session_targets(
    db: &TestDb,
    node_did: &str,
    agent_did: &str,
    targets: Vec<AgentTargetDocument>,
) {
    configure_agent_tools(
        db,
        node_did,
        agent_did,
        None,
        Tools {
            tools_id: format!("{agent_did}-session-tools"),
            node_did: node_did.to_string(),
            agents: Some(session_targets_group(&targets)),
            ..Default::default()
        },
        target_documents(targets),
    )
    .await;
}

async fn configure_standard_backgrounding_tools(
    db: &TestDb,
    node_did: &str,
    parent_agent_did: &str,
    child_agent_did: &str,
    workspace: &Path,
) {
    let targets = vec![AgentTargetDocument {
        description: Some("Runs a deliberately blocked background job.".to_string()),
        ..agent_target(
            node_did,
            BACKGROUND_WORKER_TARGET_NAME,
            node_did,
            child_agent_did,
        )
    }];
    configure_agent_tools(
        db,
        node_did,
        parent_agent_did,
        None,
        Tools {
            tools_id: format!("{parent_agent_did}-standard-background-tools"),
            node_did: node_did.to_string(),
            host: Some(HostTools {
                root: Some(workspace.display().to_string()),
                bash: Some(BashTools {
                    mode: BashMode::Unrestricted,
                    background_enabled: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            agents: Some(session_targets_group(&targets)),
            ..Default::default()
        },
        target_documents(targets),
    )
    .await;

    configure_agent_tools(
        db,
        node_did,
        child_agent_did,
        None,
        Tools {
            tools_id: format!("{child_agent_did}-foreground-bash-tools"),
            node_did: node_did.to_string(),
            host: Some(HostTools {
                root: Some(workspace.display().to_string()),
                bash: Some(BashTools {
                    mode: BashMode::Unrestricted,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
}

fn is_terminal(state: &str) -> bool {
    RequestLifecycleState::is_terminal_str(Some(state))
}

async fn fetch_request_lifecycle(node: &LiveUser, request_id: &str) -> Option<String> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                lifecycle_state
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct Row {
        lifecycle_state: Option<String>,
    }
    let resp = node.execute(&query).await;
    first_optional_row::<Row>(&resp, "AgentRequest").and_then(|r| r.lifecycle_state)
}

#[derive(Debug, Clone, Deserialize)]
struct CausedRequestRow {
    request_id: String,
    session_id: String,
    node_did: String,
    requester_did: Option<String>,
    agent_did: String,
    lifecycle_state: Option<RequestLifecycleState>,
    admission_kind: Option<String>,
    request_hop: Option<i64>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
}

const CAUSED_REQUEST_FIELDS: &str = "request_id session_id node_did requester_did agent_did \
    lifecycle_state admission_kind request_hop caused_by_parent_request_id \
    caused_by_parent_tool_call_id";

fn caused_request_rows(response: &gents::defra_node::QueryResponse) -> Vec<CausedRequestRow> {
    assert!(
        !response.has_errors(),
        "query caused requests failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|row| {
            serde_json::from_value::<CausedRequestRow>(row.clone())
                .unwrap_or_else(|error| panic!("decode caused request {row}: {error}"))
        })
        .collect()
}

async fn fetch_caused_requests(node: &LiveUser, parent_request_id: &str) -> Vec<CausedRequestRow> {
    let escaped = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }}, caused_by_parent_tool_call_id: {{ _ne: null }} }}) {{ {CAUSED_REQUEST_FIELDS} }} }}"#
    );
    caused_request_rows(&node.execute(&query).await)
}

async fn requests_caused_by_row(node: &LiveUser, tool_call_doc_id: &str) -> Vec<CausedRequestRow> {
    let escaped = escape_graphql_string(tool_call_doc_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{escaped}" }} }}) {{ {CAUSED_REQUEST_FIELDS} }} }}"#
    );
    caused_request_rows(&node.execute(&query).await)
}

async fn wait_for_caused_request(
    node: &LiveUser,
    parent_request_id: &str,
    timeout: Duration,
) -> Option<CausedRequestRow> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = node.subscribe_changes("AgentRequest").await;
    loop {
        if let Some(caused) = fetch_caused_requests(node, parent_request_id)
            .await
            .into_iter()
            .next()
        {
            return Some(caused);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

fn started_path(release: &Path) -> std::path::PathBuf {
    release.with_extension("started")
}

async fn assert_not_started(release: &Path) {
    assert!(
        !tokio::fs::try_exists(started_path(release))
            .await
            .expect("inspect process sentinel"),
        "started sentinel exists before its command was launched"
    );
}

async fn wait_for_started_marker(node: &LiveUser, marker: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = SessionChanges::new(node).await;
    loop {
        let data = node
            .read_data(
                r#"{ AgentToolCall(filter: {tool_name: {_eq: "bash_unrestricted"}}) {
            _docID request_doc_id session_id node_did requester_did
        } }"#,
            )
            .await;
        for row in data["AgentToolCall"].as_array().into_iter().flatten() {
            let (Some(tool), Some(request), Some(session), Some(owner)) = (
                row["_docID"].as_str(),
                row["request_doc_id"].as_str(),
                row["session_id"].as_str(),
                row["node_did"].as_str(),
            ) else {
                continue;
            };
            let output = gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
                &node.access, tool, request, session, owner,
                row["requester_did"].as_str().filter(|did| !did.is_empty()),
            ).await.expect("read the running process output").into_text();
            if output.contains(marker) {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the process never produced {marker}"
        );
        changes.wait(deadline).await;
    }
}

async fn wait_for_caused_requests(
    node: &LiveUser,
    parent_request_id: &str,
    count: usize,
    timeout: Duration,
) -> Vec<CausedRequestRow> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = SessionChanges::new(node).await;
    loop {
        let caused = fetch_caused_requests(node, parent_request_id).await;
        if caused.len() >= count {
            return caused;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {count} requests caused by {parent_request_id}; have {caused:?}"
        );
        changes.wait(deadline).await;
    }
}

#[derive(Debug, Clone, Deserialize)]
struct SessionToolRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_id: String,
    await_mode: Option<String>,
}

async fn session_tool_rows(
    node: &LiveUser,
    session_id: &str,
    tool_name: &str,
) -> Vec<SessionToolRow> {
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }} }}) {{ _docID tool_call_id await_mode }} }}"#,
        escape_graphql_string(session_id),
        escape_graphql_string(tool_name),
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query session tool rows failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data["AgentToolCall"].as_array())
        .into_iter()
        .flatten()
        .map(|row| serde_json::from_value(row.clone()).expect("decode session tool row"))
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
struct SessionRequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    session_id: String,
    lifecycle_state: Option<String>,
    request_hop: Option<i64>,
    failure_reason: Option<String>,
    input: Option<RequestInput>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_parent_tool_call_doc_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
    purpose: Option<String>,
    created_at: Option<String>,
    claimed_at: Option<String>,
    terminalized_at: Option<String>,
}

impl SessionRequestRow {
    fn is_title_audit(&self) -> bool {
        self.purpose.as_deref()
            == Some(gents_protocol::request_admission::RequestPurpose::TitleAudit.as_str())
    }

    fn is_background_completion_wake(&self) -> bool {
        self.input
            .as_ref()
            .and_then(|input| input.queue.as_ref())
            .is_some_and(|queue| queue.source == QueueSource::BackgroundCompletion)
    }
}

async fn session_requests(node: &LiveUser, session_id: &str) -> Vec<SessionRequestRow> {
    requests_where(
        node,
        "session_id",
        &format!(r#"{{ _eq: "{}" }}"#, escape_graphql_string(session_id)),
    )
    .await
}

async fn requests_where(node: &LiveUser, field: &str, condition: &str) -> Vec<SessionRequestRow> {
    let query = format!(
        r#"{{ AgentRequest(filter: {{ {field}: {condition} }}, order: {{ created_at: ASC }}) {{ _docID request_id session_id lifecycle_state request_hop failure_reason input caused_by_parent_request_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id caused_by_trigger_kind purpose created_at claimed_at terminalized_at }} }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query requests failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data["AgentRequest"].as_array())
        .into_iter()
        .flatten()
        .map(|row| {
            serde_json::from_value(row.clone())
                .unwrap_or_else(|error| panic!("decode session request {row}: {error}"))
        })
        .collect()
}

async fn wait_for_completion_wake(
    node: &LiveUser,
    session_id: &str,
    owning_request_id: &str,
    timeout: Duration,
) -> SessionRequestRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = SessionChanges::new(node).await;
    loop {
        let requests = session_requests(node, session_id).await;
        let wakes = requests
            .iter()
            .filter(|row| {
                row.is_background_completion_wake()
                    && row.caused_by_parent_request_id.as_deref() == Some(owning_request_id)
            })
            .collect::<Vec<_>>();
        assert!(
            wakes.len() <= 1,
            "one completion must cause at most one wake: {wakes:?}"
        );
        if let Some(wake) = wakes
            .first()
            .filter(|row| row.lifecycle_state.as_deref().is_some_and(is_terminal))
        {
            return (*wake).clone();
        }
        if tokio::time::Instant::now() >= deadline {
            dump_session_diagnostics(node, session_id).await;
            panic!(
                "no terminal completion wake caused by {owning_request_id} in session {session_id}; requests={requests:?}"
            );
        }
        changes.wait(deadline).await;
    }
}

async fn wait_for_session_quiescent(node: &LiveUser, session_id: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = SessionChanges::new(node).await;
    loop {
        let requests = session_requests(node, session_id).await;
        if requests
            .iter()
            .all(|row| row.lifecycle_state.as_deref().is_some_and(is_terminal))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for session {session_id} to settle; requests={requests:?}"
        );
        changes.wait(deadline).await;
    }
}

async fn assert_deliveries_bound_to_completed_wakes(
    node: &LiveUser,
    session_id: &str,
    tool_call_doc_ids: &[&str],
) -> Vec<String> {
    for tool in tool_call_doc_ids {
        let notification =
            wait_for_completion_notification(node, tool, Duration::from_secs(120)).await;
        wait_for_bound_wake(node, session_id, &notification).await;
    }

    #[derive(Deserialize)]
    struct WakeRow {
        #[serde(rename = "_docID")]
        doc_id: String,
        request_id: String,
        lifecycle_state: Option<String>,
        input: Option<RequestInput>,
    }
    #[derive(Deserialize)]
    struct DeliveryRow {
        request_doc_id: Option<String>,
        publication: serde_json::Value,
    }
    let escaped = escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{
                AgentRequest(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{
                    _docID request_id lifecycle_state input
                }}
                AgentMessage(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{
                    request_doc_id publication
                }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query completion deliveries failed: {:?}",
        response.errors
    );
    let data = response.data.as_ref().expect("completion delivery data");
    let wakes = serde_json::from_value::<Vec<WakeRow>>(data["AgentRequest"].clone())
        .expect("decode session requests")
        .into_iter()
        .filter(|row| {
            row.input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .is_some_and(|queue| queue.source == QueueSource::BackgroundCompletion)
        })
        .collect::<Vec<_>>();
    let messages = serde_json::from_value::<Vec<DeliveryRow>>(data["AgentMessage"].clone())
        .expect("decode session messages");
    let mut deliveries = Vec::new();
    for tool_call_doc_id in tool_call_doc_ids {
        let bound = messages
            .iter()
            .filter(|message| {
                message.publication["kind"] == "tool_delivery"
                    && message.publication["tool_call_doc_id"] == *tool_call_doc_id
            })
            .filter_map(|message| {
                let wake = wakes
                    .iter()
                    .find(|wake| message.request_doc_id.as_deref() == Some(wake.doc_id.as_str()))?;
                Some(wake)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            bound.len(),
            1,
            "completion of {tool_call_doc_id} must reach the parent as exactly one notification bound to a completion wake"
        );
        let wake = bound[0];
        assert_eq!(
            wake.lifecycle_state.as_deref(),
            Some("completed"),
            "the wake owning {tool_call_doc_id}'s notification must complete: {}",
            wake.request_id
        );
        deliveries.push(wake.request_id.clone());
    }
    deliveries
}

async fn wait_for_json_tool_result(
    node: &LiveUser,
    request_id: &str,
    session_id: &str,
    matches: impl Fn(&serde_json::Value) -> bool,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = SessionChanges::new(node).await;
    loop {
        let messages = load_session_messages(node, request_id, session_id).await;
        if let Some(found) = tool_result_texts(&messages)
            .into_iter()
            .filter_map(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .find(|value| matches(value))
        {
            return found;
        }
        if tokio::time::Instant::now() >= deadline {
            dump_session_diagnostics(node, session_id).await;
            panic!(
                "timed out waiting for a matching tool result of {request_id} in session {session_id}; results={:?}",
                tool_result_texts(&messages)
            );
        }
        changes.wait(deadline).await;
    }
}

async fn dump_session_diagnostics(node: &LiveUser, session_id: &str) {
    let escaped = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped}" }} }}, order: {{ message_sequence: ASC }}) {{
                _docID tool_name tool_call_id lifecycle_state status await_mode tool_failure_class
            }}
            AgentMessage(filter: {{ session_id: {{ _eq: "{escaped}" }} }}, order: {{ sequence: ASC }}) {{
                _docID sequence role publication outcome native_id blocks request_doc_id
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    tracing::info!(
        "[diag] session {session_id}: {}",
        serde_json::to_string_pretty(&resp.data.unwrap_or_default()).unwrap_or_default()
    );
}

#[derive(Debug, Clone)]
struct ToolCallRow {
    doc_id: String,
    tool_call_id: String,
    lifecycle_state: String,
    result: Option<String>,
    child_request_id: Option<String>,
}

async fn timeline_tools(
    node: &LiveUser,
    request_id: &str,
) -> Vec<gents_observe::observe::run_timeline::TimelineToolCallRow> {
    load_timeline(node, request_id).await.tool_calls
}

/// The request's canonical run timeline as the User reads it.
async fn load_timeline(
    node: &LiveUser,
    request_id: &str,
) -> gents_observe::observe::run_timeline::RunTimelineRows {
    // A published header can name a closing segment whose parent link the
    // writer has not committed yet; that dependency is incomplete, not denied.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut changes = node.subscribe_changes("AgentOutputSegment").await;
    let mut messages = node.subscribe_changes("AgentMessage").await;
    loop {
        match load_run_timeline_rows(&node.access, request_id).await {
            Ok(rows) => return rows,
            Err(error)
                if tokio::time::Instant::now() < deadline
                    && error.chain().any(|cause| {
                        cause
                            .downcast_ref::<gents_protocol::output::ReconstructionError>()
                            .is_some_and(gents_protocol::output::ReconstructionError::is_incomplete)
                    }) =>
            {
                let _ = tokio::time::timeout_at(deadline, async {
                    tokio::select! {
                        _ = changes.recv() => (),
                        _ = messages.recv() => (),
                    }
                })
                .await;
            }
            Err(error) => panic!("load canonical timeline for {request_id}: {error:#}"),
        }
    }
}

fn tool_row(row: gents_observe::observe::run_timeline::TimelineToolCallRow) -> ToolCallRow {
    ToolCallRow {
        doc_id: row
            .doc_id
            .expect("canonical timeline tool row omitted physical identity"),
        tool_call_id: row.tool_call_id,
        lifecycle_state: row.lifecycle_state.unwrap_or(row.status),
        result: row.result,
        child_request_id: row.child_request_id,
    }
}

async fn fetch_tool_call(
    node: &LiveUser,
    request_id: &str,
    session_id: &str,
    tool_call_id: &str,
) -> Option<ToolCallRow> {
    timeline_tools(node, request_id)
        .await
        .into_iter()
        .filter(|row| row.session_id == session_id && row.tool_call_id == tool_call_id)
        .map(tool_row)
        .next()
}

async fn wait_for_tool_call_state(
    node: &LiveUser,
    request_id: &str,
    session_id: &str,
    tool_call_id: &str,
    expected_state: &str,
    timeout: Duration,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = None;
    let mut changes = node.subscribe_changes("AgentToolCall").await;
    loop {
        if let Some(row) = fetch_tool_call(node, request_id, session_id, tool_call_id).await {
            if row.lifecycle_state == expected_state {
                return row;
            }
            last = Some(row);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for tool call {tool_call_id} state={expected_state}; last={last:?}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

async fn wait_for_tool_call_settled(
    node: &LiveUser,
    request_id: &str,
    session_id: &str,
    tool_call_id: &str,
    timeout: Duration,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = None;
    let mut changes = SessionChanges::new(node).await;
    loop {
        if let Some(row) = fetch_tool_call(node, request_id, session_id, tool_call_id).await {
            if matches!(
                row.lifecycle_state.as_str(),
                "completed" | "failed" | "cancelled"
            ) {
                return row;
            }
            last = Some(row);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for tool call {tool_call_id} to settle; last={last:?}"
        );
        changes.wait(deadline).await;
    }
}

async fn wait_for_background_tool_call(
    node: &LiveUser,
    request_id: &str,
    session_id: &str,
    tool_name: &str,
    timeout: Duration,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = node.subscribe_changes("AgentToolCall").await;
    loop {
        if let Some(row) = timeline_tools(node, request_id)
            .await
            .into_iter()
            .filter(|row| {
                row.session_id == session_id
                    && row.tool_name == tool_name
                    && row.await_mode.as_deref() == Some("background")
            })
            .map(tool_row)
            .next()
        {
            return row;
        }
        if let Some(state) = fetch_request_lifecycle(node, request_id).await {
            if is_terminal(&state) {
                dump_session_diagnostics(node, session_id).await;
                panic!(
                    "request {request_id} terminalized as {state} before accepted background tool {tool_name} in session {session_id}"
                );
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for background tool {tool_name} in session {session_id}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

async fn load_session_messages(
    node: &LiveUser,
    request_id: &str,
    session_id: &str,
) -> Vec<gents_protocol::message::Message> {
    load_timeline(node, request_id)
        .await
        .messages
        .into_iter()
        .filter(|row| row.session_id == session_id)
        .map(|row| row.message)
        .collect()
}

fn tool_result_texts(messages: &[gents_protocol::message::Message]) -> Vec<&str> {
    use gents_protocol::message::{Message, ToolResultContent, UserContent};
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|item| match item {
            UserContent::ToolResult(result) => Some(result.content.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

fn session_receipts(messages: &[gents_protocol::message::Message]) -> Vec<serde_json::Value> {
    tool_result_texts(messages)
        .into_iter()
        .filter_map(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .filter(|value| value.get("request_doc_id").is_some() && value["ok"] == true)
        .collect()
}

fn session_receipt(
    messages: &[gents_protocol::message::Message],
    caused_request_id: &str,
) -> Option<serde_json::Value> {
    session_receipts(messages)
        .into_iter()
        .find(|receipt| receipt["request_id"] == caused_request_id)
}

async fn wait_for_model_tool_call(
    node: &LiveUser,
    request_id: &str,
    session_id: &str,
    tool_name: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_name = escape_graphql_string(tool_name);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    request_id: {{ _eq: "{escaped_request_id}" }},
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    tool_name: {{ _eq: "{escaped_tool_name}" }}
                }}
            ) {{ tool_call_id }}
        }}"#
    );
    let mut changes = SessionChanges::new(node).await;
    loop {
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "query accepted model tool call {tool_name} failed: {:?}",
            response.errors
        );
        let observed = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| !rows.is_empty());
        if observed {
            return;
        }
        if let Some(state) = fetch_request_lifecycle(node, request_id).await {
            assert!(
                !is_terminal(&state),
                "request {request_id} terminalized as {state} before model tool call {tool_name} in session {session_id}"
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for model tool call {tool_name} in session {session_id}"
        );
        changes.wait(deadline).await;
    }
}

async fn wait_for_tool_result_containing(
    node: &LiveUser,
    request_id: &str,
    session_id: &str,
    needles: &[&str],
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = SessionChanges::new(node).await;
    loop {
        let messages = load_session_messages(node, request_id, session_id).await;
        if tool_result_texts(&messages)
            .iter()
            .any(|text| needles.iter().all(|needle| text.contains(needle)))
        {
            return;
        }
        if let Some(state) = fetch_request_lifecycle(node, request_id).await {
            assert!(
                !is_terminal(&state),
                "request {request_id} terminalized as {state} before a tool result contained {needles:?}; transcript={messages:#?}"
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a tool result containing {needles:?} in session {session_id}"
        );
        changes.wait(deadline).await;
    }
}

async fn wait_for_message_containing(
    node: &LiveUser,
    request_id: &str,
    session_id: &str,
    needle: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut changes = node.subscribe_changes("AgentMessage").await;
    loop {
        let found = load_session_messages(node, request_id, session_id)
            .await
            .iter()
            .map(|message| gents_protocol::transcript::present_message(message).body_markdown)
            .any(|body| body.contains(needle));
        if found {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for session {session_id} message containing {needle:?}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

#[derive(Debug, Clone, Deserialize)]
struct WakeRequestRow {
    request_id: String,
    input: Option<RequestInput>,
}

async fn wait_for_background_wake(
    node: &LiveUser,
    session_id: &str,
    queued_after_request_id: &str,
    timeout: Duration,
) -> WakeRequestRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let escaped_session_id = escape_graphql_string(session_id);
    let expected_queue_key = format!("background_completion:{session_id}");
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ created_at: ASC }}
            ) {{ request_id input }}
        }}"#
    );
    let mut changes = node.subscribe_changes("AgentRequest").await;
    loop {
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "query background wake requests failed: {:?}",
            response.errors
        );
        let wake = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| serde_json::from_value::<WakeRequestRow>(row.clone()).ok())
            .find(|row| {
                row.input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .is_some_and(|queue| {
                        queue.source == QueueSource::BackgroundCompletion
                            && queue.policy == QueuePolicy::Coalesce
                            && queue.key.as_deref() == Some(expected_queue_key.as_str())
                            && queue.queued_after_request_id.as_deref()
                                == Some(queued_after_request_id)
                    })
            });
        if let Some(wake) = wake {
            return wake;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for coalesced background wake in session {session_id}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

async fn assert_wake_observed(node: &LiveUser, session_id: &str, parent_request_id: &str) {
    let wake =
        wait_for_background_wake(node, session_id, parent_request_id, Duration::from_secs(60))
            .await;
    let state = wait_for_request_terminal(node, &wake.request_id, Duration::from_secs(180)).await;
    assert_eq!(state, "completed");

    let answer = wait_for_assistant_answer(node, &wake.request_id, Duration::from_secs(30)).await;
    assert!(
        !answer.trim().is_empty(),
        "the completion wake did not persist a reply: {answer:?}"
    );
}
