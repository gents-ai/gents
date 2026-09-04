//! Live end-to-end subagent delegation tests against an OpenAI-compatible
//! vLLM backend. These exercise the #377 subagent enablement + convergence
//! with REAL inference: an orchestrator agent, driven by the live model,
//! delegates work to a subagent behavior via `spawn_subagent`, the subagent
//! runs (live model), and the result flows back to the orchestrator.
//!
//! Normal test runs skip these (they are `#[ignore]`-gated AND early-return
//! unless `GENTS_LIVE_SUBAGENT=1`). To run locally:
//!
//! ```bash
//! GENTS_LIVE_SUBAGENT=1 \
//!   cargo test -p gents --test subagent_delegation_live -- --ignored --nocapture
//! ```
//!
//! Endpoint/model are overridable:
//! - `GENTS_LIVE_SUBAGENT_ENDPOINT` (default `http://100.73.235.38:8000/v1`)
//! - `GENTS_LIVE_SUBAGENT_MODEL` (default `d4f`)
//!
//! The #937 standard-path backgrounding test has its own gate and defaults to
//! the GLM-5.2 deployment on workstation-1:
//!
//! ```bash
//! GENTS_LIVE_BACKGROUNDING=1 \
//!   cargo test -p gents --test e2e_live \
//!   live_standard_backgrounding_uses_real_inference -- --ignored --nocapture
//! ```
//!
//! - `GENTS_LIVE_BACKGROUNDING_ENDPOINT` (default `http://100.73.235.38:8000/v1`)
//! - `GENTS_LIVE_BACKGROUNDING_MODEL` (default `GLM-5.2`)
//!
//! ## Cross-node delegation (Test 3)
//!
//! `live_cross_node_subagent_delegation` exercises default-on-A delegating to
//! fast-worker-on-B, which delegates again to reviewer-on-B, over REAL
//! in-process P2P replication installed by declarative `PeerPairingDesired`
//! rows — no test "pump". It then restarts both runtimes so the pairing
//! reconcilers reconnect and proves the canonical graph converges without
//! duplicate edges.
//!
//! Subagent targets are named `(agent_did, behavior_id)` pairs. The orchestrator
//! on A writes a targeted bridge; B's SubagentSource materializes the child
//! `AgentRequest` locally with `agent_did = DID-B` and `requester_did = DID-A`.
//! The terminal child request + its response replicate back only to A. The full
//! assertions (bridge on A -> child materialized + run on B with a non-empty
//! live answer -> terminal replicated back to A) all run live.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    agent::p2p_reconcile::resolve_template, default_behavior_id_for_agent,
    default_inference_profile_id_for_behavior, ensure_agent_principal, load_agent_behavior,
    resolve_descendant_graph, upsert_agent_behavior, upsert_tool_selection, AgentBehaviorDocument,
    AgentIdentity, DescendantGraphAccess, DescendantMaterializationState, DescendantPage,
    DescendantQuery, DocumentRuntimeOptions, Gents, SubagentTarget, ToolCeiling,
    ToolSelectionDocument,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::{first_optional_row, test_db, test_p2p_db, TestDb};

const DEFAULT_LIVE_ENDPOINT: &str = "http://100.73.235.38:8000/v1";
const DEFAULT_LIVE_MODEL: &str = "d4f";
const DEFAULT_BACKGROUNDING_MODEL: &str = "GLM-5.2";
const LIVE_BACKEND_ID: &str = "backend-live-subagent";
const RESEARCHER_BEHAVIOR_ID: &str = "live-researcher";
const FAST_WORKER_BEHAVIOR_ID: &str = "live-fast-worker";
const REVIEWER_BEHAVIOR_ID: &str = "live-reviewer";
const BACKGROUND_WORKER_BEHAVIOR_ID: &str = "live-background-worker";
/// Friendly, model-facing subagent target name (the model never sees behavior ids).
const RESEARCHER_TARGET_NAME: &str = "researcher";
const FAST_WORKER_TARGET_NAME: &str = "fast-worker";
const REVIEWER_TARGET_NAME: &str = "reviewer";
const BACKGROUND_WORKER_TARGET_NAME: &str = "background-worker";

fn live_enabled() -> bool {
    std::env::var("GENTS_LIVE_SUBAGENT").as_deref() == Ok("1")
}

