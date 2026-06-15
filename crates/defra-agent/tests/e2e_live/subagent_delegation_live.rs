//! Live end-to-end subagent delegation tests against an OpenAI-compatible
//! vLLM backend. These exercise the #377 subagent enablement + convergence
//! with REAL inference: an orchestrator agent, driven by the live model,
//! delegates work to a subagent behavior via `spawn_subagent`, the subagent
//! runs (live model), and the result flows back to the orchestrator.
//!
//! Normal test runs skip these (they are `#[ignore]`-gated AND early-return
//! unless `DEFRA_AGENT_LIVE_SUBAGENT=1`). To run locally:
//!
//! ```bash
//! DEFRA_AGENT_LIVE_SUBAGENT=1 \
//!   cargo test -p defra-agent --test subagent_delegation_live -- --ignored --nocapture
//! ```
//!
//! Endpoint/model are overridable:
//! - `DEFRA_AGENT_LIVE_SUBAGENT_ENDPOINT` (default `http://100.73.235.38:8000/v1`)
//! - `DEFRA_AGENT_LIVE_SUBAGENT_MODEL` (default `d4f`)
//!
//! ## Cross-node delegation (Test 2)
//!
//! `live_cross_node_subagent_delegation` exercises orchestrator-on-A delegating
//! to a behavior hosted on B over REAL in-process P2P replication (the proven
//! `test_p2p_db` + `install_one_way_replicator` pattern from the R5
//! cross-deployment conformance harness — no test "pump").
//!
//! Subagent targets are named `(agent_did, behavior_id)` pairs. The orchestrator
//! on A writes the child `AgentRequest` LOCALLY stamped with the TARGET's
//! `agent_did` (= DID-B). Bidirectional replication carries that child to B,
//! where B's daemon + SubagentSource claim and run it against the live model;
//! the terminal child request + its response replicate back to A. The full
//! assertions (bridge on A -> child materialized + run on B with a non-empty
//! live answer -> terminal replicated back to A) all run live.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, load_agent_behavior, upsert_agent_behavior, upsert_tool_selection,
    AgentBehaviorDocument, AgentIdentity, DefraAgent, DocumentRuntimeOptions, SubagentTarget,
    ToolCeiling, ToolSelectionDocument,
};
use serde::Deserialize;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::{first_optional_row, test_db, test_p2p_db, TestDb};

const DEFAULT_LIVE_ENDPOINT: &str = "http://100.73.235.38:8000/v1";
const DEFAULT_LIVE_MODEL: &str = "d4f";
const LIVE_BACKEND_ID: &str = "backend-live-subagent";
const RESEARCHER_BEHAVIOR_ID: &str = "live-researcher";
/// Friendly, model-facing subagent target name (the model never sees behavior ids).
const RESEARCHER_TARGET_NAME: &str = "researcher";

/// The `chat-requests` P2P collection profile (matches the `p2p pair`
/// chat-requests profile in `defra-agent-cli`). These carry the full delegation
/// round-trip: the child `AgentRequest` + bridge `AgentToolCall` A->B and the
/// terminal child + its response/messages B->A.
const CHAT_REQUEST_COLLECTIONS: &[&str] = &[
    "AgentConversation",
    "AgentRequest",
    "AgentResponse",
    "AgentToolResult",
    "AgentSession",
    "AgentMessage",
    "AgentToolCall",
    "CompactionEntry",
];

fn live_enabled() -> bool {
    std::env::var("DEFRA_AGENT_LIVE_SUBAGENT").as_deref() == Ok("1")
}

