//! Live end-to-end `agent_new`/`agent_message` tests against a real
//! inference target. An orchestrator agent, driven by the live model, starts a
//! session on an allowlisted agent; the started session runs its own agent configuration
//! (live model) and its result reaches the caller only as a background
//! completion notification plus a wake on the caller's session.
//!
//! Normal test runs skip these (they are `#[ignore]`-gated AND early-return
//! unless `GENTS_LIVE_SESSION_MESSAGE=1`). Inference comes from the target
//! named by `GENTS_EVAL_TARGET`. To run locally:
//!
//! ```bash
//! GENTS_LIVE_SESSION_MESSAGE=1 GENTS_EVAL_TARGET=workstation-1 \
//!   cargo test -p gents --features live-e2e --test e2e_live live_ -- --ignored --nocapture
//! ```
//!
//! The standard-path backgrounding test has its own gate:
//!
//! ```bash
//! GENTS_LIVE_BACKGROUNDING=1 GENTS_EVAL_TARGET=workstation-1 \
//!   cargo test -p gents --features live-e2e --test e2e_live \
//!   live_standard_backgrounding_uses_real_inference -- --ignored --nocapture
//! ```
//!
//! The randomized soak additionally needs `GENTS_LIVE_SOAK=1`;
//! `GENTS_LIVE_SOAK_ITERS` (default 20) and `GENTS_LIVE_SOAK_SEED` (random
//! and logged when unset) shape the run.
//!
//! The cross-node test starts a session on another node. The
//! caused `AgentRequest` is authored on the caller's node, replicated to the
//! target by the `agent-target-caller` data-plane route, admitted there as a
//! Peer request under the target's enrollment authority, and its terminal
//! request, session, messages and output replicate back by `agent-target-host`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::Once;
use std::time::{Duration, Instant};

use anyhow::Result;
use gents::agent::p2p_reconcile::resolve_template;
use gents::agent::p2p_reconcile::templates::{
    AGENT_TARGET_CALLER_TEMPLATE, AGENT_TARGET_HOST_TEMPLATE,
};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::document_config::{
    Agent, AgentContext, AgentTools, BashTools, HostTools, InferenceProfile, InferenceSampling,
    Tools,
};
use gents::goal::{set_goal, GoalStatus};
use gents::graphql::escape_graphql_string;
use gents::run_timeline_fetch::load_run_timeline_rows;
use gents::toolset::{
    AGENT_INTERRUPT_TOOL_NAME, AGENT_LIST_TOOL_NAME, AGENT_MESSAGE_TOOL_NAME, AGENT_NEW_TOOL_NAME,
};
use gents::{
    default_agent_id_for_node, default_inference_profile_id_for_agent, ensure_node,
    AgentTargetDocument, BashMode, Collection, DocumentRuntimeOptions, Gents, NodeIdentity,
    ReasoningEffort, ToolCeiling,
};
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use crate::support::enrollment::{authorize_enrollment_peer, wait_for_peer_identity};
use crate::support::fixtures::{agent_target, configure_agent_tools, test_identity};
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::live_inference::{
    live_target, terminal_assistant_answer, wait_for_assistant_answer, wait_for_request_terminal,
    InferenceTarget,
};
use crate::support::{
    first_optional_row, snapshots::fetch_runtime_snapshot, test_db, test_p2p_db, TestDb,
};

const RESEARCHER_AGENT_ID: &str = "live-researcher";
const FAST_WORKER_AGENT_ID: &str = "live-fast-worker";
const BACKGROUND_WORKER_AGENT_ID: &str = "live-background-worker";
/// Model-facing agent names; the model never sees agent IDs.
const RESEARCHER_TARGET_NAME: &str = "researcher";
const FAST_WORKER_TARGET_NAME: &str = "fast-worker";
const BACKGROUND_WORKER_TARGET_NAME: &str = "background-worker";
const CROSS_NODE_NETWORK_ID: &str = "net-live-session-message";
const CROSS_NODE_NETWORK_NAME: &str = "Live Session Message Net";

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

fn live_enabled() -> bool {
    std::env::var("GENTS_LIVE_SESSION_MESSAGE").as_deref() == Ok("1")
}

fn backgrounding_live_enabled() -> bool {
    std::env::var("GENTS_LIVE_BACKGROUNDING").as_deref() == Ok("1")
}