fn live_endpoint() -> String {
    std::env::var("GENTS_LIVE_SUBAGENT_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_LIVE_ENDPOINT.to_string())
}

fn live_model() -> String {
    std::env::var("GENTS_LIVE_SUBAGENT_MODEL").unwrap_or_else(|_| DEFAULT_LIVE_MODEL.to_string())
}

fn backgrounding_live_enabled() -> bool {
    std::env::var("GENTS_LIVE_BACKGROUNDING").as_deref() == Ok("1")
}

fn backgrounding_live_endpoint() -> String {
    std::env::var("GENTS_LIVE_BACKGROUNDING_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_LIVE_ENDPOINT.to_string())
}

fn backgrounding_live_model() -> String {
    std::env::var("GENTS_LIVE_BACKGROUNDING_MODEL")
        .unwrap_or_else(|_| DEFAULT_BACKGROUNDING_MODEL.to_string())
}

// ---------------------------------------------------------------------------
// Test 1: local delegation (orchestrator + subagent on one node / one DID)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SUBAGENT=1 and pass --ignored"]
async fn live_local_subagent_delegation() -> Result<()> {
    if !live_enabled() {
        eprintln!("GENTS_LIVE_SUBAGENT is not 1; skipping live local subagent delegation");
        return Ok(());
    }

    let endpoint = live_endpoint();
    let model = live_model();
    assert_endpoint_reachable(&endpoint).await;

    let db = test_db("subagent-live-local").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("subagent-live-local"));
    let agent_did = identity.did().to_string();
    let orchestrator_behavior_id = default_behavior_id_for_agent(&agent_did);

    // Ensure the principal + default (orchestrator) behavior documents exist.
    // This also creates the default inference profile we reuse for both behaviors.
    ensure_agent_principal(db.node.as_ref(), &agent_did)
        .await
        .expect("ensure principal");
    let profile_id = default_inference_profile_id_for_behavior(&orchestrator_behavior_id);
    upsert_live_backend(db.node.as_ref(), &endpoint, &model).await;

    // Orchestrator behavior: system prompt instructs delegation to the
    // researcher subagent (foreground is fine locally).
    configure_behavior(
        db.node.as_ref(),
        &orchestrator_behavior_id,
        &agent_did,
        &model,
        &profile_id,
        ORCHESTRATOR_SYSTEM_PROMPT,
        None,
    )
    .await;

    // Subagent behavior (same DID => local target).
    configure_behavior(
        db.node.as_ref(),
        RESEARCHER_BEHAVIOR_ID,
        &agent_did,
        &model,
        &profile_id,
        "You answer the user's question concisely and factually in one short sentence.",
        Some("Researches factual questions and returns a concise factual answer."),
    )
    .await;

    // Enable subagent spawning on the orchestrator behavior, targeting the
    // researcher. Foreground enabled (local), background also enabled.
    authorize_subagents(
        db.node.as_ref(),
        &agent_did,
        &orchestrator_behavior_id,
        vec![SubagentTarget {
            name: RESEARCHER_TARGET_NAME.to_string(),
            agent_did: agent_did.clone(),
            behavior_id: RESEARCHER_BEHAVIOR_ID.to_string(),
            description: Some("Researches factual questions.".to_string()),
        }],
        /* spawn */ true,
        /* background */ true,
        // Local (same-DID) target: cross-deployment stays off (default).
        /* allow_cross_deployment */
        false,
    )
    .await;

    let agent = boot_document_agent(&db, identity).await?;

    let request_id = "req-live-local-delegation";
    let session_id = "session-live-local-delegation";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &orchestrator_behavior_id,
        request_id,
        session_id,
        "Use your research subagent to find the capital of France, then tell me the answer.",
    )
    .await;

    // Wait for the orchestrator request to reach a terminal state.
    let parent_terminal =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(120)).await;
    eprintln!("[live-local] orchestrator parent terminal state = {parent_terminal}");

    // A child AgentRequest must exist with subagent lineage to the parent.
    let child = match wait_for_child_of_parent(
        db.node.as_ref(),
        request_id,
        Duration::from_secs(120),
    )
    .await
    {
        Some(child) => child,
        None => {
            dump_session_diagnostics(db.node.as_ref(), session_id).await;
            panic!("child subagent AgentRequest must be materialized");
        }
    };
    eprintln!(
        "[live-local] child request_id={} behavior_id={} lifecycle_state={:?} caused_by_trigger_kind={:?}",
        child.request_id, child.behavior_id, child.lifecycle_state, child.caused_by_trigger_kind
    );

    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(request_id),
        "child must be linked to the orchestrator request"
    );
    assert_eq!(
        child.caused_by_trigger_kind.as_deref(),
        Some("subagent"),
        "child lineage kind must be subagent"
    );
    assert_eq!(
        child.behavior_id, RESEARCHER_BEHAVIOR_ID,
        "child must run the researcher behavior the model delegated to"
    );

    // Child must reach a terminal/completed state.
    let child_terminal = wait_for_request_terminal(
        db.node.as_ref(),
        &child.request_id,
        Duration::from_secs(120),
    )
    .await;
    eprintln!("[live-local] child terminal state = {child_terminal}");
    assert_eq!(
        child_terminal, "completed",
        "child subagent request must complete; got {child_terminal}"
    );

    // Child must produce a non-empty assistant response.
    let child_answer =
        wait_for_assistant_answer(db.node.as_ref(), &child.request_id, Duration::from_secs(30))
            .await;
    eprintln!("[live-local] child assistant answer = {child_answer:?}");
    assert!(
        !child_answer.trim().is_empty(),
        "child subagent must produce a non-empty assistant response"
    );

    // Soft check: ideally the answer mentions Paris. Don't hard-fail on model wording.
    if child_answer.to_lowercase().contains("paris") {
        eprintln!("[live-local] SOFT-OK: child answer contains 'Paris'");
    } else {
        eprintln!("[live-local] SOFT-WARN: child answer did not contain 'Paris': {child_answer:?}");
    }

    // The orchestrator should have terminalized (any terminal is acceptable; we
    // care most about the delegation structure + child completion above).
    assert!(
        is_terminal(&parent_terminal),
        "orchestrator request must reach a terminal state; got {parent_terminal}"
    );

    agent.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test 2: standard background paths with real inference (#937)
// ---------------------------------------------------------------------------

/// Exercise both background-work lanes through the production owned loop:
///
/// 1. GLM-5.2 chooses `spawn_subagent`; the configured default await mode makes
///    the child background without an `await_mode` argument.
/// 2. GLM-5.2 chooses `spawn_process` for `bash_unrestricted`.
/// 3. The resolved model-facing surface contains every spawn/list/read/wait/
///    cancel tool for both lanes.
/// 4. In the fire-and-continue lanes the initial parent request completes while
///    durable work is still running; releasing it produces the completion
///    notification and a real-inference scheduled wake.
/// 5. In the managed lanes GLM-5.2 launches deliberately blocked work, lists
///    it, reads partial output/transcript while it is running, waits for it,
///    reads the terminal result, and reports the observed markers.
///
/// Release files make the non-blocking assertion deterministic: background
/// work cannot finish until this test has observed the parent return.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_BACKGROUNDING=1 and pass --ignored"]
async fn live_standard_backgrounding_uses_real_inference() -> Result<()> {
    if !backgrounding_live_enabled() {
        return Ok(());
    }

    let endpoint = backgrounding_live_endpoint();
    let model = backgrounding_live_model();
    assert_model_available(&endpoint, &model).await;

    let workspace = tempfile::tempdir().expect("backgrounding live workspace");
    let child_release = workspace.path().join("release-child");
    let tool_release = workspace.path().join("release-tool");
    let managed_child_release = workspace.path().join("release-managed-child");
    let managed_tool_release = workspace.path().join("release-managed-tool");
    let child_command = format!(
        "printf CHILD_BACKGROUND_STARTED; while [ ! -f '{}' ]; do sleep 0.2; done; printf CHILD_BACKGROUND_DONE",
        child_release.display()
    );
    let native_tool_command = format!(
        "printf NATIVE_BACKGROUND_STARTED; while [ ! -f '{}' ]; do sleep 0.2; done; printf NATIVE_BACKGROUND_DONE",
        tool_release.display()
    );
    let managed_child_command = format!(
        "printf CHILD_MANAGED_STARTED; while [ ! -f '{}' ]; do sleep 0.2; done; printf CHILD_MANAGED_DONE",
        managed_child_release.display()
    );
    let managed_tool_command = format!(
        "printf NATIVE_MANAGED_STARTED; while [ ! -f '{}' ]; do sleep 0.2; done; printf NATIVE_MANAGED_DONE",
        managed_tool_release.display()
    );
    let child_tool_args = serde_json::json!({
        "command": child_command,
        "args": [],
        "timeout_secs": 180
    });
    let native_tool_args = serde_json::json!({
        "command": native_tool_command,
        "args": [],
        "timeout_secs": 180
    });
    let managed_child_tool_args = serde_json::json!({
        "command": managed_child_command,
        "args": [],
        "timeout_secs": 180
    });
    let managed_native_tool_args = serde_json::json!({
        "command": managed_tool_command,
        "args": [],
        "timeout_secs": 180
    });

    let parent_system_prompt = format!(
        r#"You are the deterministic orchestrator in an integration test.

Apply these rules to the LATEST request:
- If it is exactly RUN_BACKGROUND_AGENT, call spawn_subagent exactly once with name "background-worker" and prompt exactly "RUN_CHILD_BACKGROUND_JOB". Omit await_mode so the configured default is exercised. As soon as the tool returns its running receipt, do not call wait_subagent, read_subagent, list_subagents, cancel_subagent, or any other tool. Reply exactly PARENT_RETURNED_AGENT_BACKGROUND.
- If it is exactly RUN_BACKGROUND_TOOL, call spawn_process exactly once with tool_name "bash_unrestricted" and args exactly {native_tool_args}. As soon as the tool returns its running receipt, do not call wait_process, read_tool_output, list_processes, cancel_process, bash_unrestricted, or any other tool. Reply exactly PARENT_RETURNED_TOOL_BACKGROUND.
- If it is exactly MANAGE_BACKGROUND_AGENT, call exactly one tool per turn and wait for its result before choosing the next tool. First call spawn_subagent exactly once with name "background-worker" and prompt exactly "RUN_MANAGED_CHILD_BACKGROUND_JOB", omitting await_mode. Then call list_subagents. Then call read_subagent for that child with include_user_messages and include_tool_results true. You MUST inspect the non-terminal transcript before calling wait_subagent. Then call wait_subagent. After it completes, call read_subagent once more for the terminal transcript. Only then reply exactly AGENT_BACKGROUND_REPORT CHILD_MANAGED_STARTED CHILD_MANAGED_DONE.
- If it is exactly MANAGE_BACKGROUND_TOOL, call exactly one tool per turn and wait for its result before choosing the next tool. First call spawn_process exactly once with tool_name "bash_unrestricted" and args exactly {managed_native_tool_args}. Then call list_processes. Then call read_process for that process at offset 0. You MUST inspect output containing NATIVE_MANAGED_STARTED with exited false before calling wait_process. Then call wait_process. After it completes, call read_process once more at offset 0. Only then reply exactly TOOL_BACKGROUND_REPORT NATIVE_MANAGED_STARTED NATIVE_MANAGED_DONE.
- If the latest request asks you to review pending background completion notifications, never repeat either spawn. Reply exactly BACKGROUND_COMPLETION_OBSERVED.

Never call bash_unrestricted directly from this behavior."#
    );
    let child_system_prompt = format!(
        r#"You are the deterministic background worker in an integration test.
When the latest request is exactly RUN_CHILD_BACKGROUND_JOB, call bash_unrestricted exactly once with these arguments: {child_tool_args}
Wait for that foreground tool call to finish, then reply exactly CHILD_BACKGROUND_DONE. Do not call any other tool.
When the latest request is exactly RUN_MANAGED_CHILD_BACKGROUND_JOB, call bash_unrestricted exactly once with these arguments: {managed_child_tool_args}
Wait for that foreground tool call to finish, then reply exactly CHILD_MANAGED_STARTED CHILD_MANAGED_DONE. Do not call any other tool."#
    );

    let db = test_db("backgrounding-live-standard-path").await;
    let identity: Arc<dyn AgentIdentity> =
        Arc::new(test_identity("backgrounding-live-standard-path"));
    let agent_did = identity.did().to_string();
    let orchestrator_behavior_id = default_behavior_id_for_agent(&agent_did);
    ensure_agent_principal(db.node.as_ref(), &agent_did)
        .await
        .expect("ensure backgrounding principal");
    let profile_id = default_inference_profile_id_for_behavior(&orchestrator_behavior_id);
    upsert_live_backend(db.node.as_ref(), &endpoint, &model).await;
    configure_behavior(
        db.node.as_ref(),
        &orchestrator_behavior_id,
        &agent_did,
        &model,
        &profile_id,
        &parent_system_prompt,
        None,
    )
    .await;
    configure_behavior(
        db.node.as_ref(),
        BACKGROUND_WORKER_BEHAVIOR_ID,
        &agent_did,
        &model,
        &profile_id,
        &child_system_prompt,
        Some("Runs a deliberately blocked background integration-test job."),
    )
    .await;
    configure_standard_backgrounding_tools(
        db.node.as_ref(),
        &agent_did,
        &orchestrator_behavior_id,
        workspace.path(),
    )
    .await;

    let loaded_agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(workspace.path()).with_command_timeout_secs(180),
            ..Default::default()
        },
    )
    .await?;
    assert_standard_backgrounding_tool_surfaces(
        &loaded_agent,
        &agent_did,
        &orchestrator_behavior_id,
    );
    let agent = boot_loaded_document_agent(&db, loaded_agent).await;

    // Lane 1: the model invokes spawn_subagent without await_mode. The
    // ToolSelection default must make the standard path background.
    let agent_request_id = "req-live-standard-background-agent";
    let agent_session_id = "session-live-standard-background-agent";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &orchestrator_behavior_id,
        agent_request_id,
        agent_session_id,
        "RUN_BACKGROUND_AGENT",
    )
    .await;

    let bridge =
        wait_for_subagent_bridge(db.node.as_ref(), agent_session_id, Duration::from_secs(180))
            .await
            .unwrap_or_else(|| panic!("live model did not create a spawn_subagent bridge"));
    assert_eq!(
        bridge.await_mode.as_deref(),
        Some("background"),
        "configured default must persist the spawn as background"
    );
    let bridge_args: serde_json::Value =
        serde_json::from_str(bridge.args.as_deref().expect("bridge args"))
            .expect("valid args JSON");
    assert!(
        bridge_args.get("await_mode").is_none(),
        "the model must omit await_mode so this test exercises the configured standard path; args={bridge_args}"
    );
    let child_request_id = bridge
        .child_request_id
        .clone()
        .expect("background bridge child request id");

    let parent_state =
        wait_for_request_terminal(db.node.as_ref(), agent_request_id, Duration::from_secs(180))
            .await;
    assert_eq!(parent_state, "completed");
    let parent_answer =
        wait_for_assistant_answer(db.node.as_ref(), agent_request_id, Duration::from_secs(30))
            .await;
    assert!(
        parent_answer.contains("PARENT_RETURNED_AGENT_BACKGROUND"),
        "parent did not acknowledge the background receipt: {parent_answer:?}"
    );
    let child_state = fetch_request_lifecycle(db.node.as_ref(), &child_request_id)
        .await
        .expect("child lifecycle after parent completion");
    assert!(
        !is_terminal(&child_state),
        "parent blocked on the background child; child was already {child_state}"
    );
    let running_bridge = fetch_subagent_bridge(db.node.as_ref(), agent_session_id)
        .await
        .expect("bridge after parent completion");
    assert_eq!(
        running_bridge.lifecycle_state, "running",
        "background bridge must remain running until child completion"
    );
    assert_no_tool_call(
        db.node.as_ref(),
        agent_session_id,
        &["wait_subagent", "cancel_subagent"],
    )
    .await;
    assert_min_completed_inference_calls(db.node.as_ref(), agent_request_id, 2).await;

    std::fs::write(&child_release, b"release").expect("release background child");
    let child_terminal = wait_for_request_terminal(
        db.node.as_ref(),
        &child_request_id,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(child_terminal, "completed");
    assert_min_completed_inference_calls(db.node.as_ref(), &child_request_id, 2).await;
    let completed_bridge = wait_for_tool_call_state(
        db.node.as_ref(),
        agent_session_id,
        &bridge.tool_call_id,
        "completed",
        Duration::from_secs(60),
    )
    .await;
    assert!(
        completed_bridge
            .result
            .as_deref()
            .is_some_and(|result| result.contains("CHILD_BACKGROUND_DONE")),
        "bridge did not receive the child result: {:?}",
        completed_bridge.result
    );
    wait_for_message_containing(
        db.node.as_ref(),
        agent_session_id,
        &format!(r#"<subagent-notification child_request_id="{child_request_id}""#),
        Duration::from_secs(60),
    )
    .await;
    let agent_wake = wait_for_background_wake(
        db.node.as_ref(),
        agent_session_id,
        agent_request_id,
        Duration::from_secs(60),
    )
    .await;
    let agent_wake_state = wait_for_request_terminal(
        db.node.as_ref(),
        &agent_wake.request_id,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(agent_wake_state, "completed");
    assert_min_completed_inference_calls(db.node.as_ref(), &agent_wake.request_id, 1).await;
    let agent_wake_answer = wait_for_assistant_answer(
        db.node.as_ref(),
        &agent_wake.request_id,
        Duration::from_secs(30),
    )
    .await;
    assert!(
        agent_wake_answer.contains("BACKGROUND_COMPLETION_OBSERVED"),
        "real-inference wake did not process the subagent notification: {agent_wake_answer:?}"
    );

    // Lane 2: the model invokes spawn_process, which creates a childless
    // background AgentToolCall for the actual bash tool.
    let tool_request_id = "req-live-standard-background-tool";
    let tool_session_id = "session-live-standard-background-tool";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &orchestrator_behavior_id,
        tool_request_id,
        tool_session_id,
        "RUN_BACKGROUND_TOOL",
    )
    .await;

    let background_tool = wait_for_background_tool_call(
        db.node.as_ref(),
        tool_session_id,
        "bash_unrestricted",
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(background_tool.await_mode.as_deref(), Some("background"));
    assert!(
        background_tool.child_request_id.is_none(),
        "native background tool must use the childless lane"
    );
    let persisted_tool_args: serde_json::Value = serde_json::from_str(
        background_tool
            .args
            .as_deref()
            .expect("native background tool args"),
    )
    .expect("valid native background args");
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
        db.node.as_ref(),
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
        db.node.as_ref(),
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
        db.node.as_ref(),
        tool_session_id,
        &format!(
            r#"<tool-completion tool_call_id="{}""#,
            background_tool.tool_call_id
        ),
        Duration::from_secs(60),
    )
    .await;
    let tool_wake = wait_for_background_wake(
        db.node.as_ref(),
        tool_session_id,
        tool_request_id,
        Duration::from_secs(60),
    )
    .await;
    let tool_wake_state = wait_for_request_terminal(
        db.node.as_ref(),
        &tool_wake.request_id,
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(tool_wake_state, "completed");
    assert_min_completed_inference_calls(db.node.as_ref(), &tool_wake.request_id, 1).await;
    let tool_wake_answer = wait_for_assistant_answer(
        db.node.as_ref(),
        &tool_wake.request_id,
        Duration::from_secs(30),
    )
    .await;
    assert!(
        tool_wake_answer.contains("BACKGROUND_COMPLETION_OBSERVED"),
        "real-inference wake did not process the tool notification: {tool_wake_answer:?}"
    );

    // Lane 3: a single real-inference request manages a local background
    // subagent end to end. The model must inspect the live child before this
    // test releases its blocked foreground command, then wait and inspect the
    // terminal transcript before reporting the markers.
    let managed_agent_request_id = "req-live-managed-background-agent";
    let managed_agent_session_id = "session-live-managed-background-agent";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &orchestrator_behavior_id,
        managed_agent_request_id,
        managed_agent_session_id,
        "MANAGE_BACKGROUND_AGENT",
    )
    .await;

    let managed_bridge = wait_for_subagent_bridge(
        db.node.as_ref(),
        managed_agent_session_id,
        Duration::from_secs(180),
    )
    .await
    .unwrap_or_else(|| panic!("live model did not create the managed background subagent"));
    assert_eq!(managed_bridge.await_mode.as_deref(), Some("background"));
    let managed_child_request_id = managed_bridge
        .child_request_id
        .as_deref()
        .expect("managed child request id");
    wait_for_model_tool_call(
        db.node.as_ref(),
        managed_agent_session_id,
        "read_subagent",
        Duration::from_secs(180),
    )
    .await;
    let managed_child_live_state =
        fetch_request_lifecycle(db.node.as_ref(), managed_child_request_id)
            .await
            .expect("managed child lifecycle during read_subagent");
    assert!(
        !is_terminal(&managed_child_live_state),
        "read_subagent was not exercised against a live child; state={managed_child_live_state}"
    );
    // The assistant wait envelope is durably snapshotted before the hook
    // blocks. Observe that exact boundary, then prove the child is still live
    // before releasing it.
    wait_for_model_tool_call(
        db.node.as_ref(),
        managed_agent_session_id,
        "wait_subagent",
        Duration::from_secs(180),
    )
    .await;
    let managed_child_wait_state =
        fetch_request_lifecycle(db.node.as_ref(), managed_child_request_id)
            .await
            .expect("managed child lifecycle during wait_subagent");
    assert!(
        !is_terminal(&managed_child_wait_state),
        "wait_subagent must begin while the child is live; state={managed_child_wait_state}"
    );
    std::fs::write(&managed_child_release, b"release").expect("release managed background child");

    let managed_agent_state = wait_for_request_terminal(
        db.node.as_ref(),
        managed_agent_request_id,
        Duration::from_secs(240),
    )
    .await;
    assert_eq!(managed_agent_state, "completed");
    let managed_agent_answer = wait_for_assistant_answer(
        db.node.as_ref(),
        managed_agent_request_id,
        Duration::from_secs(30),
    )
    .await;
    assert!(
        managed_agent_answer
            .contains("AGENT_BACKGROUND_REPORT CHILD_MANAGED_STARTED CHILD_MANAGED_DONE"),
        "model did not report the inspected background-agent result: {managed_agent_answer:?}"
    );
    assert_model_tool_call_count_at_least(
        db.node.as_ref(),
        managed_agent_session_id,
        "spawn_subagent",
        1,
    )
    .await;
    assert_model_tool_call_count_at_least(
        db.node.as_ref(),
        managed_agent_session_id,
        "list_subagents",
        1,
    )
    .await;
    assert_model_tool_call_count_at_least(
        db.node.as_ref(),
        managed_agent_session_id,
        "read_subagent",
        2,
    )
    .await;
    assert_model_tool_call_count_at_least(
        db.node.as_ref(),
        managed_agent_session_id,
        "wait_subagent",
        1,
    )
    .await;

    // Lane 4: the same acceptance flow for a native background process. The
    // release is withheld until the model's read_process result contains the
    // live STARTED marker, proving it read actual output before wait_process.
    let managed_tool_request_id = "req-live-managed-background-tool";
    let managed_tool_session_id = "session-live-managed-background-tool";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &orchestrator_behavior_id,
        managed_tool_request_id,
        managed_tool_session_id,
        "MANAGE_BACKGROUND_TOOL",
    )
    .await;

    let managed_tool = wait_for_background_tool_call(
        db.node.as_ref(),
        managed_tool_session_id,
        "bash_unrestricted",
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(managed_tool.await_mode.as_deref(), Some("background"));
    wait_for_model_tool_call(
        db.node.as_ref(),
        managed_tool_session_id,
        "read_process",
        Duration::from_secs(180),
    )
    .await;
    wait_for_tool_result_containing(
        db.node.as_ref(),
        managed_tool_session_id,
        "NATIVE_MANAGED_STARTED",
        Duration::from_secs(60),
    )
    .await;
    // Observe the durably snapshotted wait call while it is blocked, then prove
    // the native process is still live before releasing it.
    wait_for_model_tool_call(
        db.node.as_ref(),
        managed_tool_session_id,
        "wait_process",
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(
        fetch_tool_call(
            db.node.as_ref(),
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

    let managed_tool_state = wait_for_request_terminal(
        db.node.as_ref(),
        managed_tool_request_id,
        Duration::from_secs(240),
    )
    .await;
    assert_eq!(managed_tool_state, "completed");
    let managed_tool_answer = wait_for_assistant_answer(
        db.node.as_ref(),
        managed_tool_request_id,
        Duration::from_secs(30),
    )
    .await;
    assert!(
        managed_tool_answer
            .contains("TOOL_BACKGROUND_REPORT NATIVE_MANAGED_STARTED NATIVE_MANAGED_DONE"),
        "model did not report the inspected native background result: {managed_tool_answer:?}"
    );
    assert_model_tool_call_count_at_least(
        db.node.as_ref(),
        managed_tool_session_id,
        "spawn_process",
        1,
    )
    .await;
    assert_model_tool_call_count_at_least(
        db.node.as_ref(),
        managed_tool_session_id,
        "list_processes",
        1,
    )
    .await;
    assert_model_tool_call_count_at_least(
        db.node.as_ref(),
        managed_tool_session_id,
        "read_process",
        2,
    )
    .await;
    assert_model_tool_call_count_at_least(
        db.node.as_ref(),
        managed_tool_session_id,
        "wait_process",
        1,
    )
    .await;

    agent.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test 3: cross-node delegation (orchestrator on A -> behavior on B)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set GENTS_LIVE_SUBAGENT=1 and pass --ignored"]
async fn live_cross_node_subagent_delegation() -> Result<()> {
    if !live_enabled() {
        eprintln!("GENTS_LIVE_SUBAGENT is not 1; skipping live cross-node subagent delegation");
        return Ok(());
    }

    let endpoint = live_endpoint();
    let model = live_model();
    assert_endpoint_reachable(&endpoint).await;

    // Two REAL P2P-enabled nodes, two distinct DIDs.
    let db_a = test_p2p_db("subagent-live-a").await;
    let db_b = test_p2p_db("subagent-live-b").await;
    let identity_a: Arc<dyn AgentIdentity> = Arc::new(test_identity("subagent-live-a"));
    let identity_b: Arc<dyn AgentIdentity> = Arc::new(test_identity("subagent-live-b"));
    let did_a = identity_a.did().to_string();
    let did_b = identity_b.did().to_string();
    let orchestrator_behavior_id = default_behavior_id_for_agent(&did_a);

    // --- Node B: host the fast-worker and reviewer behaviors owned by DID-B. ---
    ensure_agent_principal(db_b.node.as_ref(), &did_b)
        .await
        .expect("ensure principal B");
    let profile_b =
        default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(&did_b));
    upsert_live_backend(db_b.node.as_ref(), &endpoint, &model).await;
    configure_behavior(
        db_b.node.as_ref(),
        FAST_WORKER_BEHAVIOR_ID,
        &did_b,
        &model,
        &profile_b,
        CROSS_NODE_FAST_WORKER_SYSTEM_PROMPT,
        Some("Delegates its draft to the reviewer before returning it."),
    )
    .await;
    configure_behavior(
        db_b.node.as_ref(),
        REVIEWER_BEHAVIOR_ID,
        &did_b,
        &model,
        &profile_b,
        "When asked to review the capital of France, reply exactly REVIEWER_OK: Paris is the capital of France.",
        Some("Reviews the fast worker's factual answer."),
    )
    .await;
    authorize_subagents(
        db_b.node.as_ref(),
        &did_b,
        FAST_WORKER_BEHAVIOR_ID,
        vec![SubagentTarget {
            name: REVIEWER_TARGET_NAME.to_string(),
            agent_did: did_b.clone(),
            behavior_id: REVIEWER_BEHAVIOR_ID.to_string(),
            description: Some("Reviews the fast worker's answer.".to_string()),
        }],
        /* spawn */ true,
        /* background */ true,
        /* allow_cross_deployment */ false,
    )
    .await;

    // --- Node A: host the orchestrator owned by DID-A. ---
    ensure_agent_principal(db_a.node.as_ref(), &did_a)
        .await
        .expect("ensure principal A");
    let profile_a = default_inference_profile_id_for_behavior(&orchestrator_behavior_id);
    upsert_live_backend(db_a.node.as_ref(), &endpoint, &model).await;
    configure_behavior(
        db_a.node.as_ref(),
        &orchestrator_behavior_id,
        &did_a,
        &model,
        &profile_a,
        CROSS_NODE_NESTED_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
    )
    .await;

    // The named (agent_did, behavior_id) target is owned by DID-B: a REMOTE
    // delegation target. A authors only the targeted bridge; B resolves no
    // friendly name and materializes the child locally from the resolved args,
    // so we do NOT mirror B's AgentBehavior onto A.
    authorize_subagents(
        db_a.node.as_ref(),
        &did_a,
        &orchestrator_behavior_id,
        vec![SubagentTarget {
            name: FAST_WORKER_TARGET_NAME.to_string(),
            agent_did: did_b.clone(),
            behavior_id: FAST_WORKER_BEHAVIOR_ID.to_string(),
            description: Some("Produces a reviewed factual answer.".to_string()),
        }],
        /* spawn */ true,
        /* background */ true,
        // Cross-deployment subagent delegation is deferred/flag-gated by default
        // pending ACP (#377). This cross-node test exercises the substrate behind
        // the opt-in flag; the REMOTE (DID-B) target requires it set true.
        /* allow_cross_deployment */
        true,
    )
    .await;

    let addr_a = wait_for_listen_addr(db_a.node.as_ref()).await;
    let addr_b = wait_for_listen_addr(db_b.node.as_ref()).await;

    // Pairing rows exist on BOTH nodes BEFORE the agents boot. The running
    // pairing reconcilers perform the actual installation: A's coordinator leg
    // carries only bridges targeted to B; B's host leg carries routed B-owned
    // child requests back to A. The rows carry the peer's real listen address,
    // never `[]`.
    write_pairing(
        db_a.node.as_ref(),
        "peer-b",
        &did_b,
        "subagent-coordinator",
        &addr_b,
    )
    .await;
    write_pairing(
        db_b.node.as_ref(),
        "peer-a",
        &did_a,
        "subagent-host",
        &addr_a,
    )
    .await;

    // Boot full agents on both nodes. B owns DID-B: its daemon + SubagentSource
    // claim and run the replicated child request against the live model.
    let agent_b = boot_document_agent(&db_b, identity_b.clone()).await?;
    let agent_a = boot_document_agent(&db_a, identity_a.clone()).await?;
    wait_for_replicator_installed(db_a.node.as_ref(), "peer-b", Duration::from_secs(120)).await;
    wait_for_replicator_installed(db_b.node.as_ref(), "peer-a", Duration::from_secs(120)).await;

    let request_id = "req-live-cross-node";
    let session_id = "session-live-cross-node";
    create_runtime_request(
        db_a.node.as_ref(),
        &did_a,
        &orchestrator_behavior_id,
        request_id,
        session_id,
        "Run the nested review workflow for the capital of France.",
    )
    .await;

    // Find the bridge tool call created on A (the spawn_subagent AgentToolCall).
    let bridge =
        match wait_for_subagent_bridge(db_a.node.as_ref(), session_id, Duration::from_secs(120))
            .await
        {
            Some(bridge) => bridge,
            None => {
                dump_session_diagnostics(db_a.node.as_ref(), session_id).await;
                let snap =
                    crate::support::snapshots::fetch_runtime_snapshot(db_a.node.as_ref(), &did_a)
                        .await;
                eprintln!("[live-cross] node A runtime snapshot = {snap:?}");
                agent_a.shutdown().await;
                agent_b.shutdown().await;
                db_a.node.shutdown().await;
                db_b.node.shutdown().await;
                panic!("orchestrator on A did not create a spawn_subagent bridge tool call");
            }
        };
    eprintln!(
        "[live-cross] bridge on A: tool_call_id={} child_request_id={:?} await_mode={:?} lifecycle_state={}",
        bridge.tool_call_id, bridge.child_request_id, bridge.await_mode, bridge.lifecycle_state
    );
    let child_request_id = bridge
        .child_request_id
        .clone()
        .expect("bridge must carry a child_request_id");

    // The child AgentRequest must be materialized on B and run there.
    let child_on_b = wait_for_request_on_node(
        db_b.node.as_ref(),
        &child_request_id,
        Duration::from_secs(120),
    )
    .await
    .expect("child AgentRequest must be materialized on node B");
    eprintln!(
        "[live-cross] child on B: request_id={} agent_did={} behavior_id={} lifecycle_state={:?}",
        child_on_b.request_id,
        child_on_b.agent_did,
        child_on_b.behavior_id,
        child_on_b.lifecycle_state
    );
    assert_eq!(
        child_on_b.agent_did, did_b,
        "child must be owned by DID-B (claimed + run by node B)"
    );
    assert_eq!(
        child_on_b.requester_did.as_deref(),
        Some(did_a.as_str()),
        "child request must route only to its DID-A coordinator"
    );
    assert_eq!(child_on_b.behavior_id, FAST_WORKER_BEHAVIOR_ID);

    // The live fast-worker must itself delegate to reviewer. This is a local
    // child on B, but requester_did remains DID-A so its bridge and result are
    // part of the same authorization-safe return projection.
    let reviewer_on_b = wait_for_child_of_parent(
        db_b.node.as_ref(),
        &child_request_id,
        Duration::from_secs(120),
    )
    .await
    .expect("fast-worker must materialize its reviewer child on node B");
    assert_eq!(reviewer_on_b.behavior_id, REVIEWER_BEHAVIOR_ID);
    assert_eq!(
        reviewer_on_b.caused_by_parent_request_id.as_deref(),
        Some(child_request_id.as_str())
    );
    let reviewer_terminal_b = wait_for_request_terminal(
        db_b.node.as_ref(),
        &reviewer_on_b.request_id,
        Duration::from_secs(150),
    )
    .await;
    assert_eq!(reviewer_terminal_b, "completed");
    let reviewer_answer_b = wait_for_assistant_answer(
        db_b.node.as_ref(),
        &reviewer_on_b.request_id,
        Duration::from_secs(30),
    )
    .await;
    assert!(
        reviewer_answer_b.contains("REVIEWER_OK"),
        "reviewer did not produce its live completion marker: {reviewer_answer_b:?}"
    );

    // B runs the child to completion with a non-empty live response.
    let child_terminal_b = wait_for_request_terminal(
        db_b.node.as_ref(),
        &child_request_id,
        Duration::from_secs(150),
    )
    .await;
    eprintln!("[live-cross] child terminal on B = {child_terminal_b}");
    assert_eq!(
        child_terminal_b, "completed",
        "child must complete on node B; got {child_terminal_b}"
    );
    let child_answer_b = wait_for_assistant_answer(
        db_b.node.as_ref(),
        &child_request_id,
        Duration::from_secs(30),
    )
    .await;
    eprintln!("[live-cross] child answer on B = {child_answer_b:?}");
    assert!(
        !child_answer_b.trim().is_empty(),
        "child must produce a non-empty live response on B"
    );
    if child_answer_b.to_lowercase().contains("paris") {
        eprintln!("[live-cross] SOFT-OK: child answer contains 'Paris'");
    } else {
        eprintln!(
            "[live-cross] SOFT-WARN: child answer did not contain 'Paris': {child_answer_b:?}"
        );
    }

    // The terminal child must replicate back to A and A must observe completion
    // (the BackgroundCompletionObserver projects it onto the parent bridge).
    let child_terminal_a = wait_for_request_terminal(
        db_a.node.as_ref(),
        &child_request_id,
        Duration::from_secs(120),
    )
    .await;
    eprintln!("[live-cross] child terminal observed on A = {child_terminal_a}");
    assert!(
        is_terminal(&child_terminal_a),
        "terminal child must replicate back to A; got {child_terminal_a}"
    );

    // The parent orchestrator request on A should terminalize (background
    // completion projection + a final orchestrator turn).
    let parent_terminal_a =
        wait_for_request_terminal(db_a.node.as_ref(), request_id, Duration::from_secs(150)).await;
    eprintln!("[live-cross] orchestrator parent terminal on A = {parent_terminal_a}");
    assert!(
        is_terminal(&parent_terminal_a),
        "orchestrator request on A must terminalize; got {parent_terminal_a}"
    );

    // #836: the model-facing and operator projections consume this same edge.
    // Its identity survives remote materialization and completion without a
    // second lineage reconstruction from child request labels.
    let graph =
        wait_for_descendant_graph(db_a.node.as_ref(), request_id, 2, Duration::from_secs(120))
            .await;
    assert_eq!(graph.edges.len(), 2, "both remote edges must converge once");
    let edge = &graph.edges[0];
    assert_eq!(edge.child_request_id, child_request_id);
    assert_eq!(edge.principal_did.as_deref(), Some(did_b.as_str()));
    assert_eq!(edge.behavior_id.as_deref(), Some(FAST_WORKER_BEHAVIOR_ID));
    assert_eq!(
        edge.materialization_state,
        DescendantMaterializationState::MaterializedRemote
    );
    assert!(edge.readable());
    assert!(edge.is_terminal());
    assert!(edge.terminal_result_ref.is_some());
    let reviewer_edge = &graph.edges[1];
    assert_eq!(reviewer_edge.child_request_id, reviewer_on_b.request_id);
    assert_eq!(reviewer_edge.immediate_parent_request_id, child_request_id);
    assert_eq!(
        reviewer_edge.behavior_id.as_deref(),
        Some(REVIEWER_BEHAVIOR_ID)
    );
    assert_eq!(reviewer_edge.depth, 2);
    assert!(reviewer_edge.readable());
    assert!(!reviewer_edge.controllable());
    assert!(reviewer_edge.is_terminal());

    // Stop and restart both runtimes. Their pairing reconcilers reconnect to
    // the already-running P2P nodes and re-project the same durable facts.
    // The graph must remain exactly-once after restart/replay convergence.
    agent_a.shutdown().await;
    agent_b.shutdown().await;
    let restarted_b = boot_document_agent(&db_b, identity_b).await?;
    let restarted_a = boot_document_agent(&db_a, identity_a).await?;
    wait_for_replicator_installed(db_a.node.as_ref(), "peer-b", Duration::from_secs(120)).await;
    wait_for_replicator_installed(db_b.node.as_ref(), "peer-a", Duration::from_secs(120)).await;
    let reconnected_graph =
        wait_for_descendant_graph(db_a.node.as_ref(), request_id, 2, Duration::from_secs(120))
            .await;
    let reconnected_ids = reconnected_graph
        .edges
        .iter()
        .map(|edge| edge.child_request_id.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(reconnected_graph.edges.len(), 2);
    assert_eq!(
        reconnected_ids.len(),
        2,
        "reconnect must not duplicate edges"
    );
    assert!(reconnected_ids.contains(child_request_id.as_str()));
    assert!(reconnected_ids.contains(reviewer_on_b.request_id.as_str()));

    restarted_a.shutdown().await;
    restarted_b.shutdown().await;
    // BootedAgent only stops Gents::run; P2P belongs to the embedded node.
    db_a.node.shutdown().await;
    db_b.node.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// System prompts
// ---------------------------------------------------------------------------

const ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are an orchestrator agent. You have a research subagent \
available named `researcher`. For ANY research or factual lookup the user asks for, you \
MUST delegate it by calling the `spawn_subagent` tool with name exactly \"researcher\" and a \
`prompt` describing the question. Do not answer factual questions yourself; always delegate them. After \
the subagent returns its answer, relay that answer to the user.";

const CROSS_NODE_NESTED_ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are the root of a deterministic nested \
delegation test. When asked to run the nested review workflow, call `spawn_subagent` exactly once with \
name exactly \"fast-worker\", await_mode exactly \"background\", and prompt exactly \
\"RESEARCH_AND_REVIEW_FRANCE\". The worker is remote, so foreground is rejected. Do not answer the \
question yourself. After its completion notification arrives, report its reviewed answer without spawning \
another worker.";

const CROSS_NODE_FAST_WORKER_SYSTEM_PROMPT: &str = "You are the fast worker in a deterministic nested \
delegation test. When the request is exactly RESEARCH_AND_REVIEW_FRANCE, call `spawn_subagent` exactly \
once with name exactly \"reviewer\", await_mode exactly \"foreground\", and prompt exactly \
\"Review this claim: Paris is the capital of France.\" After the reviewer returns, reply with its answer. \
Do not answer before the reviewer completes.";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

async fn assert_endpoint_reachable(endpoint: &str) {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    let resp = tokio::time::timeout(Duration::from_secs(20), client.get(&url).send()).await;
    match resp {
        Ok(Ok(r)) if r.status().is_success() => {}
        Ok(Ok(r)) => panic!("live endpoint {url} returned status {}", r.status()),
        Ok(Err(e)) => panic!("live endpoint {url} unreachable: {e}"),
        Err(_) => panic!("live endpoint {url} timed out (not reachable)"),
    }
}

async fn assert_model_available(endpoint: &str, model: &str) {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    let response = tokio::time::timeout(Duration::from_secs(20), client.get(&url).send())
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

/// Boot a full Gents from the behavior documents owned by `identity`'s DID.
async fn boot_document_agent(db: &TestDb, identity: Arc<dyn AgentIdentity>) -> Result<BootedAgent> {
    boot_document_agent_with_ceiling(db, identity, ToolCeiling::meta_only()).await
}

async fn boot_document_agent_with_ceiling(
    db: &TestDb,
    identity: Arc<dyn AgentIdentity>,
    tool_ceiling: ToolCeiling,
) -> Result<BootedAgent> {
    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling,
            ..Default::default()
        },
    )
    .await?;
    Ok(boot_loaded_document_agent(db, agent).await)
}

async fn boot_loaded_document_agent(db: &TestDb, agent: Gents) -> BootedAgent {
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    BootedAgent::new(shutdown_tx, handle, agent_did)
}

fn assert_standard_backgrounding_tool_surfaces(
    agent: &Gents,
    agent_did: &str,
    parent_behavior_id: &str,
) {
    let active_behavior_ids = agent
        .behaviors()
        .iter()
        .map(|behavior| behavior.behavior_id.clone())
        .collect::<HashSet<_>>();
    let parent = agent
        .behaviors()
        .iter()
        .find(|behavior| behavior.behavior_id == parent_behavior_id)
        .expect("loaded orchestrator behavior");
    let parent_surface = parent
        .tools
        .explain_with_runtime(false, agent_did, &active_behavior_ids);
    let parent_names = parent_surface
        .tool_names
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();

    for required in [
        "bash_unrestricted",
        "spawn_subagent",
        "list_subagents",
        "read_subagent",
        "wait_subagent",
        "cancel_subagent",
        "spawn_process",
        "list_processes",
        "read_process",
        "wait_process",
        "cancel_process",
    ] {
        assert!(
            parent_names.contains(required),
            "backgrounding-enabled behavior did not provision {required}; resolved={:?}",
            parent_surface.tool_names
        );
    }
    assert!(
        !parent_names.contains("steer_subagent"),
        "mutating subagent steering must remain separately gated"
    );
    assert_eq!(
        parent_surface.included.get("subagent"),
        Some(&vec![
            "cancel_subagent".to_string(),
            "list_subagents".to_string(),
            "read_subagent".to_string(),
            "spawn_subagent".to_string(),
            "wait_subagent".to_string(),
        ]),
        "background subagent enablement must resolve the complete inspection bundle"
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
        .behaviors()
        .iter()
        .find(|behavior| behavior.behavior_id == BACKGROUND_WORKER_BEHAVIOR_ID)
        .expect("loaded background worker behavior");
    let child_surface = child
        .tools
        .explain_with_runtime(false, agent_did, &active_behavior_ids);
    assert!(
        child_surface
            .tool_names
            .contains(&"bash_unrestricted".to_string()),
        "background worker must receive its foreground bash tool"
    );
    for parent_only in ["spawn_subagent", "spawn_process", "read_process"] {
        assert!(
            !child_surface.tool_names.contains(&parent_only.to_string()),
            "background worker must not inherit parent-only tool {parent_only}"
        );
    }
}

/// Upsert the live inference backend document (OpenAI-compatible vLLM).
async fn upsert_live_backend(node: &EmbeddedNode, endpoint: &str, model: &str) {
    let escaped_backend_id = escape_graphql_string(LIVE_BACKEND_ID);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let escaped_model = escape_graphql_string(model);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    api_key: "",
                    api_key_env_var: "",
                    max_concurrent: 4,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    api_key: "",
                    api_key_env_var: "",
                    max_concurrent: 4,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert live backend failed: {:?}",
        response.errors
    );
}

/// Upsert an `AgentBehavior` document backed by the live backend, with an
/// optional `description` (surfaced to the orchestrator's subagent preamble).
async fn configure_behavior(
    node: &EmbeddedNode,
    behavior_id: &str,
    agent_did: &str,
    model: &str,
    inference_profile_id: &str,
    system_prompt: &str,
    description: Option<&str>,
) {
    let mut behavior = load_agent_behavior(node, behavior_id)
        .await
        .expect("load behavior")
        .unwrap_or_else(|| AgentBehaviorDocument {
            behavior_id: behavior_id.to_string(),
            agent_did: agent_did.to_string(),
            display_name: Some(behavior_id.to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            request_context_template: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            enabled: true,
            created_at: Some("2026-06-02T00:00:00Z".to_string()),
        });
    behavior.agent_did = agent_did.to_string();
    behavior.backend_id = Some(LIVE_BACKEND_ID.to_string());
    behavior.model_name = Some(model.to_string());
    behavior.inference_profile_id = Some(inference_profile_id.to_string());
    behavior.system_prompt = Some(system_prompt.to_string());
    behavior.description = description.map(ToOwned::to_owned);
    behavior.enabled = true;
    upsert_agent_behavior(node, &behavior)
        .await
        .expect("upsert behavior");
}

/// Upsert a ToolSelectionDocument enabling subagent spawning and link it to
/// `behavior_id`.
async fn authorize_subagents(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    subagent_targets: Vec<SubagentTarget>,
    spawn_enabled: bool,
    background_enabled: bool,
    allow_cross_deployment: bool,
) {
    let selection_id = format!("{behavior_id}-subagent-tools");
    let target_entries = subagent_targets
        .iter()
        .map(SubagentTarget::to_entry)
        .collect();
    upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: agent_did.to_string(),
            subagent_targets: Some(target_entries),
            subagent_spawn_enabled: Some(spawn_enabled),
            subagent_background_enabled: Some(background_enabled),
            subagent_allow_cross_deployment: Some(allow_cross_deployment),
            // Keep the orchestrator's toolset focused on delegation so the live
            // model reliably reaches for spawn_subagent rather than defra_query.
            enable_meta_tools: Some(false),
            enable_defra_query: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("upsert tool selection");

    let mut behavior = load_agent_behavior(node, behavior_id)
        .await
        .expect("load behavior for tool-selection link")
        .expect("behavior must exist before linking tool selection");
    behavior.tool_selection_id = Some(selection_id);
    upsert_agent_behavior(node, &behavior)
        .await
        .expect("link tool selection");
}

/// Configure the parent with both standard background lanes and the child with
/// a foreground bash tool used to hold its request open until the test releases
/// it.
async fn configure_standard_backgrounding_tools(
    node: &EmbeddedNode,
    agent_did: &str,
    parent_behavior_id: &str,
    workspace: &Path,
) {
    let parent_selection_id = format!("{parent_behavior_id}-standard-background-tools");
    let parent_target = SubagentTarget {
        name: BACKGROUND_WORKER_TARGET_NAME.to_string(),
        agent_did: agent_did.to_string(),
        behavior_id: BACKGROUND_WORKER_BEHAVIOR_ID.to_string(),
        description: Some("Runs a deliberately blocked background job.".to_string()),
    };
    upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: parent_selection_id.clone(),
            agent_did: agent_did.to_string(),
            enable_bash: Some(true),
            bash_mode: Some("Unrestricted".to_string()),
            file_tool_root: Some(workspace.display().to_string()),
            backgroundable_tool_names: Some(vec!["bash_unrestricted".to_string()]),
            subagent_targets: Some(vec![parent_target.to_entry()]),
            subagent_spawn_enabled: Some(true),
            subagent_background_enabled: Some(true),
            subagent_default_await_mode: Some("background".to_string()),
            subagent_allow_cross_deployment: Some(false),
            enable_meta_tools: Some(false),
            enable_defra_query: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("upsert parent standard backgrounding selection");
    link_tool_selection(node, parent_behavior_id, &parent_selection_id).await;

    let child_selection_id = format!("{BACKGROUND_WORKER_BEHAVIOR_ID}-foreground-bash-tools");
    upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: child_selection_id.clone(),
            agent_did: agent_did.to_string(),
            enable_bash: Some(true),
            bash_mode: Some("Unrestricted".to_string()),
            file_tool_root: Some(workspace.display().to_string()),
            enable_meta_tools: Some(false),
            enable_defra_query: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("upsert child foreground bash selection");
    link_tool_selection(node, BACKGROUND_WORKER_BEHAVIOR_ID, &child_selection_id).await;
}

async fn link_tool_selection(node: &EmbeddedNode, behavior_id: &str, selection_id: &str) {
    let mut behavior = load_agent_behavior(node, behavior_id)
        .await
        .expect("load behavior for tool-selection link")
        .expect("behavior must exist before linking tool selection");
    behavior.tool_selection_id = Some(selection_id.to_string());
    upsert_agent_behavior(node, &behavior)
        .await
        .expect("link tool selection");
}

#[derive(Debug, Clone, Deserialize)]
struct ChildRow {
    request_id: String,
    behavior_id: String,
    /// Deserialized for Debug-trace output on failures; not read directly.
    #[allow(dead_code)]
    agent_did: String,
    lifecycle_state: Option<RequestLifecycleState>,
    caused_by_parent_request_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
}

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

async fn wait_for_request_terminal(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        if let Some(state) = fetch_request_lifecycle(node, request_id).await {
            last = state.clone();
            if is_terminal(&state) {
                return state;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for request {request_id} to terminalize; last={last}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn fetch_child_of_parent(node: &EmbeddedNode, parent_request_id: &str) -> Option<ChildRow> {
    let escaped = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                request_id
                behavior_id
                agent_did
                lifecycle_state
                caused_by_parent_request_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<ChildRow>(&resp, "AgentRequest")
}

async fn wait_for_child_of_parent(
    node: &EmbeddedNode,
    parent_request_id: &str,
    timeout: Duration,
) -> Option<ChildRow> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(child) = fetch_child_of_parent(node, parent_request_id).await {
            return Some(child);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_descendant_graph(
    node: &EmbeddedNode,
    root_request_id: &str,
    expected_edges: usize,
    timeout: Duration,
) -> DescendantPage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(graph) = resolve_descendant_graph(
            DescendantGraphAccess::Local(node),
            &DescendantQuery::all(root_request_id),
        )
        .await
        {
            let unique = graph
                .edges
                .iter()
                .map(|edge| edge.child_request_id.as_str())
                .collect::<HashSet<_>>();
            if graph.edges.len() == expected_edges && unique.len() == expected_edges {
                return graph;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {expected_edges} unique canonical descendant edges under {root_request_id}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Read the assistant answer for `request_id`: prefer the AgentResponse content,
/// fall back to the latest assistant AgentMessage on the request's session.
async fn fetch_assistant_answer(node: &EmbeddedNode, request_id: &str) -> String {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentResponse(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                content
                session_id
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct RespRow {
        content: Option<String>,
        session_id: Option<String>,
    }
    let resp = node.execute(&query).await;
    let row = first_optional_row::<RespRow>(&resp, "AgentResponse");
    if let Some(row) = &row {
        if let Some(content) = row.content.as_deref() {
            if !content.trim().is_empty() {
                return content.to_string();
            }
        }
    }
    // Fall back to the latest assistant message on the session.
    let session_id = match row.and_then(|r| r.session_id) {
        Some(s) if !s.is_empty() => s,
        _ => return String::new(),
    };
    let escaped_session = escape_graphql_string(&session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session}" }}, role: {{ _eq: "assistant" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ content }}
        }}"#
    );
    #[derive(Deserialize)]
    struct MsgRow {
        content: String,
    }
    let resp = node.execute(&query).await;
    first_optional_row::<MsgRow>(&resp, "AgentMessage")
        .map(|m| m.content)
        .unwrap_or_default()
}

/// Dump tool calls + messages for a session to stderr (debugging delegation).
async fn dump_session_diagnostics(node: &EmbeddedNode, session_id: &str) {
    let escaped = escape_graphql_string(session_id);
    let tc_query = format!(
        r#"{{
            AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped}" }} }}, order: {{ message_sequence: ASC }}) {{
                tool_name tool_call_id lifecycle_state status args result child_request_id await_mode tool_failure_class
            }}
        }}"#
    );
    let resp = node.execute(&tc_query).await;
    eprintln!(
        "[diag] tool calls for session {session_id}: {}",
        serde_json::to_string_pretty(
            &resp
                .data
                .unwrap_or_default()
                .get("AgentToolCall")
                .cloned()
                .unwrap_or_default()
        )
        .unwrap_or_default()
    );
    let msg_query = format!(
        r#"{{
            AgentMessage(filter: {{ session_id: {{ _eq: "{escaped}" }} }}, order: {{ sequence: ASC }}) {{
                sequence role content
            }}
        }}"#
    );
    let resp = node.execute(&msg_query).await;
    eprintln!(
        "[diag] messages for session {session_id}: {}",
        serde_json::to_string_pretty(
            &resp
                .data
                .unwrap_or_default()
                .get("AgentMessage")
                .cloned()
                .unwrap_or_default()
        )
        .unwrap_or_default()
    );
}

async fn wait_for_assistant_answer(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let answer = fetch_assistant_answer(node, request_id).await;
        if !answer.trim().is_empty() {
            return answer;
        }
        if tokio::time::Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ---------------------------------------------------------------------------
// Cross-node helpers (Test 2)
// ---------------------------------------------------------------------------

async fn wait_for_listen_addr(node: &EmbeddedNode) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addrs = node
            .p2p()
            .expect("p2p should be enabled")
            .listen_addresses()
            .await
            .expect("listen addresses");
        if let Some(addr) = addrs.first() {
            return addr.clone();
        }
        if Instant::now() >= deadline {
            panic!("node never exposed a P2P listen address; last_addrs={addrs:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_replicator_installed(node: &EmbeddedNode, peer_id: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let escaped_peer_id = escape_graphql_string(peer_id);
    let mut last = String::from("<none>");
    loop {
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
        let response = node.execute(&query).await;
        if let Some(row) = first_optional_row::<serde_json::Value>(&response, "PeerPairingApplied")
        {
            last = serde_json::to_string(&row).unwrap_or_else(|_| format!("{row:?}"));
            let installed = row
                .get("replicator_addresses")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|addresses| {
                    addresses
                        .iter()
                        .any(|address| address.as_str().is_some_and(|s| !s.trim().is_empty()))
                });
            if installed {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for PeerPairingApplied({peer_id}) to install a replicator; last row={last}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Write a `PeerPairingDesired` doc so the local node trusts `peer_did`.
async fn write_pairing(
    node: &EmbeddedNode,
    peer_id: &str,
    peer_did: &str,
    template: &str,
    peer_addr: &str,
) {
    let collections = resolve_template(template)
        .unwrap_or_else(|| panic!("template {template} should resolve"))
        .collections
        .iter()
        .map(|collection| format!("\"{}\"", escape_graphql_string(collection)))
        .collect::<Vec<_>>()
        .join(", ");
    let peer_id = escape_graphql_string(peer_id);
    let peer_did = escape_graphql_string(peer_did);
    let template = escape_graphql_string(template);
    let peer_addr = escape_graphql_string(peer_addr);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            upsert_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: "{peer_did}",
                    collections: [{collections}],
                    replicator_addresses: ["{peer_addr}"],
                    profiles: null,
                    template: "{template}",
                    source: "operator",
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{peer_did}",
                    collections: [{collections}],
                    replicator_addresses: ["{peer_addr}"],
                    profiles: null,
                    template: "{template}",
                    source: "operator",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "write pairing failed: {:?}",
        resp.errors
    );
}

#[derive(Debug, Clone, Deserialize)]
struct BridgeRow {
    tool_call_id: String,
    lifecycle_state: String,
    child_request_id: Option<String>,
    await_mode: Option<String>,
    args: Option<String>,
}

async fn fetch_subagent_bridge(node: &EmbeddedNode, session_id: &str) -> Option<BridgeRow> {
    let escaped = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{escaped}" }}, tool_name: {{ _eq: "spawn_subagent" }} }},
                limit: 1
            ) {{
                tool_call_id lifecycle_state child_request_id await_mode args
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<BridgeRow>(&resp, "AgentToolCall").filter(|row| {
        row.child_request_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
    })
}

async fn wait_for_subagent_bridge(
    node: &EmbeddedNode,
    session_id: &str,
    timeout: Duration,
) -> Option<BridgeRow> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(bridge) = fetch_subagent_bridge(node, session_id).await {
            return Some(bridge);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ToolCallRow {
    tool_call_id: String,
    lifecycle_state: String,
    args: Option<String>,
    result: Option<String>,
    await_mode: Option<String>,
    child_request_id: Option<String>,
}

async fn fetch_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Option<ToolCallRow> {
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_call_id: {{ _eq: "{tool_call_id}" }}
                }},
                limit: 1
            ) {{
                tool_call_id lifecycle_state args result await_mode child_request_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query tool call {tool_call_id} failed: {:?}",
        response.errors
    );
    first_optional_row::<ToolCallRow>(&response, "AgentToolCall")
}

async fn wait_for_tool_call_state(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
    expected_state: &str,
    timeout: Duration,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = None;
    loop {
        if let Some(row) = fetch_tool_call(node, session_id, tool_call_id).await {
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

async fn wait_for_background_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_name: &str,
    timeout: Duration,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let session_id = escape_graphql_string(session_id);
    let tool_name = escape_graphql_string(tool_name);
    loop {
        let query = format!(
            r#"{{
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        tool_name: {{ _eq: "{tool_name}" }},
                        await_mode: {{ _eq: "background" }}
                    }},
                    limit: 1
                ) {{
                    tool_call_id lifecycle_state args result await_mode child_request_id
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "query background tool failed: {:?}",
            response.errors
        );
        if let Some(row) = first_optional_row::<ToolCallRow>(&response, "AgentToolCall") {
            return row;
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
        assert_eq!(
            count, 0,
            "model called forbidden foreground/control tool {tool_name}"
        );
    }
}

#[derive(Debug, Deserialize)]
struct SessionMessageRow {
    role: String,
    content: String,
}

async fn load_session_messages(node: &EmbeddedNode, session_id: &str) -> Vec<SessionMessageRow> {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ role content }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query session messages failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|rows| serde_json::from_value(rows.clone()).ok())
        .unwrap_or_default()
}

fn model_tool_call_count(messages: &[SessionMessageRow], tool_name: &str) -> usize {
    messages
        .iter()
        .filter(|row| row.role == "assistant")
        .filter_map(|row| {
            serde_json::from_str::<gents_protocol::message::Message>(&row.content).ok()
        })
        .flat_map(|message| match message {
            gents_protocol::message::Message::Assistant { content, .. } => content,
            _ => Vec::new(),
        })
        .filter(|content| {
            matches!(
                content,
                gents_protocol::message::AssistantContent::ToolCall(call)
                    if call.function.name == tool_name
            )
        })
        .count()
}

fn tool_result_contains(messages: &[SessionMessageRow], needle: &str) -> bool {
    messages
        .iter()
        .filter(|row| row.role == "user")
        .filter_map(|row| {
            serde_json::from_str::<gents_protocol::message::Message>(&row.content).ok()
        })
        .any(|message| match message {
            gents_protocol::message::Message::User { content } => content.into_iter().any(|item| {
                let gents_protocol::message::UserContent::ToolResult(result) = item else {
                    return false;
                };
                result.content.into_iter().any(|part| {
                    matches!(
                        part,
                        gents_protocol::message::ToolResultContent::Text(text)
                            if text.text.contains(needle)
                    )
                })
            }),
            _ => false,
        })
}

async fn wait_for_model_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_name: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let messages = load_session_messages(node, session_id).await;
        if model_tool_call_count(&messages, tool_name) > 0 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for model tool call {tool_name} in session {session_id}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_tool_result_containing(
    node: &EmbeddedNode,
    session_id: &str,
    needle: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let messages = load_session_messages(node, session_id).await;
        if tool_result_contains(&messages, needle) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a tool result containing {needle:?} in session {session_id}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn assert_model_tool_call_count_at_least(
    node: &EmbeddedNode,
    session_id: &str,
    tool_name: &str,
    expected: usize,
) {
    let messages = load_session_messages(node, session_id).await;
    let actual = model_tool_call_count(&messages, tool_name);
    assert!(
        actual >= expected,
        "expected at least {expected} model call(s) to {tool_name} in session {session_id}, got {actual}; transcript={messages:#?}"
    );
}

async fn wait_for_message_containing(
    node: &EmbeddedNode,
    session_id: &str,
    needle: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let escaped_session_id = escape_graphql_string(session_id);
    loop {
        let query = format!(
            r#"{{
                AgentMessage(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                    order: {{ sequence: ASC }}
                ) {{ content }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "query notification messages failed: {:?}",
            response.errors
        );
        let found = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row.get("content")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|content| content.contains(needle))
                })
            });
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
    metadata: Option<String>,
}

async fn wait_for_background_wake(
    node: &EmbeddedNode,
    session_id: &str,
    queued_after_request_id: &str,
    timeout: Duration,
) -> WakeRequestRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let escaped_session_id = escape_graphql_string(session_id);
    loop {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                    order: {{ created_at: ASC }}
                ) {{ request_id metadata }}
            }}"#
        );
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
                let metadata = row
                    .metadata
                    .as_deref()
                    .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
                metadata.as_ref().is_some_and(|metadata| {
                    metadata["queue"]["source"] == "background_completion"
                        && metadata["queue"]["policy"] == "coalesce"
                        && metadata["queue"]["key"] == format!("background_completion:{session_id}")
                        && metadata["queue"]["queued_after_request_id"] == queued_after_request_id
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

#[derive(Debug, Clone, Deserialize)]
struct CrossRequestRow {
    request_id: String,
    agent_did: String,
    requester_did: Option<String>,
    behavior_id: String,
    lifecycle_state: Option<String>,
}

async fn fetch_request_on_node(node: &EmbeddedNode, request_id: &str) -> Option<CrossRequestRow> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                request_id agent_did requester_did behavior_id lifecycle_state
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<CrossRequestRow>(&resp, "AgentRequest")
}

async fn wait_for_request_on_node(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> Option<CrossRequestRow> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(row) = fetch_request_on_node(node, request_id).await {
            return Some(row);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