fn live_endpoint() -> String {
    std::env::var("DEFRA_AGENT_LIVE_SUBAGENT_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_LIVE_ENDPOINT.to_string())
}

fn live_model() -> String {
    std::env::var("DEFRA_AGENT_LIVE_SUBAGENT_MODEL")
        .unwrap_or_else(|_| DEFAULT_LIVE_MODEL.to_string())
}

// ---------------------------------------------------------------------------
// Test 1: local delegation (orchestrator + subagent on one node / one DID)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set DEFRA_AGENT_LIVE_SUBAGENT=1 and pass --ignored"]
async fn live_local_subagent_delegation() -> Result<()> {
    if !live_enabled() {
        eprintln!("DEFRA_AGENT_LIVE_SUBAGENT is not 1; skipping live local subagent delegation");
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
// Test 2: cross-node delegation (orchestrator on A -> behavior on B)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set DEFRA_AGENT_LIVE_SUBAGENT=1 and pass --ignored"]
async fn live_cross_node_subagent_delegation() -> Result<()> {
    if !live_enabled() {
        eprintln!(
            "DEFRA_AGENT_LIVE_SUBAGENT is not 1; skipping live cross-node subagent delegation"
        );
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

    // --- Node B: host the researcher behavior owned by DID-B. ---
    ensure_agent_principal(db_b.node.as_ref(), &did_b)
        .await
        .expect("ensure principal B");
    let profile_b =
        default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(&did_b));
    upsert_live_backend(db_b.node.as_ref(), &endpoint, &model).await;
    configure_behavior(
        db_b.node.as_ref(),
        RESEARCHER_BEHAVIOR_ID,
        &did_b,
        &model,
        &profile_b,
        "You answer the user's question concisely and factually in one short sentence.",
        Some("Researches factual questions and returns a concise factual answer."),
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
        CROSS_NODE_ORCHESTRATOR_SYSTEM_PROMPT,
        None,
    )
    .await;

    // The named (agent_did, behavior_id) target is owned by DID-B: a REMOTE
    // delegation target. Post-AB-fix, A authors the bridge + writes the child
    // request LOCALLY stamped with DID-B WITHOUT resolving B's behavior locally,
    // so we do NOT mirror B's AgentBehavior onto A. Replication carries the
    // child to B.
    authorize_subagents(
        db_a.node.as_ref(),
        &did_a,
        &orchestrator_behavior_id,
        vec![SubagentTarget {
            name: RESEARCHER_TARGET_NAME.to_string(),
            agent_did: did_b.clone(),
            behavior_id: RESEARCHER_BEHAVIOR_ID.to_string(),
            description: Some("Researches factual questions.".to_string()),
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

    // Pairing must exist on BOTH nodes BEFORE the agents boot (paired_peer_dids
    // are loaded at startup). A trusts DID-B; B trusts DID-A.
    write_pairing(db_a.node.as_ref(), "peer-b", &did_b).await;
    write_pairing(db_b.node.as_ref(), "peer-a", &did_a).await;

    // REAL bidirectional in-process P2P replication for the chat-requests
    // collection set, installed BEFORE the request is submitted. A->B carries
    // the child request + bridge tool call so B materializes + runs the child;
    // B->A carries the terminal child request + response/messages so A's
    // BackgroundCompletionObserver can project completion back onto the parent.
    install_one_way_replicator(
        db_a.node.as_ref(),
        db_b.node.as_ref(),
        CHAT_REQUEST_COLLECTIONS,
    )
    .await;
    install_one_way_replicator(
        db_b.node.as_ref(),
        db_a.node.as_ref(),
        CHAT_REQUEST_COLLECTIONS,
    )
    .await;

    // Boot full agents on both nodes. B owns DID-B: its daemon + SubagentSource
    // claim and run the replicated child request against the live model.
    let agent_b = boot_document_agent(&db_b, identity_b).await?;
    let agent_a = boot_document_agent(&db_a, identity_a).await?;

    let request_id = "req-live-cross-node";
    let session_id = "session-live-cross-node";
    create_runtime_request(
        db_a.node.as_ref(),
        &did_a,
        &orchestrator_behavior_id,
        request_id,
        session_id,
        "Use your research subagent to find the capital of France, then tell me the answer.",
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
    assert_eq!(child_on_b.behavior_id, RESEARCHER_BEHAVIOR_ID);

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

    agent_a.shutdown().await;
    agent_b.shutdown().await;
    // BootedAgent only stops DefraAgent::run; P2P belongs to the embedded node.
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

const CROSS_NODE_ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are an orchestrator agent. You have a remote \
research subagent available named `researcher`. For ANY research or factual lookup the \
user asks for, you MUST delegate it by calling the `spawn_subagent` tool with name exactly \
\"researcher\", await_mode exactly \"background\", and a `prompt` describing the question. The \
subagent runs on a different node, so you MUST use await_mode=\"background\" (foreground is rejected). Do \
not answer factual questions yourself; always delegate them.";

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

/// Boot a full DefraAgent from the behavior documents owned by `identity`'s DID.
async fn boot_document_agent(db: &TestDb, identity: Arc<dyn AgentIdentity>) -> Result<BootedAgent> {
    let agent = DefraAgent::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    Ok(BootedAgent::new(shutdown_tx, handle, agent_did))
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
            system_context_template: None,
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

#[derive(Debug, Clone, Deserialize)]
struct ChildRow {
    request_id: String,
    behavior_id: String,
    /// Deserialized for Debug-trace output on failures; not read directly.
    #[allow(dead_code)]
    agent_did: String,
    lifecycle_state: Option<String>,
    caused_by_parent_request_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
}

fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "dead" | "interrupted" | "superseded"
    )
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

/// Connect two P2P nodes and install a sender->receiver replicator for
/// `collections`. Mirrors the proven R5 cross-deployment harness pattern: add
/// the collections to both nodes' P2P sets and register the replicator on both
/// the sender (push target) and receiver (authorization) sides. Call it both
/// directions (A->B and B->A) for bidirectional replication.
async fn install_one_way_replicator(
    sender: &EmbeddedNode,
    receiver: &EmbeddedNode,
    collections: &[&str],
) {
    let sender_addr = wait_for_listen_addr(sender).await;
    let receiver_addr = wait_for_listen_addr(receiver).await;
    let sender_p2p = sender.p2p().expect("sender p2p");
    let receiver_p2p = receiver.p2p().expect("receiver p2p");

    sender_p2p
        .connect_peer(&receiver_addr)
        .await
        .expect("connect sender to receiver");
    wait_for_connected_peer(sender).await;
    wait_for_connected_peer(receiver).await;

    let collection_names = collections
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    sender_p2p
        .add_collections(collection_names.clone())
        .await
        .expect("add sender p2p collections");
    receiver_p2p
        .add_collections(collection_names.clone())
        .await
        .expect("add receiver p2p collections");
    receiver_p2p
        .add_replicator(
            collection_names.clone(),
            Some(&sender_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("authorize sender as receiver-side replicator");
    sender_p2p
        .add_replicator(
            collection_names,
            Some(&receiver_addr),
            Default::default(),
            Vec::new(),
            None,
        )
        .await
        .expect("install sender to receiver replicator");
}

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

async fn wait_for_connected_peer(node: &EmbeddedNode) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let peers = node
            .p2p()
            .expect("p2p should be enabled")
            .connected_peers()
            .await
            .expect("connected peers");
        if !peers.is_empty() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("node never reported a connected peer; last_peers={peers:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Write a `PeerPairingDesired` doc so the local node trusts `peer_did`.
async fn write_pairing(node: &EmbeddedNode, peer_id: &str, peer_did: &str) {
    let peer_id = escape_graphql_string(peer_id);
    let peer_did = escape_graphql_string(peer_did);
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            upsert_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: "{peer_did}",
                    collections: ["AgentRequest", "AgentToolCall", "AgentResponse", "AgentMessage"],
                    replicator_addresses: [],
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{ agent_did: "{peer_did}", updated_at: "{now}" }}
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
}

async fn fetch_subagent_bridge(node: &EmbeddedNode, session_id: &str) -> Option<BridgeRow> {
    let escaped = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{escaped}" }}, tool_name: {{ _eq: "spawn_subagent" }} }},
                limit: 1
            ) {{
                tool_call_id lifecycle_state child_request_id await_mode
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
struct CrossRequestRow {
    request_id: String,
    agent_did: String,
    behavior_id: String,
    lifecycle_state: Option<String>,
}

async fn fetch_request_on_node(node: &EmbeddedNode, request_id: &str) -> Option<CrossRequestRow> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                request_id agent_did behavior_id lifecycle_state
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