fn completion_marker(tool_call_id: &str, tool_name: &str) -> String {
    format!(r#"<tool-completion tool_call_id="{tool_call_id}" tool_name="{tool_name}""#)
}

// ---------------------------------------------------------------------------
// Test 1: local agent_new (orchestrator + target on one node / one DID)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 and pass --ignored"]
async fn live_local_create_session() -> Result<()> {
    if !live_enabled() {
        tracing::info!("GENTS_LIVE_SESSION_MESSAGE is not 1; skipping live local agent_new");
        return Ok(());
    }

    let target = live_target();
    target.assert_reachable().await;

    let db = test_db("session-message-live-local").await;
    let identity: Arc<dyn NodeIdentity> = Arc::new(test_identity("session-message-live-local"));
    let node_did = identity.did().to_string();
    let orchestrator_agent_id = default_agent_id_for_node(&node_did);
    let profile_id = default_inference_profile_id_for_agent(&orchestrator_agent_id);
    upsert_live_backend(db.node.as_ref(), &node_did, &target).await;
    configure_agent(
        db.node.as_ref(),
        &orchestrator_agent_id,
        &node_did,
        &target,
        &profile_id,
        ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    configure_agent(
        db.node.as_ref(),
        RESEARCHER_AGENT_ID,
        &node_did,
        &target,
        &profile_id,
        "You answer the user's question concisely and factually in one short sentence.",
        Some("Researches factual questions and returns a concise factual answer."),
        false,
    )
    .await;
    authorize_session_targets(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        vec![AgentTargetDocument {
            description: Some("Researches factual questions.".to_string()),
            ..agent_target(
                &node_did,
                RESEARCHER_TARGET_NAME,
                node_did.clone(),
                RESEARCHER_AGENT_ID,
            )
        }],
    )
    .await;

    let agent = boot_document_agent(&db, identity).await?;

    let request_id = "req-live-local-create-session";
    let session_id = "session-live-local-create-session";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        request_id,
        session_id,
        "Use your research agent to find the capital of France, then tell me the answer.",
    )
    .await;

    wait_for_background_tool_call(
        &db.node,
        request_id,
        session_id,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(180),
    )
    .await;
    let Some(caused) =
        wait_for_caused_request(db.node.as_ref(), request_id, Duration::from_secs(120)).await
    else {
        dump_session_diagnostics(db.node.as_ref(), session_id).await;
        panic!("agent_new must cause an AgentRequest linked to the orchestrator request");
    };
    tracing::info!("[live-local] caused request = {caused:?}");
    assert_eq!(
        caused.caused_by_parent_request_id.as_deref(),
        Some(request_id)
    );
    // The model may start more than one session; follow the call this
    // caused request names.
    let row = fetch_tool_call(
        &db.node,
        request_id,
        session_id,
        caused
            .caused_by_parent_tool_call_id
            .as_deref()
            .expect("a caused request names its agent_new call"),
    )
    .await
    .expect("the caused request must name an agent_new call of the orchestrator request");
    assert_eq!(caused.agent_id, RESEARCHER_AGENT_ID);
    assert_eq!(caused.node_did, node_did);
    assert_eq!(caused.requester_did.as_deref(), Some(node_did.as_str()));
    assert_eq!(caused.request_hop, Some(1));
    assert_ne!(
        caused.session_id, session_id,
        "agent_new must start a new session"
    );

    let caused_terminal = wait_for_request_terminal(
        db.node.as_ref(),
        &caused.request_id,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(caused_terminal, "completed");
    let caused_answer = wait_for_assistant_answer(
        db.node.as_ref(),
        &caused.request_id,
        Duration::from_secs(30),
    )
    .await;
    tracing::info!("[live-local] started session answer = {caused_answer:?}");
    assert!(
        !caused_answer.trim().is_empty(),
        "the started session must produce a non-empty assistant response"
    );
    if !caused_answer.to_lowercase().contains("paris") {
        tracing::warn!("[live-local] SOFT-WARN: answer did not contain 'Paris': {caused_answer:?}");
    }

    let settled = wait_for_tool_call_state(
        &db.node,
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
    let messages = load_session_messages(&db.node, request_id, session_id).await;
    let receipt = session_receipt(&messages, &caused.request_id)
        .unwrap_or_else(|| panic!("agent_new receipt missing; transcript={messages:#?}"));
    assert_eq!(receipt["session_id"], caused.session_id.as_str());
    assert_eq!(receipt["tool_call_id"], row.tool_call_id.as_str());
    assert_eq!(receipt["await_mode"], "background");
    wait_for_message_containing(
        &db.node,
        request_id,
        session_id,
        &completion_marker(&row.tool_call_id, AGENT_NEW_TOOL_NAME),
        Duration::from_secs(60),
    )
    .await;
    let wake = wait_for_background_wake(
        db.node.as_ref(),
        session_id,
        request_id,
        Duration::from_secs(60),
    )
    .await;
    let wake_state =
        wait_for_request_terminal(db.node.as_ref(), &wake.request_id, Duration::from_secs(180))
            .await;
    assert_eq!(wake_state, "completed");

    let parent_terminal =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(60)).await;
    assert_eq!(parent_terminal, "completed");

    agent.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test 2: standard background paths with real inference
// ---------------------------------------------------------------------------

/// Exercise both background-work lanes through the production owned loop:
///
/// 1. The resolved model-facing surface contains `agent_new`,
///    `agent_message` and every spawn/list/read/wait/cancel process tool.
/// 2. Fire-and-continue: the parent request completes while the session it
///    started (or the process it spawned) is still blocked; releasing it
///    settles the row and produces the completion notification and a
///    real-inference wake.
/// 3. Managed session: the model starts a session, sees its running row with
///    `list_processes`, and steers it with `agent_message` while it is busy.
/// 4. Managed process: the model spawns a blocked process, lists it, reads
///    partial output while it runs, waits for it, and reads the terminal
///    output.
///
/// Release files make the non-blocking assertions deterministic: background
/// work cannot finish until this test has observed the parent return.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_BACKGROUNDING=1 and pass --ignored"]
async fn live_standard_backgrounding_uses_real_inference() -> Result<()> {
    if !backgrounding_live_enabled() {
        return Ok(());
    }
    init_live_test_tracing();

    let target = live_target();
    assert_model_available(&target).await;

    let workspace = tempfile::tempdir().expect("backgrounding live workspace");
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

    let parent_system_prompt = format!(
        r#"You are the deterministic orchestrator in an integration test.

Apply these rules to the LATEST request:
- If the latest request begins RUN_BACKGROUND_AGENT:, call agent_new exactly once with agent "background-worker" and prompt exactly "RUN_CHILD_BACKGROUND_JOB". As soon as the tool returns its running receipt, do not call agent_message, list_processes, read_process, wait_process, cancel_process, or any other tool. Reply exactly PARENT_RETURNED_AGENT_BACKGROUND.
- If it is exactly RUN_BACKGROUND_TOOL, call spawn_process exactly once with tool_name "bash_unrestricted" and args exactly {native_tool_args}. As soon as the tool returns its running receipt, do not call wait_process, read_process, list_processes, cancel_process, bash_unrestricted, or any other tool. Reply exactly PARENT_RETURNED_TOOL_BACKGROUND.
- If the latest request begins MANAGE_BACKGROUND_AGENT_CREATE:, obey its explicit agent_new instruction, then reply exactly AGENT_BACKGROUND_CREATED.
- If the latest request begins MANAGE_BACKGROUND_AGENT_LIST:, obey its explicit list_processes instruction, then reply exactly AGENT_BACKGROUND_LISTED.
- If the latest request begins MANAGE_BACKGROUND_AGENT_MESSAGE:, obey its explicit agent_message instruction, then reply exactly AGENT_BACKGROUND_MESSAGED.
- If the latest request begins MANAGE_BACKGROUND_TOOL_SPAWN:, obey its explicit spawn_process instruction, then reply exactly TOOL_BACKGROUND_SPAWNED.
- If the latest request begins MANAGE_BACKGROUND_TOOL_LIST:, obey its explicit list_processes instruction, then reply exactly TOOL_BACKGROUND_LISTED.
- If the latest request begins MANAGE_BACKGROUND_TOOL_READ_RUNNING:, obey its explicit read_process instruction. After inspecting output containing NATIVE_MANAGED_STARTED with exited false, reply exactly TOOL_BACKGROUND_READ_RUNNING.
- If the latest request begins MANAGE_BACKGROUND_TOOL_WAIT:, obey its explicit wait_process instruction. After it completes, reply exactly TOOL_BACKGROUND_WAITED.
- If the latest request begins MANAGE_BACKGROUND_TOOL_READ_TERMINAL:, obey its explicit read_process instruction. After inspecting NATIVE_MANAGED_STARTED and NATIVE_MANAGED_DONE, reply exactly TOOL_BACKGROUND_REPORT NATIVE_MANAGED_STARTED NATIVE_MANAGED_DONE.
- If the latest request asks you to review pending background completion notifications, never repeat agent_new, agent_message or spawn_process. Reply exactly BACKGROUND_COMPLETION_OBSERVED.

Never call bash_unrestricted directly from this agent."#
    );
    let child_system_prompt = format!(
        r#"You are the deterministic background worker in an integration test.
When the latest request is exactly RUN_CHILD_BACKGROUND_JOB, call bash_unrestricted exactly once with these arguments: {child_tool_args}
Wait for that foreground tool call to finish, then reply exactly CHILD_BACKGROUND_DONE. Do not call any other tool.
When the latest request is exactly RUN_MANAGED_CHILD_BACKGROUND_JOB, call bash_unrestricted exactly once with these arguments: {managed_child_tool_args}
Wait for that foreground tool call to finish, then reply exactly CHILD_MANAGED_STARTED CHILD_MANAGED_DONE. Do not call any other tool.
If you receive a message STEERING_NOTE, do not call any tool for it; append STEERING_ACK to your final reply."#
    );

    let db = test_db("backgrounding-live-standard-path").await;
    let identity: Arc<dyn NodeIdentity> =
        Arc::new(test_identity("backgrounding-live-standard-path"));
    let node_did = identity.did().to_string();
    let orchestrator_agent_id = default_agent_id_for_node(&node_did);
    let profile_id = default_inference_profile_id_for_agent(&orchestrator_agent_id);
    upsert_live_backend(db.node.as_ref(), &node_did, &target).await;
    configure_agent(
        db.node.as_ref(),
        &orchestrator_agent_id,
        &node_did,
        &target,
        &profile_id,
        &parent_system_prompt,
        None,
        true,
    )
    .await;
    configure_agent(
        db.node.as_ref(),
        BACKGROUND_WORKER_AGENT_ID,
        &node_did,
        &target,
        &profile_id,
        &child_system_prompt,
        Some("Runs a deliberately blocked background integration-test job."),
        false,
    )
    .await;
    configure_standard_backgrounding_tools(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        workspace.path(),
    )
    .await;

    // Runtime startup probes before resolving its runnable snapshot. This test
    // inspects the resolved surfaces before `run`, so perform the same probe
    // first rather than assuming an unobserved backend is already healthy.
    gents::backend_registry::probe_and_promote_enabled_backends(db.node.as_ref()).await;
    let loaded_agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(workspace.path()).with_command_timeout_secs(180),
            ..Default::default()
        },
    )
    .await?;
    assert_standard_backgrounding_tool_surfaces(&loaded_agent, &node_did, &orchestrator_agent_id);
    let agent = boot_loaded_document_agent(&db, loaded_agent).await;

    // Lane 1: agent_new fire-and-continue.
    let agent_request_id = "req-live-standard-background-agent";
    let agent_session_id = "session-live-standard-background-agent";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        agent_request_id,
        agent_session_id,
        "RUN_BACKGROUND_AGENT: invoke agent_new now for background-worker with prompt RUN_CHILD_BACKGROUND_JOB. Do not answer until its running receipt arrives.",
    )
    .await;

    let session_row = wait_for_background_tool_call(
        &db.node,
        agent_request_id,
        agent_session_id,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(180),
    )
    .await;
    let parent_state =
        wait_for_request_terminal(db.node.as_ref(), agent_request_id, Duration::from_secs(180))
            .await;
    assert_eq!(parent_state, "completed");
    let parent_answer =
        wait_for_assistant_answer(db.node.as_ref(), agent_request_id, Duration::from_secs(30))
            .await;
    assert!(
        parent_answer.contains("PARENT_RETURNED_AGENT_BACKGROUND"),
        "parent did not acknowledge the agent_new receipt: {parent_answer:?}"
    );
    let caused =
        wait_for_caused_request(db.node.as_ref(), agent_request_id, Duration::from_secs(60))
            .await
            .expect("agent_new must cause a request");
    assert_eq!(caused.agent_id, BACKGROUND_WORKER_AGENT_ID);
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
        fetch_runtime_snapshot(db.node.as_ref(), &node_did)
            .await
            .is_some_and(|snapshot| snapshot.process_state == "ready"),
        "runtime must remain ready while the started session runs; caused={caused:?}"
    );
    let running_row = fetch_tool_call(
        &db.node,
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
    let messages = load_session_messages(&db.node, agent_request_id, agent_session_id).await;
    let receipt = session_receipt(&messages, &caused.request_id)
        .unwrap_or_else(|| panic!("agent_new receipt missing; transcript={messages:#?}"));
    assert_eq!(receipt["status"], "running");
    assert_eq!(receipt["session_id"], caused.session_id.as_str());
    assert_no_tool_call(
        db.node.as_ref(),
        agent_session_id,
        &[
            AGENT_MESSAGE_TOOL_NAME,
            "wait_process",
            "read_process",
            "cancel_process",
        ],
    )
    .await;
    assert_min_completed_inference_calls(db.node.as_ref(), agent_request_id, 2).await;

    std::fs::write(&child_release, b"release").expect("release started session");
    let caused_terminal = wait_for_request_terminal(
        db.node.as_ref(),
        &caused.request_id,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(caused_terminal, "completed");
    assert_min_completed_inference_calls(db.node.as_ref(), &caused.request_id, 2).await;
    let caused_answer = terminal_assistant_answer(db.node.as_ref(), &caused.request_id).await;
    assert!(
        caused_answer.contains("CHILD_BACKGROUND_DONE"),
        "completed session lacks its selected canonical terminal output: {caused_answer:?}"
    );
    wait_for_tool_call_state(
        &db.node,
        agent_request_id,
        agent_session_id,
        &session_row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    wait_for_message_containing(
        &db.node,
        agent_request_id,
        agent_session_id,
        &completion_marker(&session_row.tool_call_id, AGENT_NEW_TOOL_NAME),
        Duration::from_secs(60),
    )
    .await;
    assert_wake_observed(db.node.as_ref(), agent_session_id, agent_request_id).await;

    // Lane 2: spawn_process fire-and-continue.
    let tool_request_id = "req-live-standard-background-tool";
    let tool_session_id = "session-live-standard-background-tool";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        tool_request_id,
        tool_session_id,
        "RUN_BACKGROUND_TOOL",
    )
    .await;

    let background_tool = wait_for_background_tool_call(
        &db.node,
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
    let persisted_tool_args: serde_json::Value =
        serde_json::from_str(&background_tool.args).expect("valid native background args");
    assert_eq!(
        persisted_tool_args["command"], native_tool_args["command"],
        "the live model did not invoke the deterministic long-running command"
    );

    let tool_parent_state =
        wait_for_request_terminal(db.node.as_ref(), tool_request_id, Duration::from_secs(180))
            .await;
    assert_eq!(tool_parent_state, "completed");
    let tool_parent_answer =
        wait_for_assistant_answer(db.node.as_ref(), tool_request_id, Duration::from_secs(30)).await;
    assert!(
        tool_parent_answer.contains("PARENT_RETURNED_TOOL_BACKGROUND"),
        "parent did not acknowledge the background process receipt: {tool_parent_answer:?}"
    );
    let still_running = fetch_tool_call(
        &db.node,
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
    assert_no_tool_call(
        db.node.as_ref(),
        tool_session_id,
        &["wait_process", "cancel_process"],
    )
    .await;
    assert_min_completed_inference_calls(db.node.as_ref(), tool_request_id, 2).await;

    std::fs::write(&tool_release, b"release").expect("release native background tool");
    let completed_tool = wait_for_tool_call_state(
        &db.node,
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
    wait_for_message_containing(
        &db.node,
        tool_request_id,
        tool_session_id,
        &format!(
            r#"<tool-completion tool_call_id="{}""#,
            background_tool.tool_call_id
        ),
        Duration::from_secs(60),
    )
    .await;
    assert_wake_observed(db.node.as_ref(), tool_session_id, tool_request_id).await;

    // Lane 3: a managed session. Each step is its own request so the test
    // observes the started session still blocked at every step.
    assert_not_started(&managed_child_release);
    let managed_agent_session_id = "session-live-managed-background-agent";
    let managed_create_request_id = "req-live-managed-background-agent-create";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        managed_create_request_id,
        managed_agent_session_id,
        "MANAGE_BACKGROUND_AGENT_CREATE: Call agent_new exactly once now with agent background-worker and prompt RUN_MANAGED_CHILD_BACKGROUND_JOB. Do not call any other tool.",
    )
    .await;
    let managed_row = wait_for_background_tool_call(
        &db.node,
        managed_create_request_id,
        managed_agent_session_id,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            managed_create_request_id,
            Duration::from_secs(180)
        )
        .await,
        "completed"
    );
    let managed_caused = wait_for_caused_request(
        db.node.as_ref(),
        managed_create_request_id,
        Duration::from_secs(60),
    )
    .await
    .expect("managed agent_new must cause a request");
    // The started session is busy once its shell reports it is blocking.
    wait_for_model_tool_call(
        &db.node,
        &managed_caused.request_id,
        &managed_caused.session_id,
        "bash_unrestricted",
        Duration::from_secs(180),
    )
    .await;
    wait_for_started_marker(
        &managed_child_release,
        "CHILD_MANAGED_STARTED",
        Duration::from_secs(120),
    )
    .await;

    let managed_list_request_id = "req-live-managed-background-agent-list";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        managed_list_request_id,
        managed_agent_session_id,
        "MANAGE_BACKGROUND_AGENT_LIST: Call list_processes exactly once now. Do not call any other tool.",
    )
    .await;
    wait_for_tool_result_containing(
        &db.node,
        managed_list_request_id,
        managed_agent_session_id,
        &[managed_row.tool_call_id.as_str(), AGENT_NEW_TOOL_NAME],
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            managed_list_request_id,
            Duration::from_secs(180)
        )
        .await,
        "completed"
    );

    let managed_state = fetch_request_lifecycle(db.node.as_ref(), &managed_caused.request_id)
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
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        managed_message_request_id,
        managed_agent_session_id,
        &managed_message_prompt,
    )
    .await;
    let message_row = wait_for_background_tool_call(
        &db.node,
        managed_message_request_id,
        managed_agent_session_id,
        AGENT_MESSAGE_TOOL_NAME,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            managed_message_request_id,
            Duration::from_secs(180)
        )
        .await,
        "completed"
    );
    let messages = load_session_messages(
        &db.node,
        managed_message_request_id,
        managed_agent_session_id,
    )
    .await;
    let message_receipt = session_receipts(&messages)
        .into_iter()
        .find(|receipt| receipt["tool_call_id"] == message_row.tool_call_id.as_str())
        .unwrap_or_else(|| panic!("agent_message receipt missing; transcript={messages:#?}"));
    assert_eq!(
        message_receipt["session_id"],
        managed_caused.session_id.as_str()
    );
    assert_eq!(
        message_receipt["delivery"], "steering",
        "a message to a busy started session must be delivered as steering"
    );
    assert!(
        !is_terminal(
            &fetch_request_lifecycle(db.node.as_ref(), &managed_caused.request_id)
                .await
                .expect("managed caused lifecycle")
        ),
        "agent_message must have been exercised against a live session"
    );
    assert_eq!(
        fetch_tool_call(
            &db.node,
            managed_create_request_id,
            managed_agent_session_id,
            &managed_row.tool_call_id,
        )
        .await
        .expect("managed agent_new row")
        .lifecycle_state,
        "running"
    );

    std::fs::write(&managed_child_release, b"release").expect("release managed session");
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            &managed_caused.request_id,
            Duration::from_secs(240)
        )
        .await,
        "completed"
    );
    let managed_answer =
        terminal_assistant_answer(db.node.as_ref(), &managed_caused.request_id).await;
    assert!(
        managed_answer.contains("CHILD_MANAGED_DONE"),
        "managed session lacks its terminal output: {managed_answer:?}"
    );
    if !managed_answer.contains("STEERING_ACK") {
        tracing::warn!("[live-managed] SOFT-WARN: steering was not acknowledged in the active turn: {managed_answer:?}"
        );
    }
    wait_for_tool_call_state(
        &db.node,
        managed_create_request_id,
        managed_agent_session_id,
        &managed_row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    wait_for_tool_call_settled(
        &db.node,
        managed_message_request_id,
        managed_agent_session_id,
        &message_row.tool_call_id,
        Duration::from_secs(180),
    )
    .await;
    wait_for_message_containing(
        &db.node,
        managed_create_request_id,
        managed_agent_session_id,
        &completion_marker(&managed_row.tool_call_id, AGENT_NEW_TOOL_NAME),
        Duration::from_secs(60),
    )
    .await;
    wait_for_message_containing(
        &db.node,
        managed_create_request_id,
        managed_agent_session_id,
        &completion_marker(&message_row.tool_call_id, AGENT_MESSAGE_TOOL_NAME),
        Duration::from_secs(60),
    )
    .await;
    assert_model_tool_call_count_at_least(
        &db.node,
        managed_create_request_id,
        managed_agent_session_id,
        AGENT_NEW_TOOL_NAME,
        1,
    )
    .await;
    assert_model_tool_call_count_at_least(
        &db.node,
        managed_list_request_id,
        managed_agent_session_id,
        "list_processes",
        1,
    )
    .await;
    assert_model_tool_call_count_at_least(
        &db.node,
        managed_message_request_id,
        managed_agent_session_id,
        AGENT_MESSAGE_TOOL_NAME,
        1,
    )
    .await;

    // Lane 4: a managed native background process. The release is withheld
    // until the model's read_process result contains the live STARTED marker,
    // proving it read actual output before wait_process.
    let managed_tool_spawn_prompt = format!(
        "MANAGE_BACKGROUND_TOOL_SPAWN: Call spawn_process exactly once now with tool_name bash_unrestricted and args exactly {managed_native_tool_args}. Do not call any other tool."
    );
    let managed_tool_request_id = "req-live-managed-background-tool-spawn";
    let managed_tool_session_id = "session-live-managed-background-tool";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        managed_tool_request_id,
        managed_tool_session_id,
        &managed_tool_spawn_prompt,
    )
    .await;

    let managed_tool = wait_for_background_tool_call(
        &db.node,
        managed_tool_request_id,
        managed_tool_session_id,
        "bash_unrestricted",
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            managed_tool_request_id,
            Duration::from_secs(180),
        )
        .await,
        "completed"
    );
    let managed_process_handle = managed_tool.tool_call_id.clone();

    let managed_tool_list_request_id = "req-live-managed-background-tool-list";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        managed_tool_list_request_id,
        managed_tool_session_id,
        "MANAGE_BACKGROUND_TOOL_LIST: Call list_processes exactly once now. Do not call any other tool.",
    )
    .await;
    wait_for_model_tool_call(
        &db.node,
        managed_tool_list_request_id,
        managed_tool_session_id,
        "list_processes",
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
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
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        managed_tool_read_request_id,
        managed_tool_session_id,
        &managed_tool_read_prompt,
    )
    .await;
    wait_for_model_tool_call(
        &db.node,
        managed_tool_read_request_id,
        managed_tool_session_id,
        "read_process",
        Duration::from_secs(180),
    )
    .await;
    wait_for_tool_result_containing(
        &db.node,
        managed_tool_read_request_id,
        managed_tool_session_id,
        &["NATIVE_MANAGED_STARTED"],
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
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
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        managed_tool_wait_request_id,
        managed_tool_session_id,
        &managed_tool_wait_prompt,
    )
    .await;
    // Observe the durably snapshotted wait call while it is blocked, then prove
    // the native process is still live before releasing it.
    wait_for_model_tool_call(
        &db.node,
        managed_tool_wait_request_id,
        managed_tool_session_id,
        "wait_process",
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        fetch_tool_call(
            &db.node,
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
    std::fs::write(&managed_tool_release, b"release")
        .expect("release managed native background tool");

    let managed_tool_wait_state = wait_for_request_terminal(
        db.node.as_ref(),
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
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        managed_tool_terminal_read_request_id,
        managed_tool_session_id,
        &managed_tool_terminal_read_prompt,
    )
    .await;
    wait_for_model_tool_call(
        &db.node,
        managed_tool_terminal_read_request_id,
        managed_tool_session_id,
        "read_process",
        Duration::from_secs(180),
    )
    .await;
    wait_for_tool_result_containing(
        &db.node,
        managed_tool_terminal_read_request_id,
        managed_tool_session_id,
        &["NATIVE_MANAGED_DONE"],
        Duration::from_secs(60),
    )
    .await;
    let managed_tool_state = wait_for_request_terminal(
        db.node.as_ref(),
        managed_tool_terminal_read_request_id,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(managed_tool_state, "completed");
    let managed_tool_answer = wait_for_assistant_answer(
        db.node.as_ref(),
        managed_tool_terminal_read_request_id,
        Duration::from_secs(30),
    )
    .await;
    assert!(
        managed_tool_answer
            .contains("TOOL_BACKGROUND_REPORT NATIVE_MANAGED_STARTED NATIVE_MANAGED_DONE"),
        "model did not report the inspected native background result: {managed_tool_answer:?}"
    );
    for (request_id, tool_name) in [
        (managed_tool_request_id, "spawn_process"),
        (managed_tool_list_request_id, "list_processes"),
        (managed_tool_read_request_id, "read_process"),
        (managed_tool_terminal_read_request_id, "read_process"),
        (managed_tool_wait_request_id, "wait_process"),
    ] {
        assert_model_tool_call_count_at_least(
            &db.node,
            request_id,
            managed_tool_session_id,
            tool_name,
            1,
        )
        .await;
    }

    agent.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test 3: cross-node agent_new (orchestrator on A -> agent on B)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 and pass --ignored"]
async fn live_cross_node_create_session() -> Result<()> {
    if !live_enabled() {
        tracing::info!("GENTS_LIVE_SESSION_MESSAGE is not 1; skipping live cross-node agent_new");
        return Ok(());
    }

    let target = live_target();
    target.assert_reachable().await;

    let db_a = test_p2p_db("session-message-live-a").await;
    let db_b = test_p2p_db("session-message-live-b").await;
    let identity_a: Arc<dyn NodeIdentity> = db_a.node_identity.clone();
    let identity_b: Arc<dyn NodeIdentity> = db_b.node_identity.clone();
    let did_a = identity_a.did().to_string();
    let did_b = identity_b.did().to_string();
    let orchestrator_agent_id = default_agent_id_for_node(&did_a);

    // Node B hosts the fast-worker agent owned by DID-B.
    let profile_b = default_inference_profile_id_for_agent(&default_agent_id_for_node(&did_b));
    upsert_live_backend(db_b.node.as_ref(), &did_b, &target).await;
    configure_agent(
        db_b.node.as_ref(),
        FAST_WORKER_AGENT_ID,
        &did_b,
        &target,
        &profile_b,
        "You answer the user's factual question in one short sentence. Do not call any tool.",
        Some("Answers factual questions."),
        true,
    )
    .await;

    // Node A hosts the orchestrator owned by DID-A. Its allowlist names the
    // (DID-B, fast-worker) pair; B's agent is not mirrored onto A.
    let profile_a = default_inference_profile_id_for_agent(&orchestrator_agent_id);
    upsert_live_backend(db_a.node.as_ref(), &did_a, &target).await;
    configure_agent(
        db_a.node.as_ref(),
        &orchestrator_agent_id,
        &did_a,
        &target,
        &profile_a,
        CROSS_NODE_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    authorize_session_targets(
        db_a.node.as_ref(),
        &did_a,
        &orchestrator_agent_id,
        vec![AgentTargetDocument {
            description: Some("Answers factual questions on another node.".to_string()),
            ..agent_target(
                &did_a,
                FAST_WORKER_TARGET_NAME,
                did_b.clone(),
                FAST_WORKER_AGENT_ID,
            )
        }],
    )
    .await;

    let agent_b = boot_document_agent(&db_b, identity_b.clone()).await?;
    let agent_a = boot_document_agent(&db_a, identity_a.clone()).await?;

    let (peer_a, peer_b) = pair_session_message_nodes(&db_a, &identity_a, &db_b, &identity_b).await;

    let request_id = "req-live-cross-node";
    let session_id = "session-live-cross-node";
    create_runtime_request(
        db_a.node.as_ref(),
        &did_a,
        &orchestrator_agent_id,
        request_id,
        session_id,
        "Run the remote research workflow for the capital of France.",
    )
    .await;

    let row = wait_for_background_tool_call(
        &db_a.node,
        request_id,
        session_id,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(180),
    )
    .await;
    let caused_a = wait_for_caused_request(db_a.node.as_ref(), request_id, Duration::from_secs(60))
        .await
        .expect("agent_new on A must author the caused request");
    tracing::info!("[live-cross] caused request on A = {caused_a:?}");
    assert_eq!(caused_a.node_did, did_b);
    assert_eq!(caused_a.requester_did.as_deref(), Some(did_a.as_str()));
    assert_eq!(caused_a.agent_id, FAST_WORKER_AGENT_ID);
    assert_eq!(caused_a.admission_kind.as_deref(), Some("peer"));
    assert_eq!(
        caused_a.caused_by_parent_tool_call_id.as_deref(),
        Some(row.tool_call_id.as_str())
    );

    let caused_b = wait_for_request_on_node(
        db_b.node.as_ref(),
        &caused_a.request_id,
        Duration::from_secs(120),
    )
    .await
    .unwrap_or_else(|| {
        panic!(
            "caused request {} must replicate to node B",
            caused_a.request_id
        )
    });
    tracing::info!("[live-cross] caused request on B = {caused_b:?}");
    assert_eq!(caused_b.node_did, did_b);
    assert_eq!(caused_b.requester_did.as_deref(), Some(did_a.as_str()));
    assert_eq!(caused_b.admission_kind.as_deref(), Some("peer"));
    assert_eq!(
        caused_b.caused_by_parent_request_id.as_deref(),
        Some(request_id)
    );

    let terminal_b = wait_for_request_terminal(
        db_b.node.as_ref(),
        &caused_a.request_id,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        terminal_b, "completed",
        "B must admit and run the Peer request"
    );
    let answer_b = wait_for_assistant_answer(
        db_b.node.as_ref(),
        &caused_a.request_id,
        Duration::from_secs(30),
    )
    .await;
    tracing::info!("[live-cross] answer on B = {answer_b:?}");
    assert!(
        !answer_b.trim().is_empty(),
        "the started session must produce a non-empty live response on B"
    );
    if !answer_b.to_lowercase().contains("paris") {
        tracing::warn!("[live-cross] SOFT-WARN: answer did not contain 'Paris': {answer_b:?}");
    }

    let terminal_a = wait_for_request_terminal(
        db_a.node.as_ref(),
        &caused_a.request_id,
        Duration::from_secs(120),
    )
    .await;
    assert_eq!(
        terminal_a, "completed",
        "the terminal must replicate back to A"
    );
    let answer_a = wait_for_assistant_answer(
        db_a.node.as_ref(),
        &caused_a.request_id,
        Duration::from_secs(60),
    )
    .await;
    assert!(
        !answer_a.trim().is_empty(),
        "the started session's terminal output must replicate back to A"
    );
    wait_for_tool_call_state(
        &db_a.node,
        request_id,
        session_id,
        &row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    wait_for_message_containing(
        &db_a.node,
        request_id,
        session_id,
        &completion_marker(&row.tool_call_id, AGENT_NEW_TOOL_NAME),
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(db_a.node.as_ref(), request_id, Duration::from_secs(60)).await,
        "completed"
    );

    // Restart both runtimes. Their reconcilers reconnect to the running P2P
    // nodes and re-project the same durable facts exactly once.
    agent_a.shutdown().await;
    agent_b.shutdown().await;
    let restarted_b = boot_document_agent(&db_b, identity_b).await?;
    let restarted_a = boot_document_agent(&db_a, identity_a).await?;
    wait_for_session_message_routes(&db_a, &peer_b, &db_b, &peer_a).await;
    for (node, label) in [(db_a.node.as_ref(), "A"), (db_b.node.as_ref(), "B")] {
        let caused = fetch_caused_requests(node, request_id).await;
        assert_eq!(
            caused.len(),
            1,
            "reconnect must not duplicate the caused request on {label}: {caused:?}"
        );
    }
    assert_eq!(
        fetch_tool_call(&db_a.node, request_id, session_id, &row.tool_call_id)
            .await
            .expect("agent_new row after restart")
            .lifecycle_state,
        "completed"
    );

    restarted_a.shutdown().await;
    restarted_b.shutdown().await;
    // BootedAgent only stops Gents::run; P2P belongs to the embedded node.
    db_a.node.shutdown().await;
    db_b.node.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test 4: fan-out, agent_list and agent_message continuation
// ---------------------------------------------------------------------------

const ALPHA_AGENT_ID: &str = "live-alpha";
const BETA_AGENT_ID: &str = "live-beta";
const BLOCKER_AGENT_ID: &str = "live-blocker";
const RELAY_AGENT_ID: &str = "live-relay";

/// A code word only the worker's own system prompt holds. Its presence in
/// the parent's answer proves the worker's result reached the parent.
fn code_word(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8].to_uppercase()
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

/// One parent fans out to two agents, both results return as notifications
/// and wake the parent through completed wakes. The parent then lists the
/// sessions it started and continues one of them with `agent_message`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 and pass --ignored"]
async fn live_fan_out_list_and_continue() -> Result<()> {
    if !live_enabled() {
        return Ok(());
    }
    init_live_test_tracing();
    let target = live_target();
    assert_model_available(&target).await;

    let alpha_first = code_word("ALPHA");
    let alpha_second = code_word("ALPHATWO");
    let beta_first = code_word("BETA");

    let db = test_db("session-message-live-fan-out").await;
    let identity: Arc<dyn NodeIdentity> = Arc::new(test_identity("session-message-live-fan-out"));
    let node_did = identity.did().to_string();
    let orchestrator_agent_id = default_agent_id_for_node(&node_did);
    let profile_id = default_inference_profile_id_for_agent(&orchestrator_agent_id);
    upsert_live_backend(db.node.as_ref(), &node_did, &target).await;
    configure_agent(
        db.node.as_ref(),
        &orchestrator_agent_id,
        &node_did,
        &target,
        &profile_id,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    for (agent_id, name, first, second) in [
        (ALPHA_AGENT_ID, "alpha", &alpha_first, &alpha_second),
        (BETA_AGENT_ID, "beta", &beta_first, &code_word("BETATWO")),
    ] {
        configure_agent(
            db.node.as_ref(),
            agent_id,
            &node_did,
            &target,
            &profile_id,
            &code_worker_prompt(name, first, second),
            Some("Knows a code word."),
            false,
        )
        .await;
    }
    authorize_session_targets(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        vec![
            agent_target(&node_did, "alpha", node_did.clone(), ALPHA_AGENT_ID),
            agent_target(&node_did, "beta", node_did.clone(), BETA_AGENT_ID),
        ],
    )
    .await;
    let agent = boot_document_agent(&db, identity).await?;

    let session_id = "session-live-fan-out";
    let fan_out_request_id = "req-live-fan-out";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        fan_out_request_id,
        session_id,
        "Call agent_new twice in this turn: once with agent \"alpha\" and prompt \"What is your code word?\", and once with agent \"beta\" and prompt \"What is your code word?\". After both running receipts arrive, reply exactly STARTED_BOTH and call no other tool.",
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            fan_out_request_id,
            Duration::from_secs(240)
        )
        .await,
        "completed"
    );
    let caused = wait_for_caused_requests(
        db.node.as_ref(),
        fan_out_request_id,
        2,
        Duration::from_secs(60),
    )
    .await;
    let rows = session_tool_rows(db.node.as_ref(), session_id, AGENT_NEW_TOOL_NAME).await;
    assert_eq!(
        rows.len(),
        2,
        "fan-out must start exactly two sessions, one per agent_new call: {rows:?}"
    );
    assert_eq!(
        caused.len(),
        2,
        "each agent_new call must cause exactly one request: {caused:?}"
    );
    let alpha = caused
        .iter()
        .find(|row| row.agent_id == ALPHA_AGENT_ID)
        .unwrap_or_else(|| panic!("no alpha session was started; caused={caused:?}"))
        .clone();
    let beta = caused
        .iter()
        .find(|row| row.agent_id == BETA_AGENT_ID)
        .unwrap_or_else(|| panic!("no beta session was started; caused={caused:?}"))
        .clone();
    for started in [&alpha, &beta] {
        assert_eq!(started.request_hop, Some(1));
        assert_eq!(started.admission_kind.as_deref(), Some("local-self"));
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
            wait_for_request_terminal(
                db.node.as_ref(),
                &started.request_id,
                Duration::from_secs(240)
            )
            .await,
            "completed"
        );
        wait_for_message_containing(
            &db.node,
            fan_out_request_id,
            session_id,
            &completion_marker(
                started.caused_by_parent_tool_call_id.as_deref().unwrap(),
                AGENT_NEW_TOOL_NAME,
            ),
            Duration::from_secs(60),
        )
        .await;
    }
    wait_for_session_quiescent(db.node.as_ref(), session_id, Duration::from_secs(240)).await;
    let delivered = rows
        .iter()
        .map(|row| row.doc_id.as_str())
        .collect::<Vec<_>>();
    let wakes =
        assert_deliveries_bound_to_completed_wakes(db.node.as_ref(), session_id, &delivered).await;
    tracing::info!(
        parent_session = session_id,
        alpha_session = %alpha.session_id,
        beta_session = %beta.session_id,
        wakes = ?wakes,
        "[live-fan-out] each result reached the parent through a completed wake"
    );

    // agent_list reports both started sessions and their relationship.
    let list_request_id = "req-live-fan-out-list";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        list_request_id,
        session_id,
        "Call agent_list exactly once now, then reply exactly LISTED and call no other tool.",
    )
    .await;
    let listed = wait_for_json_tool_result(
        &db.node,
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
                    "agent_list omitted started session {}: {listed}",
                    started.session_id
                )
            });
        assert_eq!(entry["relationship"], "started_by_you");
        assert_eq!(entry["node_did"], node_did.as_str());
        assert_eq!(entry["can_message"], true);
        assert_eq!(entry["can_interrupt"], true);
        assert_eq!(entry["status"], "idle");
    }
    let listed_agents = listed["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["agent"].as_str())
        .collect::<HashSet<_>>();
    assert_eq!(listed_agents, HashSet::from(["alpha", "beta"]));
    assert_eq!(
        wait_for_request_terminal(db.node.as_ref(), list_request_id, Duration::from_secs(180))
            .await,
        "completed"
    );
    wait_for_session_quiescent(db.node.as_ref(), session_id, Duration::from_secs(120)).await;

    // agent_message continues the idle alpha session with a new request.
    let message_request_id = "req-live-fan-out-message";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        message_request_id,
        session_id,
        &format!(
            "Call agent_message exactly once now with session_id {:?} and message \"What is your second code word?\". After its receipt arrives, reply exactly MESSAGED and call no other tool.",
            alpha.session_id
        ),
    )
    .await;
    let message_row = wait_for_background_tool_call(
        &db.node,
        message_request_id,
        session_id,
        AGENT_MESSAGE_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let continued = wait_for_caused_request(
        db.node.as_ref(),
        message_request_id,
        Duration::from_secs(120),
    )
    .await
    .expect("agent_message must cause a request in the idle session");
    assert_eq!(continued.session_id, alpha.session_id);
    assert_eq!(continued.agent_id, ALPHA_AGENT_ID);
    assert_eq!(
        continued.caused_by_parent_tool_call_id.as_deref(),
        Some(message_row.tool_call_id.as_str())
    );
    let messages = load_session_messages(&db.node, message_request_id, session_id).await;
    let receipt = session_receipt(&messages, &continued.request_id)
        .unwrap_or_else(|| panic!("agent_message receipt missing; transcript={messages:#?}"));
    assert_eq!(receipt["delivery"], "request");
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            &continued.request_id,
            Duration::from_secs(240)
        )
        .await,
        "completed"
    );
    wait_for_message_containing(
        &db.node,
        message_request_id,
        session_id,
        &completion_marker(&message_row.tool_call_id, AGENT_MESSAGE_TOOL_NAME),
        Duration::from_secs(60),
    )
    .await;
    let continued_wake = wait_for_completion_wake(
        db.node.as_ref(),
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
    let continued_answer =
        terminal_assistant_answer(db.node.as_ref(), &continued_wake.request_id).await;
    assert!(
        continued_answer.contains(&alpha_second),
        "the wake for the agent_message completion must use the continued session's result"
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

// ---------------------------------------------------------------------------
// Test 5: agent_interrupt is spawner-only
// ---------------------------------------------------------------------------

/// A session that did not start the busy worker is refused; the session
/// that started it interrupts its turn, and the interrupted result still
/// reaches the starting session as a notification.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 and pass --ignored"]
async fn live_agent_interrupt_is_spawner_only() -> Result<()> {
    if !live_enabled() {
        return Ok(());
    }
    init_live_test_tracing();
    let target = live_target();
    assert_model_available(&target).await;

    let workspace = tempfile::tempdir().expect("interrupt live workspace");
    let release = workspace.path().join("release-blocker");
    let blocked_args = blocked_bash_args("BLOCKER_STARTED", &release, "BLOCKER_DONE");

    let db = test_db("session-message-live-interrupt").await;
    let identity: Arc<dyn NodeIdentity> = Arc::new(test_identity("session-message-live-interrupt"));
    let node_did = identity.did().to_string();
    let orchestrator_agent_id = default_agent_id_for_node(&node_did);
    let profile_id = default_inference_profile_id_for_agent(&orchestrator_agent_id);
    upsert_live_backend(db.node.as_ref(), &node_did, &target).await;
    configure_agent(
        db.node.as_ref(),
        &orchestrator_agent_id,
        &node_did,
        &target,
        &profile_id,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    configure_agent(
        db.node.as_ref(),
        BLOCKER_AGENT_ID,
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
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        vec![agent_target(
            &node_did,
            "blocker",
            node_did.clone(),
            BLOCKER_AGENT_ID,
        )],
    )
    .await;
    configure_bash_agent_tools(
        db.node.as_ref(),
        &node_did,
        BLOCKER_AGENT_ID,
        workspace.path(),
        Vec::new(),
    )
    .await;
    let agent = boot_workspace_agent(&db, identity, workspace.path()).await?;

    assert_not_started(&release);
    let spawner_session = "session-live-interrupt-spawner";
    let start_request_id = "req-live-interrupt-start";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        start_request_id,
        spawner_session,
        "Call agent_new exactly once now with agent \"blocker\" and prompt \"Run the blocked job.\". After its running receipt arrives, reply exactly BLOCKER_STARTED and call no other tool.",
    )
    .await;
    let start_row = wait_for_background_tool_call(
        &db.node,
        start_request_id,
        spawner_session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let worker =
        wait_for_caused_request(db.node.as_ref(), start_request_id, Duration::from_secs(120))
            .await
            .expect("agent_new must start the blocker");
    assert_eq!(worker.agent_id, BLOCKER_AGENT_ID);
    wait_for_model_tool_call(
        &db.node,
        &worker.request_id,
        &worker.session_id,
        "bash_unrestricted",
        Duration::from_secs(240),
    )
    .await;
    wait_for_started_marker(&release, "BLOCKER_STARTED", Duration::from_secs(120)).await;
    assert_eq!(
        wait_for_request_terminal(db.node.as_ref(), start_request_id, Duration::from_secs(240))
            .await,
        "completed"
    );

    // Another root session of the same node did not start the worker.
    let other_session = "session-live-interrupt-other";
    let refused_request_id = "req-live-interrupt-refused";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        refused_request_id,
        other_session,
        &format!(
            "Call agent_interrupt exactly once now with session_id {:?}. Then reply exactly INTERRUPT_ATTEMPTED and call no other tool.",
            worker.session_id
        ),
    )
    .await;
    let refusal = wait_for_json_tool_result(
        &db.node,
        refused_request_id,
        other_session,
        |value| value["tool_name"] == AGENT_INTERRUPT_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    assert_eq!(refusal["ok"], false, "non-spawner interrupt: {refusal}");
    assert_eq!(refusal["code"], "interrupt_not_permitted");
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            refused_request_id,
            Duration::from_secs(180)
        )
        .await,
        "completed"
    );
    let still = fetch_request_lifecycle(db.node.as_ref(), &worker.request_id)
        .await
        .expect("worker lifecycle");
    assert!(
        !is_terminal(&still),
        "a refused interrupt must leave the worker's turn running; it is {still}"
    );

    // The starting session may interrupt.
    let interrupt_request_id = "req-live-interrupt-spawner";
    create_runtime_request(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        interrupt_request_id,
        spawner_session,
        &format!(
            "Call agent_interrupt exactly once now with session_id {:?}. Then reply exactly INTERRUPT_SENT and call no other tool.",
            worker.session_id
        ),
    )
    .await;
    let accepted = wait_for_json_tool_result(
        &db.node,
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
    let worker_terminal = wait_for_request_terminal(
        db.node.as_ref(),
        &worker.request_id,
        Duration::from_secs(120),
    )
    .await;
    assert_eq!(worker_terminal, "interrupted");
    assert!(
        !release.exists(),
        "the worker must have been interrupted while still blocked"
    );
    // The worker's session sees its cancelled command and a live model may
    // run it again; released, that retry cannot hold the session open.
    std::fs::write(&release, b"release").expect("release worker");
    let settled = wait_for_tool_call_settled(
        &db.node,
        start_request_id,
        spawner_session,
        &start_row.tool_call_id,
        Duration::from_secs(120),
    )
    .await;
    wait_for_message_containing(
        &db.node,
        start_request_id,
        spawner_session,
        &completion_marker(&start_row.tool_call_id, AGENT_NEW_TOOL_NAME),
        Duration::from_secs(120),
    )
    .await;
    let wake = wait_for_completion_wake(
        db.node.as_ref(),
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

    wait_for_session_quiescent(db.node.as_ref(), spawner_session, Duration::from_secs(240)).await;
    agent.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test 6: the hop bound stops a chain
// ---------------------------------------------------------------------------

/// With `max_request_hop` 1, the started session (hop 1) may not start
/// another; its refusal is a tool result, it still completes, and its result
/// notification still reaches the root session. The root's completion wake
/// would be hop 2, so it is refused and the chain stops there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 and pass --ignored"]
async fn live_hop_bound_stops_chain() -> Result<()> {
    if !live_enabled() {
        return Ok(());
    }
    init_live_test_tracing();
    let target = live_target();
    assert_model_available(&target).await;

    let db = test_db("session-message-live-hop").await;
    let identity: Arc<dyn NodeIdentity> = Arc::new(test_identity("session-message-live-hop"));
    let node_did = identity.did().to_string();
    let orchestrator_agent_id = default_agent_id_for_node(&node_did);
    let profile_id = default_inference_profile_id_for_agent(&orchestrator_agent_id);
    upsert_live_backend(db.node.as_ref(), &node_did, &target).await;
    configure_agent(
        db.node.as_ref(),
        &orchestrator_agent_id,
        &node_did,
        &target,
        &profile_id,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    configure_agent(
        db.node.as_ref(),
        RELAY_AGENT_ID,
        &node_did,
        &target,
        &profile_id,
        "You are a relay in an integration test. For any request: first call agent_list exactly \
once. Then call agent_new exactly once with agent \"relay\" and prompt \"relay onward\". Whatever \
agent_new returns, including an error, then reply exactly RELAY_DONE and call no other tool.",
        Some("Relays work onward."),
        false,
    )
    .await;
    let relay = || agent_target(&node_did, "relay", node_did.clone(), RELAY_AGENT_ID);
    authorize_session_targets(
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        vec![relay()],
    )
    .await;
    authorize_session_targets(db.node.as_ref(), &node_did, RELAY_AGENT_ID, vec![relay()]).await;
    let mut node_config = ensure_node(db.node.as_ref(), &node_did)
        .await
        .expect("node_config");
    node_config.max_request_hop = Some(1);
    apply_fixture_documents(
        db.node.as_ref(),
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
        db.node.as_ref(),
        &node_did,
        &orchestrator_agent_id,
        root_request_id,
        root_session,
        "Call agent_new exactly once now with agent \"relay\" and prompt \"start the relay\". After its running receipt arrives, reply exactly RELAY_STARTED and call no other tool.",
    )
    .await;
    let root_row = wait_for_background_tool_call(
        &db.node,
        root_request_id,
        root_session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let first =
        wait_for_caused_request(db.node.as_ref(), root_request_id, Duration::from_secs(120))
            .await
            .expect("the root must start the relay");
    assert_eq!(first.agent_id, RELAY_AGENT_ID);
    assert_eq!(first.request_hop, Some(1));
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            &first.request_id,
            Duration::from_secs(300)
        )
        .await,
        "completed",
        "the relay must finish its turn after its onward start is refused"
    );

    // The relay saw who started it.
    let listed = wait_for_json_tool_result(
        &db.node,
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
        .unwrap_or_else(|| panic!("the relay's agent_list omitted its starter: {listed}"));
    assert_eq!(started_by["relationship"], "started_you");
    assert_eq!(started_by["can_interrupt"], false);

    // Its onward start was refused at the bound; nothing was caused.
    let refused = wait_for_json_tool_result(
        &db.node,
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
    let caused_by_relay = fetch_caused_requests(db.node.as_ref(), &first.request_id).await;
    assert!(
        caused_by_relay.is_empty(),
        "no session may be started beyond the hop bound: {caused_by_relay:?}"
    );

    // The relay's result still reaches the root as a notification.
    wait_for_message_containing(
        &db.node,
        root_request_id,
        root_session,
        &completion_marker(&root_row.tool_call_id, AGENT_NEW_TOOL_NAME),
        Duration::from_secs(120),
    )
    .await;
    let settled = wait_for_tool_call_settled(
        &db.node,
        root_request_id,
        root_session,
        &root_row.tool_call_id,
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(settled.lifecycle_state, "completed");
    wait_for_session_quiescent(db.node.as_ref(), root_session, Duration::from_secs(240)).await;
    let wakes = session_requests(db.node.as_ref(), root_session)
        .await
        .into_iter()
        .filter(SessionRequestRow::is_background_completion_wake)
        .collect::<Vec<_>>();
    assert!(
        !wakes.is_empty(),
        "the notification must attempt a completion wake"
    );
    for wake in &wakes {
        assert_eq!(
            wake.lifecycle_state.as_deref(),
            Some("failed"),
            "a completion wake beyond the hop bound must not run: {wake:?}"
        );
        assert!(
            wake.failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("max_request_hop")),
            "the over-bound wake must be refused by the hop bound: {wake:?}"
        );
    }
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

// ---------------------------------------------------------------------------
// Shared fixture for the delegation-semantics tests below
// ---------------------------------------------------------------------------

/// One node whose default agent is an orchestrator, every
/// agent backed by the live target.
struct LiveNode {
    db: TestDb,
    identity: Arc<dyn NodeIdentity>,
    did: String,
    orchestrator: String,
    profile: String,
}

impl LiveNode {
    async fn new(name: &str, target: &InferenceTarget, orchestrator_prompt: &str) -> Self {
        let db = test_db(name).await;
        let identity: Arc<dyn NodeIdentity> = Arc::new(test_identity(name));
        let did = identity.did().to_string();
        let orchestrator = default_agent_id_for_node(&did);
        let profile = default_inference_profile_id_for_agent(&orchestrator);
        upsert_live_backend(db.node.as_ref(), &did, target).await;
        configure_agent(
            db.node.as_ref(),
            &orchestrator,
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
            identity,
            did,
            orchestrator,
            profile,
        }
    }

    fn node(&self) -> &EmbeddedNode {
        self.db.node.as_ref()
    }

    async fn agent(
        &self,
        target: &InferenceTarget,
        agent_id: &str,
        system_prompt: &str,
        description: &str,
    ) {
        configure_agent(
            self.node(),
            agent_id,
            &self.did,
            target,
            &self.profile,
            system_prompt,
            Some(description),
            false,
        )
        .await;
    }

    fn target(&self, name: &str, agent_id: &str) -> AgentTargetDocument {
        agent_target(&self.did, name, self.did.clone(), agent_id)
    }

    async fn request(&self, request_id: &str, session_id: &str, content: &str) {
        create_runtime_request(
            self.node(),
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

// ---------------------------------------------------------------------------
// Test 7: interrupting a session does not cascade to the session it started
// ---------------------------------------------------------------------------

const MIDDLE_AGENT_ID: &str = "live-middle";
const LEAF_AGENT_ID: &str = "live-leaf";

/// Root starts middle, middle starts leaf and then blocks. Root interrupts
/// middle: only middle's turn stops. Leaf keeps running, and once released its
/// result reaches middle's session as a notification and a wake.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 and pass --ignored"]
async fn live_interrupt_does_not_cascade() -> Result<()> {
    if !live_enabled() {
        return Ok(());
    }
    init_live_test_tracing();
    let target = live_target();
    assert_model_available(&target).await;

    let workspace = tempfile::tempdir().expect("no-cascade workspace");
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
    fx.agent(
        &target,
        MIDDLE_AGENT_ID,
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
    fx.agent(
        &target,
        LEAF_AGENT_ID,
        &blocked_worker_prompt("leaf", "RUN_LEAF", &leaf_args, &leaf_code),
        "Runs a blocked job and reports its code word.",
    )
    .await;
    authorize_session_targets(
        fx.node(),
        &fx.did,
        &fx.orchestrator,
        vec![fx.target("middle", MIDDLE_AGENT_ID)],
    )
    .await;
    configure_bash_agent_tools(
        fx.node(),
        &fx.did,
        MIDDLE_AGENT_ID,
        workspace.path(),
        vec![fx.target("leaf", LEAF_AGENT_ID)],
    )
    .await;
    configure_bash_agent_tools(
        fx.node(),
        &fx.did,
        LEAF_AGENT_ID,
        workspace.path(),
        Vec::new(),
    )
    .await;
    let agent = fx.boot(workspace.path()).await?;

    assert_not_started(&middle_release);
    assert_not_started(&leaf_release);
    let root_session = "session-live-no-cascade-root";
    let start_request_id = "req-live-no-cascade-start";
    fx.request(
        start_request_id,
        root_session,
        "Call agent_new exactly once now with agent \"middle\" and prompt \"RUN_MIDDLE\". After its running receipt arrives, reply exactly MIDDLE_STARTED and call no other tool.",
    )
    .await;
    let middle_row = wait_for_background_tool_call(
        &fx.db.node,
        start_request_id,
        root_session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let middle = wait_for_caused_request(fx.node(), start_request_id, Duration::from_secs(120))
        .await
        .expect("root must start middle");
    assert_eq!(middle.agent_id, MIDDLE_AGENT_ID);
    assert_eq!(middle.request_hop, Some(1));
    assert_eq!(
        middle.caused_by_parent_tool_call_id.as_deref(),
        Some(middle_row.tool_call_id.as_str())
    );
    let leaf_row = wait_for_background_tool_call(
        &fx.db.node,
        &middle.request_id,
        &middle.session_id,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let leaf = wait_for_caused_request(fx.node(), &middle.request_id, Duration::from_secs(120))
        .await
        .expect("middle must start leaf");
    assert_eq!(leaf.agent_id, LEAF_AGENT_ID);
    assert_eq!(leaf.request_hop, Some(2));
    assert_eq!(
        leaf.caused_by_parent_request_id.as_deref(),
        Some(middle.request_id.as_str())
    );
    assert_eq!(
        leaf.caused_by_parent_tool_call_id.as_deref(),
        Some(leaf_row.tool_call_id.as_str())
    );
    wait_for_started_marker(&leaf_release, "LEAF_STARTED", Duration::from_secs(240)).await;
    wait_for_started_marker(&middle_release, "MIDDLE_STARTED", Duration::from_secs(240)).await;
    assert_eq!(
        wait_for_request_terminal(fx.node(), start_request_id, Duration::from_secs(240)).await,
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
        &fx.db.node,
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
        wait_for_request_terminal(fx.node(), &middle.request_id, Duration::from_secs(120)).await,
        "interrupted"
    );
    let middle_bash = timeline_tools(&fx.db.node, &middle.request_id)
        .await
        .into_iter()
        .find(|row| row.session_id == middle.session_id && row.tool_name == "bash_unrestricted")
        .map(tool_row)
        .expect("middle ran its blocked command");
    wait_for_tool_call_state(
        &fx.db.node,
        &middle.request_id,
        &middle.session_id,
        &middle_bash.tool_call_id,
        "cancelled",
        Duration::from_secs(60),
    )
    .await;
    assert!(
        !middle_release.exists(),
        "middle must have been interrupted while still blocked"
    );
    // Middle's wake sees its cancelled command in the transcript and a live
    // model may run it again; released, that retry cannot hold the wake open.
    std::fs::write(&middle_release, b"release").expect("release middle");

    // No cascade: the session middle started keeps running, and middle's
    // background row for it outlives middle's interrupted turn.
    let hold_until = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let leaf_state = fetch_request_lifecycle(fx.node(), &leaf.request_id)
            .await
            .expect("leaf lifecycle");
        assert!(
            !is_terminal(&leaf_state),
            "interrupting middle must not stop the session it started; leaf is {leaf_state}"
        );
        assert_eq!(
            fetch_tool_call(
                &fx.db.node,
                &middle.request_id,
                &middle.session_id,
                &leaf_row.tool_call_id
            )
            .await
            .expect("middle's agent_new row")
            .lifecycle_state,
            "running",
            "middle's agent_new row must outlive its interrupted turn"
        );
        if tokio::time::Instant::now() >= hold_until {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    std::fs::write(&leaf_release, b"release").expect("release leaf");
    assert_eq!(
        wait_for_request_terminal(fx.node(), &leaf.request_id, Duration::from_secs(240)).await,
        "completed"
    );
    wait_for_tool_call_state(
        &fx.db.node,
        &middle.request_id,
        &middle.session_id,
        &leaf_row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    wait_for_message_containing(
        &fx.db.node,
        &middle.request_id,
        &middle.session_id,
        &completion_marker(&leaf_row.tool_call_id, AGENT_NEW_TOOL_NAME),
        Duration::from_secs(60),
    )
    .await;
    let notification =
        wait_for_completion_notification(fx.node(), &leaf_row.doc_id, Duration::from_secs(60))
            .await;
    assert_eq!(
        notification.session_id.as_deref(),
        Some(middle.session_id.as_str())
    );
    let wake = wait_for_bound_wake(fx.node(), &middle.session_id, &notification).await;
    assert_eq!(
        wake.request_hop,
        Some(3),
        "the wake climbs past leaf's hop: max(1, 2 + 1)"
    );
    let wake_answer = terminal_assistant_answer(fx.node(), &wake.request_id).await;
    if !wake_answer.contains(&leaf_code) {
        tracing::warn!("[live-no-cascade] SOFT-WARN: middle's wake did not repeat {leaf_code}: {wake_answer:?}");
    }

    for session in [
        root_session,
        middle.session_id.as_str(),
        leaf.session_id.as_str(),
    ] {
        wait_for_session_quiescent(fx.node(), session, Duration::from_secs(240)).await;
    }
    assert_deliveries_bound_to_completed_wakes(fx.node(), &middle.session_id, &[&leaf_row.doc_id])
        .await;
    let leaf_requests = session_requests(fx.node(), &leaf.session_id).await;
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

// ---------------------------------------------------------------------------
// Test 8: agent_message with interrupt is a true steer
// ---------------------------------------------------------------------------

const STEERED_AGENT_ID: &str = "live-steered";

/// `agent_message` with `interrupt` stops the busy session's turn and its
/// message runs next in that session as a new request caused by the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 and pass --ignored"]
async fn live_agent_message_interrupt_steers() -> Result<()> {
    if !live_enabled() {
        return Ok(());
    }
    init_live_test_tracing();
    let target = live_target();
    assert_model_available(&target).await;

    let workspace = tempfile::tempdir().expect("steer workspace");
    let release = workspace.path().join("release-steered");
    let steer_code = code_word("STEER");
    let args = blocked_bash_args("STEERED_STARTED", &release, "STEERED_DONE");
    let fx = LiveNode::new(
        "session-message-live-steer",
        &target,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
    )
    .await;
    fx.agent(
        &target,
        STEERED_AGENT_ID,
        &blocked_worker_prompt("worker", "RUN_LONG_JOB", &args, &code_word("WORKER")),
        "Runs a long job and accepts steering.",
    )
    .await;
    authorize_session_targets(
        fx.node(),
        &fx.did,
        &fx.orchestrator,
        vec![fx.target("worker", STEERED_AGENT_ID)],
    )
    .await;
    configure_bash_agent_tools(
        fx.node(),
        &fx.did,
        STEERED_AGENT_ID,
        workspace.path(),
        Vec::new(),
    )
    .await;
    let agent = fx.boot(workspace.path()).await?;

    assert_not_started(&release);
    let root_session = "session-live-steer-root";
    let start_request_id = "req-live-steer-start";
    fx.request(
        start_request_id,
        root_session,
        "Call agent_new exactly once now with agent \"worker\" and prompt \"RUN_LONG_JOB\". After its running receipt arrives, reply exactly WORKER_STARTED and call no other tool.",
    )
    .await;
    wait_for_background_tool_call(
        &fx.db.node,
        start_request_id,
        root_session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let worker = wait_for_caused_request(fx.node(), start_request_id, Duration::from_secs(120))
        .await
        .expect("root must start the worker");
    assert_eq!(worker.request_hop, Some(1));
    wait_for_started_marker(&release, "STEERED_STARTED", Duration::from_secs(240)).await;
    assert_eq!(
        wait_for_request_terminal(fx.node(), start_request_id, Duration::from_secs(240)).await,
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
        &fx.db.node,
        steer_request_id,
        root_session,
        AGENT_MESSAGE_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let message_args: serde_json::Value =
        serde_json::from_str(&message_row.args).expect("agent_message arguments");
    assert_eq!(
        message_args["interrupt"], true,
        "test premise: the live model must pass interrupt true: {message_args}"
    );
    let steer = wait_for_caused_request(fx.node(), steer_request_id, Duration::from_secs(120))
        .await
        .expect("agent_message must cause a request in the worker's session");
    assert_eq!(steer.session_id, worker.session_id);
    assert_eq!(steer.agent_id, STEERED_AGENT_ID);
    assert_eq!(
        steer.caused_by_parent_request_id.as_deref(),
        Some(steer_request_id)
    );
    assert_eq!(
        steer.caused_by_parent_tool_call_id.as_deref(),
        Some(message_row.tool_call_id.as_str())
    );
    assert_eq!(steer.requester_did.as_deref(), Some(fx.did.as_str()));
    assert_eq!(
        steer.request_hop,
        Some(1),
        "a cross-session message takes max(session hop 1, caller hop 0 + 1)"
    );
    let messages = load_session_messages(&fx.db.node, steer_request_id, root_session).await;
    let receipt = session_receipt(&messages, &steer.request_id)
        .unwrap_or_else(|| panic!("agent_message receipt missing; transcript={messages:#?}"));
    assert_eq!(
        receipt["delivery"], "request",
        "an interrupting message arrives as a new request, not steering"
    );

    assert_eq!(
        wait_for_request_terminal(fx.node(), &worker.request_id, Duration::from_secs(120)).await,
        "interrupted"
    );
    assert!(
        !release.exists(),
        "the worker must have been interrupted while still blocked"
    );
    // The steer runs in the worker's session, which holds the cancelled
    // command; released, a live model's retry of it cannot hold the steer open.
    std::fs::write(&release, b"release").expect("release worker");
    assert_eq!(
        wait_for_request_terminal(fx.node(), &steer.request_id, Duration::from_secs(240)).await,
        "completed"
    );
    let steer_answer = terminal_assistant_answer(fx.node(), &steer.request_id).await;
    let steer_token = steer_code.rsplit('-').next().expect("code word suffix");
    assert!(
        steer_answer.contains(steer_token),
        "the steered session's final answer must reflect the steer: {steer_answer:?}"
    );
    let worker_requests = session_requests(fx.node(), &worker.session_id)
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
        &fx.db.node,
        steer_request_id,
        root_session,
        &message_row.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    wait_for_message_containing(
        &fx.db.node,
        steer_request_id,
        root_session,
        &completion_marker(&message_row.tool_call_id, AGENT_MESSAGE_TOOL_NAME),
        Duration::from_secs(60),
    )
    .await;
    wait_for_session_quiescent(fx.node(), root_session, Duration::from_secs(240)).await;
    wait_for_session_quiescent(fx.node(), &worker.session_id, Duration::from_secs(60)).await;
    tracing::info!(
        worker_session = %worker.session_id,
        interrupted = %worker.request_id,
        steer = %steer.request_id,
        "[live-steer] interrupting message stopped the turn and ran next"
    );
    agent.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test 9: an active Goal does not suppress a completion wake
// ---------------------------------------------------------------------------

const GOAL_WORKER_AGENT_ID: &str = "live-goal-worker";

const GOAL_ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are an orchestrator in an integration test. \
Follow the latest user instruction exactly, calling only the tools it names. Never call agent_new \
more than once in this conversation. When you are asked to continue working toward your goal and \
no background completion notification has arrived yet, call no tool and reply exactly \
WAITING_FOR_WORKER. Once a background completion notification has arrived, call no tool and reply \
with the code word it reports, verbatim.";

/// A session with an active Goal starts a worker and keeps continuing while
/// the worker is blocked. Once released, the worker's completion is delivered
/// through its own completed wake, no newer Goal continuation is claimed
/// before that wake, and the Goal continues after it. Which request a later
/// continuation names as its parent is covered by the native Goal tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 and pass --ignored"]
async fn live_goal_does_not_suppress_completion_wake() -> Result<()> {
    if !live_enabled() {
        return Ok(());
    }
    init_live_test_tracing();
    let target = live_target();
    assert_model_available(&target).await;

    let workspace = tempfile::tempdir().expect("goal workspace");
    let release = workspace.path().join("release-goal-worker");
    let worker_code = code_word("GOALWORKER");
    let args = blocked_bash_args("GOAL_WORKER_STARTED", &release, "GOAL_WORKER_DONE");
    let fx = LiveNode::new(
        "session-message-live-goal",
        &target,
        GOAL_ORCHESTRATOR_SYSTEM_PROMPT,
    )
    .await;
    fx.agent(
        &target,
        GOAL_WORKER_AGENT_ID,
        &blocked_worker_prompt("worker", "RUN_JOB", &args, &worker_code),
        "Runs a blocked job and reports its code word.",
    )
    .await;
    authorize_session_targets(
        fx.node(),
        &fx.did,
        &fx.orchestrator,
        vec![fx.target("worker", GOAL_WORKER_AGENT_ID)],
    )
    .await;
    configure_bash_agent_tools(
        fx.node(),
        &fx.did,
        GOAL_WORKER_AGENT_ID,
        workspace.path(),
        Vec::new(),
    )
    .await;
    let agent = fx.boot(workspace.path()).await?;

    let session = "session-live-goal-parent";
    let goal = set_goal(
        fx.node(),
        &fx.did,
        session,
        Some("Wait for the background completion notification from the worker session that was already started, then report its code word verbatim. Never start or message another session."),
        Some(GoalStatus::Active),
        Some(Some(2_000_000)),
    )
    .await?;
    assert_not_started(&release);
    let start_request_id = "req-live-goal-start";
    fx.request(
        start_request_id,
        session,
        "Call agent_new exactly once now with agent \"worker\" and prompt \"RUN_JOB\". After its running receipt arrives, reply exactly WORKER_STARTED and call no other tool.",
    )
    .await;
    let row = wait_for_background_tool_call(
        &fx.db.node,
        start_request_id,
        session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let worker = wait_for_caused_request(fx.node(), start_request_id, Duration::from_secs(120))
        .await
        .expect("the Goal session must start the worker");
    wait_for_started_marker(&release, "GOAL_WORKER_STARTED", Duration::from_secs(240)).await;
    assert_eq!(
        wait_for_request_terminal(fx.node(), start_request_id, Duration::from_secs(240)).await,
        "completed"
    );

    // The Goal keeps continuing while the worker still owes its result.
    let continuation = wait_for_session_request(
        fx.node(),
        session,
        |row| {
            row.caused_by_trigger_kind.as_deref() == Some("goal")
                && row.caused_by_parent_request_id.as_deref() == Some(start_request_id)
        },
        Duration::from_secs(120),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            fx.node(),
            &continuation.request_id,
            Duration::from_secs(240)
        )
        .await,
        "completed"
    );
    let worker_state = fetch_request_lifecycle(fx.node(), &worker.request_id)
        .await
        .expect("worker lifecycle");
    assert!(
        !is_terminal(&worker_state),
        "test premise: the worker must still be blocked during the Goal continuation; it is {worker_state}"
    );

    std::fs::write(&release, b"release").expect("release goal worker");
    assert_eq!(
        wait_for_request_terminal(fx.node(), &worker.request_id, Duration::from_secs(240)).await,
        "completed"
    );
    let notification =
        wait_for_completion_notification(fx.node(), &row.doc_id, Duration::from_secs(120)).await;
    assert_eq!(notification.session_id.as_deref(), Some(session));
    // An active Goal must not suppress the wake: the notification binds a
    // completion wake, not a Goal continuation, and that wake runs.
    let wake = wait_for_bound_wake(fx.node(), session, &notification).await;
    // The Goal is not wedged by the wake: some continuation is claimed after
    // the wake ends. Its parent edge is not asserted here.
    let wake_terminalized = rfc3339(&wake.terminalized_at).expect("terminal wake time");
    let resumed = wait_for_session_request(
        fx.node(),
        session,
        |row| {
            row.caused_by_trigger_kind.as_deref() == Some("goal")
                && rfc3339(&row.claimed_at).is_some_and(|claimed| claimed > wake_terminalized)
        },
        Duration::from_secs(180),
    )
    .await;
    set_goal(
        fx.node(),
        &fx.did,
        session,
        None,
        Some(GoalStatus::Paused),
        None,
    )
    .await?;
    wait_for_session_quiescent(fx.node(), session, Duration::from_secs(240)).await;
    assert_deliveries_bound_to_completed_wakes(fx.node(), session, &[&row.doc_id]).await;

    // No Goal continuation created after the wake runs ahead of it.
    // `created_at` has second resolution, so only a continuation created in a
    // later second is decisively newer than the wake.
    let requests = session_requests(fx.node(), session).await;
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
    let mut continued = HashSet::new();
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
    let wake_answer = terminal_assistant_answer(fx.node(), &wake.request_id).await;
    if !wake_answer.contains(&worker_code) {
        tracing::warn!(
            "[live-goal] SOFT-WARN: the wake did not report {worker_code}: {wake_answer:?}"
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

/// The first request of `session_id` that satisfies `matches`.
async fn wait_for_session_request(
    node: &EmbeddedNode,
    session_id: &str,
    matches: impl Fn(&SessionRequestRow) -> bool,
    timeout: Duration,
) -> SessionRequestRow {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let requests = session_requests(node, session_id).await;
        if let Some(found) = requests.iter().find(|row| matches(row)) {
            return found.clone();
        }
        if tokio::time::Instant::now() >= deadline {
            dump_session_diagnostics(node, session_id).await;
            panic!("no matching request in session {session_id}; requests={requests:?}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn rfc3339(value: &Option<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    value
        .as_deref()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.with_timezone(&chrono::Utc))
}

// ---------------------------------------------------------------------------
// Test 10: a restart mid-delegation delivers the completion exactly once
// ---------------------------------------------------------------------------

const RESTART_WORKER_AGENT_ID: &str = "live-restart-worker";

/// The caller's runtime stops while the session it started on another node
/// is blocked, then restarts on the same store. The started session keeps
/// running meanwhile; once released, its terminal settles the caller's row
/// and is delivered as exactly one notification bound to a completed wake,
/// still exactly one after a crash and restart that follow delivery.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 and pass --ignored"]
async fn live_restart_mid_delegation_recovers() -> Result<()> {
    if !live_enabled() {
        return Ok(());
    }
    init_live_test_tracing();
    let target = live_target();
    assert_model_available(&target).await;

    let workspace = tempfile::tempdir().expect("restart workspace");
    let release = workspace.path().join("release-restart-worker");
    let worker_code = code_word("RESTART");
    let args = blocked_bash_args("RESTART_WORKER_STARTED", &release, "RESTART_WORKER_DONE");
    let db_a = test_p2p_db("session-message-live-restart-a").await;
    let db_b = test_p2p_db("session-message-live-restart-b").await;
    let identity_a: Arc<dyn NodeIdentity> = db_a.node_identity.clone();
    let identity_b: Arc<dyn NodeIdentity> = db_b.node_identity.clone();
    let did_a = identity_a.did().to_string();
    let did_b = identity_b.did().to_string();
    let orchestrator = default_agent_id_for_node(&did_a);

    let profile_b = default_inference_profile_id_for_agent(&default_agent_id_for_node(&did_b));
    upsert_live_backend(db_b.node.as_ref(), &did_b, &target).await;
    configure_agent(
        db_b.node.as_ref(),
        RESTART_WORKER_AGENT_ID,
        &did_b,
        &target,
        &profile_b,
        &blocked_worker_prompt("worker", "RUN_JOB", &args, &worker_code),
        Some("Runs a blocked job and reports its code word."),
        true,
    )
    .await;
    configure_bash_agent_tools(
        db_b.node.as_ref(),
        &did_b,
        RESTART_WORKER_AGENT_ID,
        workspace.path(),
        Vec::new(),
    )
    .await;
    let profile_a = default_inference_profile_id_for_agent(&orchestrator);
    upsert_live_backend(db_a.node.as_ref(), &did_a, &target).await;
    configure_agent(
        db_a.node.as_ref(),
        &orchestrator,
        &did_a,
        &target,
        &profile_a,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
        true,
    )
    .await;
    authorize_session_targets(
        db_a.node.as_ref(),
        &did_a,
        &orchestrator,
        vec![agent_target(
            &did_a,
            "worker",
            did_b.clone(),
            RESTART_WORKER_AGENT_ID,
        )],
    )
    .await;
    let agent_b = boot_workspace_agent(&db_b, identity_b.clone(), workspace.path()).await?;
    let agent_a = boot_document_agent(&db_a, identity_a.clone()).await?;
    let (peer_a, peer_b) = pair_session_message_nodes(&db_a, &identity_a, &db_b, &identity_b).await;

    assert_not_started(&release);
    let session = "session-live-restart-parent";
    let start_request_id = "req-live-restart-start";
    create_runtime_request(
        db_a.node.as_ref(),
        &did_a,
        &orchestrator,
        start_request_id,
        session,
        "Call agent_new exactly once now with agent \"worker\" and prompt \"RUN_JOB\". After its running receipt arrives, reply exactly WORKER_STARTED and call no other tool.",
    )
    .await;
    let row = wait_for_background_tool_call(
        &db_a.node,
        start_request_id,
        session,
        AGENT_NEW_TOOL_NAME,
        Duration::from_secs(240),
    )
    .await;
    let worker = wait_for_caused_request(
        db_a.node.as_ref(),
        start_request_id,
        Duration::from_secs(120),
    )
    .await
    .expect("the caller must start the worker");
    let caller_doc_id = requests_where(
        db_a.node.as_ref(),
        "request_id",
        &format!(
            r#"{{ _eq: "{}" }}"#,
            escape_graphql_string(start_request_id)
        ),
    )
    .await
    .into_iter()
    .next()
    .expect("calling request row")
    .doc_id;
    assert_eq!(
        worker.caused_by_parent_tool_call_doc_id.as_deref(),
        Some(row.doc_id.as_str()),
        "the worker must be caused by the observed agent_new row: {worker:?}"
    );
    assert_eq!(
        worker.caused_by_parent_tool_call_id.as_deref(),
        Some(row.tool_call_id.as_str())
    );
    assert_eq!(
        worker.caused_by_parent_request_doc_id.as_deref(),
        Some(caller_doc_id.as_str())
    );
    assert_eq!(worker.node_did, did_b);
    assert_eq!(worker.admission_kind.as_deref(), Some("peer"));
    wait_for_request_on_node(
        db_b.node.as_ref(),
        &worker.request_id,
        Duration::from_secs(120),
    )
    .await
    .expect("the caused request must replicate to the worker's node");
    wait_for_started_marker(&release, "RESTART_WORKER_STARTED", Duration::from_secs(240)).await;
    assert_eq!(
        wait_for_request_terminal(
            db_a.node.as_ref(),
            start_request_id,
            Duration::from_secs(240)
        )
        .await,
        "completed"
    );

    agent_a.shutdown().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let worker_state = fetch_request_lifecycle(db_b.node.as_ref(), &worker.request_id)
        .await
        .expect("worker lifecycle on its node");
    assert!(
        !is_terminal(&worker_state),
        "stopping the caller must not stop the session it started; worker is {worker_state}"
    );
    assert_eq!(
        fetch_tool_call(&db_a.node, start_request_id, session, &row.tool_call_id)
            .await
            .expect("agent_new row while the caller is down")
            .lifecycle_state,
        "running"
    );
    let restarted_a = boot_document_agent(&db_a, identity_a.clone()).await?;
    wait_for_session_message_routes(&db_a, &peer_b, &db_b, &peer_a).await;
    assert!(
        completion_notifications(db_a.node.as_ref(), &row.doc_id)
            .await
            .is_empty(),
        "nothing may be delivered before the worker finishes"
    );

    std::fs::write(&release, b"release").expect("release restart worker");
    assert_eq!(
        wait_for_request_terminal(
            db_b.node.as_ref(),
            &worker.request_id,
            Duration::from_secs(240)
        )
        .await,
        "completed"
    );
    assert_eq!(
        wait_for_request_terminal(
            db_a.node.as_ref(),
            &worker.request_id,
            Duration::from_secs(120)
        )
        .await,
        "completed",
        "the worker's terminal must replicate back to the restarted caller"
    );
    let answer = wait_for_assistant_answer(
        db_a.node.as_ref(),
        &worker.request_id,
        Duration::from_secs(60),
    )
    .await;
    if !answer.contains(&worker_code) {
        tracing::warn!("[live-restart] SOFT-WARN: worker answer lacks {worker_code}: {answer:?}");
    }
    wait_for_tool_call_state(
        &db_a.node,
        start_request_id,
        session,
        &row.tool_call_id,
        "completed",
        Duration::from_secs(120),
    )
    .await;
    let notification =
        wait_for_completion_notification(db_a.node.as_ref(), &row.doc_id, Duration::from_secs(120))
            .await;
    assert_eq!(notification.session_id.as_deref(), Some(session));
    let wake = wait_for_bound_wake(db_a.node.as_ref(), session, &notification).await;
    wait_for_session_quiescent(db_a.node.as_ref(), session, Duration::from_secs(240)).await;
    assert_deliveries_bound_to_completed_wakes(db_a.node.as_ref(), session, &[&row.doc_id]).await;

    // A crash and restart re-runs every recovery owner over the same facts;
    // none may deliver the completion again.
    restarted_a.crash().await;
    let again = boot_document_agent(&db_a, identity_a).await?;
    wait_for_session_message_routes(&db_a, &peer_b, &db_b, &peer_a).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    wait_for_session_quiescent(db_a.node.as_ref(), session, Duration::from_secs(120)).await;
    for (node, label) in [
        (db_a.node.as_ref(), "caller"),
        (db_b.node.as_ref(), "worker"),
    ] {
        let caused = requests_caused_by_row(node, &row.doc_id).await;
        assert_eq!(
            caused
                .iter()
                .map(|request| request.request_id.as_str())
                .collect::<Vec<_>>(),
            vec![worker.request_id.as_str()],
            "the agent_new row must cause exactly the observed worker on the {label} node: {caused:?}"
        );
    }
    assert_eq!(
        completion_notifications(db_a.node.as_ref(), &row.doc_id)
            .await
            .len(),
        1,
        "the completion notification must be delivered exactly once"
    );
    assert_deliveries_bound_to_completed_wakes(db_a.node.as_ref(), session, &[&row.doc_id]).await;
    tracing::info!(
        worker = %worker.request_id,
        wake = %wake.request_id,
        "[live-restart] the worker outlived the caller's restart; completion delivered exactly once"
    );
    again.shutdown().await;
    agent_b.shutdown().await;
    // BootedAgent only stops Gents::run; P2P belongs to the embedded node.
    db_a.node.shutdown().await;
    db_b.node.shutdown().await;
    Ok(())
}

/// A completion notification a background row published into its session.
#[derive(Debug, Clone, Deserialize)]
struct NotificationRow {
    session_id: Option<String>,
    request_doc_id: Option<String>,
}

/// The notifications keyed by the row `tool_call_doc_id`; exactly-once
/// delivery means at most one ever exists.
async fn completion_notifications(
    node: &EmbeddedNode,
    tool_call_doc_id: &str,
) -> Vec<NotificationRow> {
    let key = format!("background-completion-notification:{tool_call_doc_id}:tool");
    let query = format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }} }}) {{ session_id request_doc_id }} }}"#,
        escape_graphql_string(&key)
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query completion notifications failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data["AgentMessage"].as_array())
        .into_iter()
        .flatten()
        .map(|row| serde_json::from_value(row.clone()).expect("decode completion notification"))
        .collect()
}

/// The completion wake `notification` binds, once it has run to terminal.
async fn wait_for_bound_wake(
    node: &EmbeddedNode,
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
    node: &EmbeddedNode,
    tool_call_doc_id: &str,
    timeout: Duration,
) -> NotificationRow {
    let deadline = tokio::time::Instant::now() + timeout;
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
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

// ---------------------------------------------------------------------------
// Test 11: randomized soak over the agents tools
// ---------------------------------------------------------------------------

const SOAK_GATES: usize = 3;
const SOAK_PRELUDE: usize = 5;

/// Operations of the soak's root requests that took effect, read from the
/// durable rows.
#[derive(Debug, Default)]
struct SoakCoverage {
    agent_new_started: usize,
    messages_delivered: usize,
    interrupting_messages: usize,
    interrupts_landed: usize,
    interrupts_refused: usize,
    interrupts_idle: usize,
    lists: usize,
}

async fn soak_coverage(fx: &LiveNode, op_requests: &[String]) -> SoakCoverage {
    let mut coverage = SoakCoverage::default();
    for request_id in op_requests {
        for tool in timeline_tools(&fx.db.node, &request_id)
            .await
            .into_iter()
            .filter(|tool| tool.request_id.as_deref() == Some(request_id.as_str()))
        {
            let result = tool
                .result
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .unwrap_or_default();
            let caused = match tool.doc_id.as_deref() {
                Some(doc) => !requests_caused_by_row(fx.node(), doc).await.is_empty(),
                None => false,
            };
            match tool.tool_name.as_str() {
                AGENT_NEW_TOOL_NAME if caused => coverage.agent_new_started += 1,
                AGENT_MESSAGE_TOOL_NAME if caused => {
                    coverage.messages_delivered += 1;
                    let args =
                        serde_json::from_str::<serde_json::Value>(&tool.args).unwrap_or_default();
                    if args["interrupt"] == true {
                        coverage.interrupting_messages += 1;
                    }
                }
                AGENT_INTERRUPT_TOOL_NAME => match result["status"].as_str() {
                    Some("interrupting") => coverage.interrupts_landed += 1,
                    Some("idle") => coverage.interrupts_idle += 1,
                    _ if result["ok"] == false => coverage.interrupts_refused += 1,
                    _ => {}
                },
                AGENT_LIST_TOOL_NAME if result.get("sessions").is_some() => coverage.lists += 1,
                _ => {}
            }
        }
    }
    coverage
}

fn soak_enabled() -> bool {
    live_enabled() && std::env::var("GENTS_LIVE_SOAK").as_deref() == Ok("1")
}

/// Logs the seed when the soak fails anywhere, including inside a helper.
struct SoakSeed(u64);

impl Drop for SoakSeed {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(
                seed = self.0,
                "[live-soak] FAILED; reproduce with GENTS_LIVE_SOAK_SEED={}",
                self.0
            );
        }
    }
}

/// Seeded random sequences of `agent_new`, `agent_message` (with and without
/// interrupt), `agent_interrupt` and `agent_list` from two root sessions over
/// a pool of answering and blocking agents, with the test releasing blocking
/// gates at random. After quiescence the durable rows must satisfy the
/// delegation invariants.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SESSION_MESSAGE=1 GENTS_LIVE_SOAK=1 and pass --ignored"]
async fn live_randomized_soak() -> Result<()> {
    use rand::{Rng, SeedableRng};

    if !soak_enabled() {
        return Ok(());
    }
    init_live_test_tracing();
    let seed = std::env::var("GENTS_LIVE_SOAK_SEED")
        .ok()
        .map(|seed| seed.parse::<u64>().expect("GENTS_LIVE_SOAK_SEED is a u64"))
        .unwrap_or_else(rand::random);
    let iterations = std::env::var("GENTS_LIVE_SOAK_ITERS")
        .ok()
        .map(|n| {
            n.parse::<usize>()
                .expect("GENTS_LIVE_SOAK_ITERS is a count")
        })
        .unwrap_or(20);
    let _seed_guard = SoakSeed(seed);
    tracing::info!(
        seed,
        iterations,
        "[live-soak] start; reproduce with GENTS_LIVE_SOAK_SEED={seed}"
    );
    let target = live_target();
    assert_model_available(&target).await;

    let workspace = tempfile::tempdir().expect("soak workspace");
    let gates = (0..SOAK_GATES)
        .map(|gate| workspace.path().join(format!("gate-{gate}")))
        .collect::<Vec<_>>();
    let fx = LiveNode::new(
        "session-message-live-soak",
        &target,
        DELEGATING_ORCHESTRATOR_SYSTEM_PROMPT,
    )
    .await;
    for (agent_id, name) in [(ALPHA_AGENT_ID, "alpha"), (BETA_AGENT_ID, "beta")] {
        fx.agent(
            &target,
            agent_id,
            &code_worker_prompt(name, &code_word("FIRST"), &code_word("SECOND")),
            "Knows a code word.",
        )
        .await;
    }
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
    fx.agent(
        &target,
        BLOCKER_AGENT_ID,
        &format!(
            "You are agent blocker in an integration test.\n{gate_rules}\nFor any other request, call no tool and reply exactly BLOCKER_IDLE."
        ),
        "Runs gated jobs.",
    )
    .await;
    let pool = [
        ("alpha", ALPHA_AGENT_ID),
        ("beta", BETA_AGENT_ID),
        ("blocker", BLOCKER_AGENT_ID),
    ];
    authorize_session_targets(
        fx.node(),
        &fx.did,
        &fx.orchestrator,
        pool.iter()
            .map(|(name, agent_id)| fx.target(name, agent_id))
            .collect(),
    )
    .await;
    configure_bash_agent_tools(
        fx.node(),
        &fx.did,
        BLOCKER_AGENT_ID,
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
    // Sessions started so far, with the agent each runs. Interrupts favor
    // blocker sessions, the ones likely to be busy.
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
    let mut op_requests = Vec::new();
    for step in 0..iterations {
        let roll = rng.random_range(0..100);
        let root = roots[rng.random_range(0..roots.len())];
        // A fixed prelude guarantees coverage of every kind against busy
        // sessions: each root starts a blocker, root 0 interrupts its own,
        // root 1 messages its own, and root 0 lists. The rest is seeded.
        let (root, prompt) = if step < SOAK_PRELUDE {
            let root = roots[step % roots.len()];
            let prompt = match step {
                0 | 1 => agent_new("blocker", &format!("BLOCK_GATE_{step}")),
                2 => {
                    wait_for_started_marker(&gates[0], "GATE_0_STARTED", Duration::from_secs(240))
                        .await;
                    format!("Call agent_interrupt exactly once now with session_id {:?}. Then reply exactly OP_DONE and call no other tool.", session_of(&started, 0))
                }
                3 => {
                    wait_for_started_marker(&gates[1], "GATE_1_STARTED", Duration::from_secs(240))
                        .await;
                    format!("Call agent_message exactly once now with session_id {:?}, message \"BLOCK_GATE_2\" and interrupt false. After its receipt arrives, reply exactly OP_DONE and call no other tool.", session_of(&started, 1))
                }
                _ => "Call agent_list exactly once now, then reply exactly OP_DONE and call no other tool.".to_owned(),
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
                "Call agent_list exactly once now, then reply exactly OP_DONE and call no other tool."
                    .to_owned(),
            )
        } else {
            let gate = rng.random_range(0..SOAK_GATES);
            std::fs::write(&gates[gate], b"release").expect("release soak gate");
            tracing::info!(seed, step, gate, "[live-soak] released gate");
            continue;
        };
        let request_id = format!("req-live-soak-{step}");
        op_requests.push(request_id.clone());
        tracing::info!(seed, step, root, %prompt, "[live-soak] op");
        fx.request(&request_id, root, &prompt).await;
        let state =
            wait_for_request_terminal(fx.node(), &request_id, Duration::from_secs(300)).await;
        tracing::info!(seed, step, %state, "[live-soak] op settled");
        for caused in fetch_caused_requests(fx.node(), &request_id).await {
            if started
                .iter()
                .any(|(session, _)| *session == caused.session_id)
            {
                continue;
            }
            if let Some((name, _)) = pool
                .iter()
                .find(|(_, agent_id)| *agent_id == caused.agent_id)
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
        std::fs::write(gate, b"release").expect("release soak gate");
    }
    wait_for_node_quiescent(fx.node(), &fx.did, Duration::from_secs(900)).await;

    let coverage = soak_coverage(&fx, &op_requests).await;
    tracing::info!(
        seed,
        iterations,
        ?coverage,
        "[live-soak] executed operations"
    );
    assert!(
        coverage.agent_new_started >= roots.len()
            && coverage.messages_delivered >= 1
            && coverage.interrupts_landed >= 1
            && coverage.lists >= 1,
        "[live-soak] seed={seed} iterations={iterations}: operations did not execute: {coverage:?}"
    );
    let violations = soak_violations(&fx).await;
    assert!(
        violations.is_empty(),
        "[live-soak] seed={seed} iterations={iterations}: invariant violations: {violations:#?}"
    );
    // Per caller session, every delivered completion is bound to a completed
    // wake.
    let mut delivered = std::collections::BTreeMap::<String, Vec<String>>::new();
    for row in session_message_tool_rows(fx.node(), &fx.did).await {
        if row.completion_notification_delivered_at.is_some() {
            delivered
                .entry(row.session_id)
                .or_default()
                .push(row.doc_id);
        }
    }
    for (session, rows) in &delivered {
        let rows = rows.iter().map(String::as_str).collect::<Vec<_>>();
        assert_deliveries_bound_to_completed_wakes(fx.node(), session, &rows).await;
    }
    agent.shutdown().await;
    Ok(())
}

/// An `agent_new`/`agent_message` row of the node_config.
#[derive(Debug, Clone, Deserialize)]
struct SessionMessageToolRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_id: String,
    tool_name: String,
    request_id: String,
    session_id: String,
    lifecycle_state: Option<String>,
    completion_notification_delivered_at: Option<String>,
}

async fn session_message_tool_rows(
    node: &EmbeddedNode,
    node_did: &str,
) -> Vec<SessionMessageToolRow> {
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ node_did: {{ _eq: "{}" }}, tool_name: {{ _in: ["{}", "{}"] }} }}) {{ _docID tool_call_id tool_name request_id session_id lifecycle_state completion_notification_delivered_at }} }}"#,
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

/// Wait until every request of the node_config is terminal and every
/// session-message row has settled and delivered its notification, twice in
/// a row so a wake published by the last settlement is observed.
async fn wait_for_node_quiescent(node: &EmbeddedNode, node_did: &str, timeout: Duration) {
    let condition = format!(r#"{{ _eq: "{}" }}"#, escape_graphql_string(node_did));
    let deadline = tokio::time::Instant::now() + timeout;
    let mut quiet_polls = 0;
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
            quiet_polls += 1;
            if quiet_polls >= 2 {
                return;
            }
        } else {
            quiet_polls = 0;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "node_config never became quiescent; busy requests={busy_requests:?}; busy rows={busy_rows:?}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Every delegation invariant the durable rows violate.
async fn soak_violations(fx: &LiveNode) -> Vec<String> {
    let node = fx.node();
    let max_request_hop = i64::from(
        ensure_node(node, &fx.did)
            .await
            .expect("node_config")
            .max_request_hop
            .unwrap_or(8),
    );
    let requests = requests_where(
        node,
        "node_did",
        &format!(r#"{{ _eq: "{}" }}"#, escape_graphql_string(&fx.did)),
    )
    .await;
    let rows = session_message_tool_rows(node, &fx.did).await;
    let by_id = requests
        .iter()
        .map(|row| (row.request_id.as_str(), row))
        .collect::<std::collections::HashMap<_, _>>();
    let by_doc = requests
        .iter()
        .map(|row| (row.doc_id.as_str(), row))
        .collect::<std::collections::HashMap<_, _>>();
    let rows_by_doc = rows
        .iter()
        .map(|row| (row.doc_id.as_str(), row))
        .collect::<std::collections::HashMap<_, _>>();
    let hop = |row: &SessionRequestRow| row.request_hop.unwrap_or(0);
    let mut violations = Vec::new();
    let mut caused_per_row = std::collections::HashMap::<&str, usize>::new();

    for request in &requests {
        let state = request.lifecycle_state.as_deref().unwrap_or("<none>");
        if !is_terminal(state) {
            violations.push(format!("non-terminal after quiescence: {request:?}"));
        }
        let refused_by_bound = state == "failed"
            && request
                .failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("max_request_hop"));
        if hop(request) > max_request_hop && !refused_by_bound {
            violations.push(format!(
                "hop {} exceeds max_request_hop {max_request_hop}: {request:?}",
                hop(request)
            ));
        }
        if request.caused_by_parent_tool_call_id.is_some() {
            // A request caused by agent_new/agent_message.
            let Some(parent) = request
                .caused_by_parent_request_id
                .as_deref()
                .and_then(|id| by_id.get(id))
            else {
                violations.push(format!(
                    "caused request names no parent request: {request:?}"
                ));
                continue;
            };
            let Some(row) = request
                .caused_by_parent_tool_call_doc_id
                .as_deref()
                .and_then(|doc| rows_by_doc.get(doc))
            else {
                violations.push(format!(
                    "caused request names no session-message row: {request:?}"
                ));
                continue;
            };
            *caused_per_row.entry(row.doc_id.as_str()).or_default() += 1;
            if Some(row.tool_call_id.as_str()) != request.caused_by_parent_tool_call_id.as_deref()
                || row.request_id != parent.request_id
                || row.session_id != parent.session_id
            {
                violations.push(format!(
                    "caused request provenance disagrees with its row: request={request:?} row={row:?} parent={parent:?}"
                ));
            }
            let expected_min = hop(parent) + 1;
            let hop_ok = if row.tool_name == AGENT_NEW_TOOL_NAME {
                hop(request) == expected_min && request.session_id != parent.session_id
            } else {
                hop(request) >= expected_min
            };
            if !hop_ok {
                violations.push(format!(
                    "{} hop {} does not follow the rule from caller hop {}: {request:?}",
                    row.tool_name,
                    hop(request),
                    hop(parent)
                ));
            }
            if is_terminal(state) {
                if !matches!(
                    row.lifecycle_state.as_deref(),
                    Some("completed" | "failed" | "cancelled")
                ) || row.completion_notification_delivered_at.is_none()
                {
                    violations.push(format!(
                        "terminal caused request left its row unsettled or undelivered: request={request:?} row={row:?}"
                    ));
                }
                let notifications = completion_notifications(node, &row.doc_id).await;
                if notifications.len() != 1 {
                    violations.push(format!(
                        "terminal caused request produced {} notifications: request={request:?} row={row:?}",
                        notifications.len()
                    ));
                }
                for notification in notifications {
                    let bound = notification
                        .request_doc_id
                        .as_deref()
                        .and_then(|doc| by_doc.get(doc));
                    if notification.session_id.as_deref() != Some(row.session_id.as_str())
                        || !bound.is_some_and(|wake| {
                            wake.session_id == row.session_id
                                && wake.is_background_completion_wake()
                        })
                    {
                        violations.push(format!(
                            "notification is not bound to a wake in the caller's session: {notification:?} wake={bound:?} row={row:?}"
                        ));
                    }
                }
            }
        } else if request.is_background_completion_wake() {
            let parent = request
                .caused_by_parent_request_id
                .as_deref()
                .and_then(|id| by_id.get(id));
            match parent {
                Some(parent) if parent.session_id == request.session_id => {
                    if hop(request) < hop(parent) + 1 && !refused_by_bound {
                        violations.push(format!(
                            "wake hop {} is not past its session-message completion (owning hop {}): {request:?}",
                            hop(request),
                            hop(parent)
                        ));
                    }
                }
                _ => violations.push(format!(
                    "wake names no owning request in its session: {request:?}"
                )),
            }
        }
    }
    for row in &rows {
        let caused = caused_per_row
            .get(row.doc_id.as_str())
            .copied()
            .unwrap_or(0);
        if caused > 1 || (row.lifecycle_state.as_deref() == Some("completed") && caused != 1) {
            violations.push(format!("row caused {caused} requests: {row:?}"));
        }
    }

    // Interrupts reach only their addressed session.
    let mut addressed = HashSet::new();
    let mut interrupting = 0;
    for request in requests.iter().filter(|row| !row.is_title_audit()) {
        for tool in timeline_tools(&fx.db.node, &request.request_id)
            .await
            .into_iter()
            .filter(|tool| tool.request_id.as_deref() == Some(request.request_id.as_str()))
        {
            let args = serde_json::from_str::<serde_json::Value>(&tool.args).unwrap_or_default();
            let result = tool
                .result
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .unwrap_or_default();
            let interrupts = (tool.tool_name == AGENT_INTERRUPT_TOOL_NAME && result["ok"] == true)
                || (tool.tool_name == AGENT_MESSAGE_TOOL_NAME && args["interrupt"] == true);
            if result["status"] == "interrupting" {
                interrupting += 1;
            }
            if interrupts {
                if let Some(session) = args["session_id"].as_str() {
                    addressed.insert(session.trim().to_owned());
                }
            }
        }
    }
    for request in requests
        .iter()
        .filter(|row| row.lifecycle_state.as_deref() == Some("interrupted"))
    {
        if !addressed.contains(&request.session_id) {
            violations.push(format!(
                "request interrupted in a session no interrupt addressed: {request:?}"
            ));
        }
    }
    tracing::info!(
        requests = requests.len(),
        caused = caused_per_row.len(),
        wakes = requests
            .iter()
            .filter(|row| row.is_background_completion_wake())
            .count(),
        interrupted = requests
            .iter()
            .filter(|row| row.lifecycle_state.as_deref() == Some("interrupted"))
            .count(),
        addressed = addressed.len(),
        interrupting,
        max_hop = requests.iter().map(hop).max().unwrap_or(0),
        "[live-soak] invariant scan"
    );
    violations
}

// ---------------------------------------------------------------------------
// System prompts
// ---------------------------------------------------------------------------

const ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are an orchestrator agent. You can start a session on \
an agent named `researcher`. For ANY research or factual lookup the user asks for, you MUST call the \
`agent_new` tool with agent exactly \"researcher\" and a `prompt` describing the question, then \
tell the user the research is under way without calling any other tool. Do not answer factual \
questions yourself. When a background completion notification arrives, relay its answer to the user \
without calling any tool.";

const CROSS_NODE_ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are the root of a deterministic remote \
session test. When asked to run the remote research workflow, call `agent_new` exactly once with \
agent exactly \"fast-worker\" and prompt exactly \"What is the capital of France? Answer in one short \
sentence.\" After its running receipt arrives, reply exactly REMOTE_SESSION_STARTED and do not call any \
other tool. Do not answer the question yourself. When a background completion notification arrives, \
report its answer without calling any tool.";

// ---------------------------------------------------------------------------
// Configuration and boot helpers
// ---------------------------------------------------------------------------

async fn assert_model_available(target: &InferenceTarget) {
    let model = target.model();
    let url = format!("{}/models", target.endpoint().trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    let mut request = client.get(&url);
    if let Some(key) = target.auth().resolve_api_key().expect("target credential") {
        request = request.bearer_auth(key);
    }
    let response = tokio::time::timeout(Duration::from_secs(20), request.send())
        .await
        .unwrap_or_else(|_| panic!("live endpoint {url} timed out"))
        .unwrap_or_else(|error| panic!("live endpoint {url} unreachable: {error}"));
    assert!(
        response.status().is_success(),
        "live endpoint {url} returned status {}",
        response.status()
    );
    let payload: serde_json::Value = response
        .json()
        .await
        .unwrap_or_else(|error| panic!("live endpoint {url} returned invalid model JSON: {error}"));
    let available = payload
        .get("data")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        available.contains(&model),
        "requested live model {model:?} is not served by {url}; available={available:?}"
    );
}

/// Boot a full Gents from the agent documents owned by `identity`'s DID.
async fn boot_document_agent(db: &TestDb, identity: Arc<dyn NodeIdentity>) -> Result<BootedAgent> {
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
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
    wait_for_runtime_ready(db.node.as_ref(), &node_did).await;
    BootedAgent::new(shutdown_tx, handle, node_did)
}

/// Boot a full Gents whose host tools may run blocked commands in
/// `workspace`.
async fn boot_workspace_agent(
    db: &TestDb,
    identity: Arc<dyn NodeIdentity>,
    workspace: &Path,
) -> Result<BootedAgent> {
    let loaded = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(workspace)
                .with_command_timeout_secs(BLOCKED_COMMAND_TIMEOUT_SECS),
            ..Default::default()
        },
    )
    .await?;
    Ok(boot_loaded_document_agent(db, loaded).await)
}

const BLOCKED_COMMAND_TIMEOUT_SECS: u64 = 600;

/// `bash_unrestricted` arguments that print and record `started`, block
/// until `release` exists, then print `done`. The loop also ends once the
/// run's workspace is removed, so a failed run leaves no orphaned shell
/// spinning on the host.
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

/// Give `agent_id` a foreground `bash_unrestricted` rooted at `workspace`
/// and, when `targets` is non-empty, the agents tools over them.
async fn configure_bash_agent_tools(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
    workspace: &Path,
    targets: Vec<AgentTargetDocument>,
) {
    configure_agent_tools(
        node,
        node_did,
        agent_id,
        None,
        Tools {
            tools_id: format!("{agent_id}-bash-tools"),
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

fn assert_standard_backgrounding_tool_surfaces(
    agent: &Gents,
    node_did: &str,
    parent_agent_id: &str,
) {
    let active_agent_ids = agent
        .agents()
        .iter()
        .map(|agent_config| agent_config.agent_id.clone())
        .collect::<HashSet<_>>();
    let parent = agent
        .agents()
        .iter()
        .find(|agent_config| agent_config.agent_id == parent_agent_id)
        .unwrap_or_else(|| {
            panic!(
                "loaded orchestrator agent {parent_agent_id}; active agents: {active_agent_ids:?}; unavailable: {:?}",
                agent.unavailable_agents()
            )
        });
    let parent_surface = parent
        .tools
        .explain_with_runtime(false, node_did, &active_agent_ids);
    for required in [
        "bash_unrestricted",
        AGENT_NEW_TOOL_NAME,
        AGENT_MESSAGE_TOOL_NAME,
        "spawn_process",
        "list_processes",
        "read_process",
        "wait_process",
        "cancel_process",
    ] {
        assert!(
            parent_surface
                .tool_names
                .iter()
                .any(|name| name == required),
            "backgrounding-enabled agent did not provision {required}; resolved={:?}; config={:?}",
            parent_surface.tool_names,
            parent.tools
        );
    }
    assert_eq!(
        parent_surface.included.get("agent"),
        Some(&{
            let mut names = gents::toolset::AGENT_TOOL_NAMES
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>();
            names.sort();
            names
        }),
        "enabled session targets must resolve exactly the agents tool group"
    );
    assert_eq!(
        parent_surface.included.get("background_process"),
        Some(&vec![
            "cancel_process".to_string(),
            "list_processes".to_string(),
            "read_process".to_string(),
            "spawn_process".to_string(),
            "wait_process".to_string(),
        ]),
        "native background allowlisting must resolve the complete process bundle"
    );

    let child = agent
        .agents()
        .iter()
        .find(|agent_config| agent_config.agent_id == BACKGROUND_WORKER_AGENT_ID)
        .expect("loaded background worker agent");
    let child_surface = child
        .tools
        .explain_with_runtime(false, node_did, &active_agent_ids);
    assert!(
        child_surface
            .tool_names
            .iter()
            .any(|name| name == "bash_unrestricted"),
        "background worker must receive its foreground bash tool"
    );
    for parent_only in [AGENT_NEW_TOOL_NAME, "spawn_process", "read_process"] {
        assert!(
            !child_surface
                .tool_names
                .iter()
                .any(|name| name == parent_only),
            "background worker must not inherit parent-only tool {parent_only}"
        );
    }
}

async fn upsert_live_backend(node: &EmbeddedNode, node_did: &str, target: &InferenceTarget) {
    let backend = target.backend(node_did);
    apply_fixture_documents(
        node,
        vec![(
            Collection::InferenceBackend,
            serde_json::to_value(backend).expect("serialize live backend"),
        )],
    )
    .await;
}

/// Upsert an `Agent` document backed by the live backend, with an
/// optional `description` (surfaced in the caller's agent list).
#[allow(clippy::too_many_arguments)]
async fn configure_agent(
    node: &EmbeddedNode,
    agent_id: &str,
    node_did: &str,
    target: &InferenceTarget,
    inference_profile_id: &str,
    system_prompt: &str,
    description: Option<&str>,
    default_for_node: bool,
) {
    let mut node_config = ensure_node(node, node_did)
        .await
        .expect("ensure live fixture node_config");
    let context_id = format!("{agent_id}:context");
    let sampling_id = format!("{agent_id}:live-sampling");
    let profile = InferenceProfile {
        profile_id: inference_profile_id.to_string(),
        sampling_id: Some(sampling_id.clone()),
        reasoning_effort: Some(ReasoningEffort::High),
        ..target.profile(node_did)
    };
    let sampling = InferenceSampling {
        node_did: node_did.to_string(),
        sampling_id,
        display_name: Some("live high-thinking sampling".to_string()),
        temperature: Some(1.0),
        top_p: Some(0.95),
        ..Default::default()
    };
    let context = AgentContext {
        context_id: context_id.clone(),
        node_did: node_did.to_string(),
        display_name: None,
        description: None,
        system_prompt: Some(system_prompt.to_string()),
        tools_id: None,
        compaction_id: None,
        skill_ids: Vec::new(),
        tags: Vec::new(),
    };
    let agent_config = Agent {
        agent_id: agent_id.to_string(),
        node_did: node_did.to_string(),
        display_name: Some(agent_id.to_string()),
        description: description.map(ToOwned::to_owned),
        context_id: Some(context_id),
        inference_profile_id: inference_profile_id.to_string(),
        enabled: true,
        tags: Vec::new(),
        created_at: Some("2026-06-02T00:00:00Z".to_string()),
    };
    let mut documents = vec![
        (
            Collection::InferenceSampling,
            serde_json::to_value(sampling).expect("serialize live inference sampling"),
        ),
        (
            Collection::InferenceProfile,
            serde_json::to_value(profile).expect("serialize live inference profile"),
        ),
        (
            Collection::AgentContext,
            serde_json::to_value(context).expect("serialize live agent context"),
        ),
        (
            Collection::Agent,
            serde_json::to_value(agent_config).expect("serialize live agent"),
        ),
    ];
    if default_for_node {
        node_config.default_agent_id = Some(agent_id.to_string());
        documents.push((
            Collection::Node,
            serde_json::to_value(node_config).expect("serialize live node_config"),
        ));
    }
    apply_fixture_documents(node, documents).await;
}

async fn apply_fixture_documents(
    node: &EmbeddedNode,
    documents: Vec<(Collection, serde_json::Value)>,
) {
    use gents::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };

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
    .expect("build live fixture plan");
    gents::ConfigAccess::transact_local(node, None, "test.live_session_message_fixture", |txn| {
        let plan = &plan;
        Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
    })
    .await
    .expect("apply live fixture documents");
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

/// Publish canonical tools enabling `agent_new`/`agent_message` over
/// `targets` for `agent_id`.
async fn authorize_session_targets(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
    targets: Vec<AgentTargetDocument>,
) {
    configure_agent_tools(
        node,
        node_did,
        agent_id,
        None,
        Tools {
            tools_id: format!("{agent_id}-session-tools"),
            node_did: node_did.to_string(),
            agents: Some(session_targets_group(&targets)),
            ..Default::default()
        },
        target_documents(targets),
    )
    .await;
}

/// Configure the parent with both background lanes and the worker with a
/// foreground bash tool used to hold its request open until the test
/// releases it.
async fn configure_standard_backgrounding_tools(
    node: &EmbeddedNode,
    node_did: &str,
    parent_agent_id: &str,
    workspace: &Path,
) {
    let targets = vec![AgentTargetDocument {
        description: Some("Runs a deliberately blocked background job.".to_string()),
        ..agent_target(
            node_did,
            BACKGROUND_WORKER_TARGET_NAME,
            node_did,
            BACKGROUND_WORKER_AGENT_ID,
        )
    }];
    configure_agent_tools(
        node,
        node_did,
        parent_agent_id,
        None,
        Tools {
            tools_id: format!("{parent_agent_id}-standard-background-tools"),
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
        node,
        node_did,
        BACKGROUND_WORKER_AGENT_ID,
        None,
        Tools {
            tools_id: format!("{BACKGROUND_WORKER_AGENT_ID}-foreground-bash-tools"),
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

// ---------------------------------------------------------------------------
// Request observation
// ---------------------------------------------------------------------------

fn is_terminal(state: &str) -> bool {
    RequestLifecycleState::is_terminal_str(Some(state))
}

async fn fetch_request_lifecycle(node: &EmbeddedNode, request_id: &str) -> Option<String> {
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

/// A request caused by an `agent_new`/`agent_message` call, identified by
/// its `caused_by_parent_*` edge naming a tool call. Completion wakes also
/// name their parent request, but no tool call.
#[derive(Debug, Clone, Deserialize)]
struct CausedRequestRow {
    request_id: String,
    session_id: String,
    node_did: String,
    requester_did: Option<String>,
    agent_id: String,
    lifecycle_state: Option<RequestLifecycleState>,
    admission_kind: Option<String>,
    request_hop: Option<i64>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_parent_tool_call_doc_id: Option<String>,
}

const CAUSED_REQUEST_FIELDS: &str = "request_id session_id node_did requester_did agent_id \
    lifecycle_state admission_kind request_hop caused_by_parent_request_id \
    caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id";

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

async fn fetch_caused_requests(
    node: &EmbeddedNode,
    parent_request_id: &str,
) -> Vec<CausedRequestRow> {
    let escaped = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }}, caused_by_parent_tool_call_id: {{ _ne: null }} }}) {{ {CAUSED_REQUEST_FIELDS} }} }}"#
    );
    caused_request_rows(&node.execute(&query).await)
}

/// The requests whose calling edge names the physical tool-call row.
async fn requests_caused_by_row(
    node: &EmbeddedNode,
    tool_call_doc_id: &str,
) -> Vec<CausedRequestRow> {
    let escaped = escape_graphql_string(tool_call_doc_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{escaped}" }} }}) {{ {CAUSED_REQUEST_FIELDS} }} }}"#
    );
    caused_request_rows(&node.execute(&query).await)
}

async fn wait_for_caused_request(
    node: &EmbeddedNode,
    parent_request_id: &str,
    timeout: Duration,
) -> Option<CausedRequestRow> {
    let deadline = tokio::time::Instant::now() + timeout;
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
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Where a blocked test command records its started marker. A running
/// foreground tool's stdout is not durable until it exits, so the command
/// also writes the marker here.
fn started_path(release: &Path) -> std::path::PathBuf {
    release.with_extension("started")
}

/// The sentinel lives in this run's own temporary workspace; it must not
/// exist before the blocked command is launched, so only that shell can
/// create it.
fn assert_not_started(release: &Path) {
    assert!(
        !started_path(release).exists(),
        "started sentinel exists before its command was launched"
    );
}

async fn wait_for_started_marker(release: &Path, marker: &str, timeout: Duration) {
    let path = started_path(release);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if std::fs::read_to_string(&path).is_ok_and(|text| text.contains(marker)) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the blocked command never reported {marker}; it did not start blocking"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_caused_requests(
    node: &EmbeddedNode,
    parent_request_id: &str,
    count: usize,
    timeout: Duration,
) -> Vec<CausedRequestRow> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let caused = fetch_caused_requests(node, parent_request_id).await;
        if caused.len() >= count {
            return caused;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {count} requests caused by {parent_request_id}; have {caused:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
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
    node: &EmbeddedNode,
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

/// One request, with the lineage and queue facts the live assertions read.
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

async fn session_requests(node: &EmbeddedNode, session_id: &str) -> Vec<SessionRequestRow> {
    requests_where(
        node,
        "session_id",
        &format!(r#"{{ _eq: "{}" }}"#, escape_graphql_string(session_id)),
    )
    .await
}

/// Every request whose `field` satisfies the GraphQL `condition`, oldest first.
async fn requests_where(
    node: &EmbeddedNode,
    field: &str,
    condition: &str,
) -> Vec<SessionRequestRow> {
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

/// The terminal completion wake caused by a background row of
/// `owning_request_id` settling: a background-completion wake whose cause
/// names that request.
async fn wait_for_completion_wake(
    node: &EmbeddedNode,
    session_id: &str,
    owning_request_id: &str,
    timeout: Duration,
) -> SessionRequestRow {
    let deadline = tokio::time::Instant::now() + timeout;
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
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Wait until every request of the session is terminal.
async fn wait_for_session_quiescent(node: &EmbeddedNode, session_id: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
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
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Every listed background row's completion reached the parent as exactly one
/// notification bound to a completed background-completion wake (Lean
/// `WakeAttemptSnapshot.acknowledgedBindings`). A completion landing while an
/// earlier wake is claimed belongs to a successor wake, so which wake carries
/// which notification, the wake count and each wake's wording are not part of
/// the contract. Returns the bound wakes' request ids, one per row.
async fn assert_deliveries_bound_to_completed_wakes(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_doc_ids: &[&str],
) -> Vec<String> {
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

/// Wait for a JSON tool result of `request_id` that satisfies `matches`.
async fn wait_for_json_tool_result(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    matches: impl Fn(&serde_json::Value) -> bool,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + timeout;
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
            dump_session_diagnostics(node.as_ref(), session_id).await;
            panic!("timed out waiting for a matching tool result of {request_id} in session {session_id}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_request_on_node(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> Option<CausedRequestRow> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ {CAUSED_REQUEST_FIELDS} }} }}"#
    );
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(row) = caused_request_rows(&node.execute(&query).await)
            .into_iter()
            .next()
        {
            return Some(row);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Dump tool calls + messages for a session to stderr.
async fn dump_session_diagnostics(node: &EmbeddedNode, session_id: &str) {
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

// ---------------------------------------------------------------------------
// Tool-call rows and transcript observation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ToolCallRow {
    doc_id: String,
    tool_call_id: String,
    lifecycle_state: String,
    args: String,
    result: Option<String>,
    child_request_id: Option<String>,
}

async fn timeline_tools(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
) -> Vec<gents::TimelineToolCallRow> {
    load_run_timeline_rows(&ConfigAccess::Local(node.clone()), request_id)
        .await
        .unwrap_or_else(|error| panic!("load canonical timeline for {request_id}: {error:#}"))
        .tool_calls
}

fn tool_row(row: gents::TimelineToolCallRow) -> ToolCallRow {
    ToolCallRow {
        doc_id: row
            .doc_id
            .expect("canonical timeline tool row omitted physical identity"),
        tool_call_id: row.tool_call_id,
        lifecycle_state: row.lifecycle_state.unwrap_or(row.status),
        args: row.args,
        result: row.result,
        child_request_id: row.child_request_id,
    }
}

async fn fetch_tool_call(
    node: &Arc<EmbeddedNode>,
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
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    tool_call_id: &str,
    expected_state: &str,
    timeout: Duration,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = None;
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
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_tool_call_settled(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    tool_call_id: &str,
    timeout: Duration,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = None;
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
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_background_tool_call(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    tool_name: &str,
    timeout: Duration,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + timeout;
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
        if let Some(state) = fetch_request_lifecycle(node.as_ref(), request_id).await {
            if is_terminal(&state) {
                dump_session_diagnostics(node.as_ref(), session_id).await;
                panic!(
                    "request {request_id} terminalized as {state} before accepted background tool {tool_name} in session {session_id}"
                );
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for background tool {tool_name} in session {session_id}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn assert_no_tool_call(node: &EmbeddedNode, session_id: &str, tool_names: &[&str]) {
    let session_id = escape_graphql_string(session_id);
    for tool_name in tool_names {
        let tool_name = escape_graphql_string(tool_name);
        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        tool_name: {{ _eq: "{tool_name}" }}
                    }}
                ) {{ tool_call_id }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "query forbidden tool calls failed: {:?}",
            response.errors
        );
        let count = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        assert_eq!(count, 0, "model called forbidden control tool {tool_name}");
    }
}

async fn load_session_messages(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
) -> Vec<gents_protocol::message::Message> {
    load_run_timeline_rows(&ConfigAccess::Local(node.clone()), request_id)
        .await
        .unwrap_or_else(|error| {
            panic!("load canonical session timeline for {request_id}: {error:#}")
        })
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

/// Every `agent_new`/`agent_message` receipt the model received.
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

fn model_tool_call_count(messages: &[gents_protocol::message::Message], tool_name: &str) -> usize {
    use gents_protocol::message::{AssistantContent, Message};
    messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { content, .. } => Some(content.iter()),
            _ => None,
        })
        .flatten()
        .filter(|content| {
            matches!(content, AssistantContent::ToolCall(call) if call.function.name == tool_name)
        })
        .count()
}

async fn wait_for_model_tool_call(
    node: &Arc<EmbeddedNode>,
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
        if let Some(state) = fetch_request_lifecycle(node.as_ref(), request_id).await {
            assert!(
                !is_terminal(&state),
                "request {request_id} terminalized as {state} before model tool call {tool_name} in session {session_id}"
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for model tool call {tool_name} in session {session_id}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Wait for one tool-result text part that contains every needle.
async fn wait_for_tool_result_containing(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    needles: &[&str],
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let messages = load_session_messages(node, request_id, session_id).await;
        if tool_result_texts(&messages)
            .iter()
            .any(|text| needles.iter().all(|needle| text.contains(needle)))
        {
            return;
        }
        if let Some(state) = fetch_request_lifecycle(node.as_ref(), request_id).await {
            assert!(
                !is_terminal(&state),
                "request {request_id} terminalized as {state} before a tool result contained {needles:?}; transcript={messages:#?}"
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a tool result containing {needles:?} in session {session_id}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn assert_model_tool_call_count_at_least(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    tool_name: &str,
    expected: usize,
) {
    let messages = load_session_messages(node, request_id, session_id).await;
    let actual = model_tool_call_count(&messages, tool_name);
    assert!(
        actual >= expected,
        "expected at least {expected} model call(s) to {tool_name} in session {session_id}, got {actual}; transcript={messages:#?}"
    );
}

async fn wait_for_message_containing(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    needle: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
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
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[derive(Debug, Clone, Deserialize)]
struct WakeRequestRow {
    request_id: String,
    input: Option<RequestInput>,
}

async fn wait_for_background_wake(
    node: &EmbeddedNode,
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
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The completion wake after `parent_request_id` runs real inference and
/// processes the notification.
async fn assert_wake_observed(node: &EmbeddedNode, session_id: &str, parent_request_id: &str) {
    let wake =
        wait_for_background_wake(node, session_id, parent_request_id, Duration::from_secs(60))
            .await;
    let state = wait_for_request_terminal(node, &wake.request_id, Duration::from_secs(180)).await;
    assert_eq!(state, "completed");
    assert_min_completed_inference_calls(node, &wake.request_id, 1).await;
    let answer = wait_for_assistant_answer(node, &wake.request_id, Duration::from_secs(30)).await;
    assert!(
        answer.contains("BACKGROUND_COMPLETION_OBSERVED"),
        "real-inference wake did not process the completion notification: {answer:?}"
    );
}

async fn assert_min_completed_inference_calls(
    node: &EmbeddedNode,
    request_id: &str,
    expected: usize,
) {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{
                    request_id: {{ _eq: "{request_id}" }},
                    call_state: {{ _eq: "completed" }}
                }}
            ) {{ call_id }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query completed inference calls failed: {:?}",
        response.errors
    );
    let completed = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    assert!(
        completed >= expected,
        "request {request_id} completed with only {completed} real inference call(s); expected at least {expected}"
    );
}

// ---------------------------------------------------------------------------
// Cross-node pairing
// ---------------------------------------------------------------------------

/// Each node enrolls the other's node_config and routes the session-message
/// data plane: A coordinates requests it authors for B, B hosts them for A.
/// B's enrollment is the Peer admission authority for requests DID-A
/// authors for DID-B; both enrollments gate the transport. Returns the peer
/// ids of A and B.
async fn pair_session_message_nodes(
    db_a: &TestDb,
    identity_a: &Arc<dyn NodeIdentity>,
    db_b: &TestDb,
    identity_b: &Arc<dyn NodeIdentity>,
) -> (String, String) {
    let did_a = identity_a.did().to_string();
    let did_b = identity_b.did().to_string();
    let (peer_a, addr_a) = wait_for_peer_identity(db_a.node.as_ref()).await;
    let (peer_b, addr_b) = wait_for_peer_identity(db_b.node.as_ref()).await;
    authorize_enrollment_peer(
        db_a.node.clone(),
        CROSS_NODE_NETWORK_ID,
        CROSS_NODE_NETWORK_NAME,
        identity_a.clone(),
        identity_b.clone(),
        &peer_b,
        &addr_b,
    )
    .await;
    authorize_enrollment_peer(
        db_b.node.clone(),
        CROSS_NODE_NETWORK_ID,
        CROSS_NODE_NETWORK_NAME,
        identity_b.clone(),
        identity_a.clone(),
        &peer_a,
        &addr_a,
    )
    .await;
    write_data_plane_pairing(
        db_a.node.as_ref(),
        &peer_b,
        &did_a,
        &addr_b,
        AGENT_TARGET_CALLER_TEMPLATE,
    )
    .await;
    write_data_plane_pairing(
        db_b.node.as_ref(),
        &peer_a,
        &did_b,
        &addr_a,
        AGENT_TARGET_HOST_TEMPLATE,
    )
    .await;
    wait_for_session_message_routes(db_a, &peer_b, db_b, &peer_a).await;
    (peer_a, peer_b)
}

async fn wait_for_session_message_routes(db_a: &TestDb, peer_b: &str, db_b: &TestDb, peer_a: &str) {
    wait_for_pairing_applied(
        db_a.node.as_ref(),
        peer_b,
        "AgentRequest",
        Duration::from_secs(120),
    )
    .await;
    wait_for_pairing_applied(
        db_b.node.as_ref(),
        peer_a,
        "AgentOutputSegment",
        Duration::from_secs(120),
    )
    .await;
}

/// Author the local data-plane layer for one enrolled peer. Enrollment remains
/// the transport and identity gate; this row only selects the scope template.
async fn write_data_plane_pairing(
    node: &EmbeddedNode,
    peer_id: &str,
    self_did: &str,
    peer_addr: &str,
    template: &str,
) {
    let collections = resolve_template(template)
        .unwrap_or_else(|| panic!("template {template} should resolve"))
        .collections
        .iter()
        .map(|collection| format!("\"{}\"", escape_graphql_string(collection)))
        .collect::<Vec<_>>()
        .join(", ");
    let peer_id = escape_graphql_string(peer_id);
    let self_did = escape_graphql_string(self_did);
    let template = escape_graphql_string(template);
    let peer_addr = escape_graphql_string(peer_addr);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_DataPlanePairingDesired(input: {{
                peer_id: "{peer_id}",
                node_did: "{self_did}",
                collections: [{collections}],
                replicator_addresses: ["{peer_addr}"],
                template: "{template}",
                source: "test-session-message",
                created_at: "{now}",
                updated_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create DataPlanePairingDesired failed: {:?}",
        resp.errors
    );
}

/// Wait until the applied route for `peer_id` has an address and a replicator
/// filter for `collection`.
async fn wait_for_pairing_applied(
    node: &EmbeddedNode,
    peer_id: &str,
    collection: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    let escaped_peer_id = escape_graphql_string(peer_id);
    let query = format!(
        r#"{{
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{escaped_peer_id}" }} }}, limit: 1) {{
                peer_id
                collections
                replicator_addresses
                replicator_filter
            }}
        }}"#
    );
    let mut last = String::from("<none>");
    loop {
        let response = node.execute(&query).await;
        if let Some(row) = first_optional_row::<serde_json::Value>(&response, "PeerPairingApplied")
        {
            last = row.to_string();
            let addressed = row
                .get("replicator_addresses")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|addresses| {
                    addresses
                        .iter()
                        .any(|address| address.as_str().is_some_and(|s| !s.trim().is_empty()))
                });
            let filtered = row
                .get("replicator_filter")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .is_some_and(|filter| filter.get(collection).is_some());
            if addressed && filtered {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for PeerPairingApplied({peer_id}) to route {collection}; last row={last}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
