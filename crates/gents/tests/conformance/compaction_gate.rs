//! Fences the *wiring* of the compaction gate, not just its predicate.
//!
//! `streaming_compaction.rs` drives `compaction::safe_to_reduce` and
//! `compaction::pair_safe_boundary` directly. That checks the functions agree
//! with `Compaction.PromptView.safeToReduce` and `Compaction.pairSafeBoundary`,
//! but it would stay green if `BehaviorDaemon::handle_request` stopped
//! consulting the gate altogether — the same class of gap #993 was filed for,
//! where the conformance case reimplemented the gate inside the test.
//!
//! This drives a real daemon. Compaction issues a sub-completion carrying
//! `compaction_prompt()`, so the mock backend's per-marker request counter is a
//! direct observation of whether the daemon attempted to reduce: zero while a
//! response in the session is still streaming, non-zero once it is terminal.

use super::*;

use std::time::Duration;

use super::support::fixtures::bind_default_behavior_backend;
use super::support::interrupt::{create_runtime_request, BootedAgent};
use super::support::streaming_backend::{MockStreamingBackend, StreamScript};

const GATE_MODEL: &str = "default";
const GATE_BACKEND_ID: &str = "backend-compaction-gate";
const GATE_MARKER: &str = "compaction-gate-request";
const LIVE_FIXTURE_MARKER: &str = "live compaction-gate fixture";
/// A distinctive slice of `compaction::summary::compaction_prompt()`. It only
/// ever appears in a body the compactor sent.
const COMPACTION_MARKER: &str = "supplied structured-output schema";

/// Roughly 30k estimated tokens of history — above the 20k `keep_recent_tokens`
/// default, so `split_messages_for_summary` yields a non-empty prefix to
/// summarize, and above the configured budget so the threshold is crossed.
const SEEDED_TURNS: usize = 12;
const SEEDED_TURN_BYTES: usize = 10_000;
const GATE_CONTEXT_WINDOW: usize = 30_000;
const GATE_COMPACTION_THRESHOLD: f64 = 0.5;

pub(super) async fn compaction_gate_blocks_reduction_while_a_response_streams() {
    let db = signed_materializer_test_db("compaction-gate").await;

    // The compaction plan must come first: the compactor's body carries both the
    // prompt and the seeded history, and the backend picks the first plan whose
    // marker the body contains.
    let backend = MockStreamingBackend::start(
        GATE_MODEL,
        vec![
            StreamScript::completes(
                COMPACTION_MARKER,
                [r#"{"goal": "continue the task", "completed_work": ["earlier turns inspected files"]}"#],
            ),
            StreamScript::completes(GATE_MARKER, ["ok"]),
            // If the daemon wins a fixture-claim race, let that abandoned
            // attempt terminate cleanly in its own session before we retry.
            StreamScript::completes(LIVE_FIXTURE_MARKER, ["fixture-race"]),
        ],
    )
    .expect("start mock streaming backend");

    let agent_did = signed_materializer_agent_did(&db).to_owned();
    let behavior_id = gents::default_behavior_id_for_agent(&agent_did);
    let agent = boot_compaction_gate_agent(&db, backend.endpoint()).await;
    assert_eq!(agent.agent_did, agent_did);

    // A response projection in this session is still mid-stream. Its
    // half-written turn can still receive more transcript rows, so reducing now
    // could summarize away a turn that is still being written. The fixture uses
    // a real signed pending -> claimed request and copies its exact source/claim
    // provenance into the live projection.
    let (mut live_lifecycle, live_response_doc_id, session_id) =
        create_exact_live_response(db.node.clone(), &agent_did, &behavior_id).await;
    // Model the durable-outcome -> live-projection crash gap. Terminal truth
    // and the request lifecycle have settled, so this fixture consumes no
    // execution slot, but the still-streaming projection must conservatively
    // keep compaction closed until it too converges.
    gents::publish_response_outcome_for_test(
        db.node.as_ref(),
        &live_lifecycle.request().doc_id,
        &agent_did,
        "error",
        Some("fixture_terminal"),
        None,
    )
    .await
    .expect("publish exact terminal response outcome");
    live_lifecycle
        .fail_with_reason("fixture_terminal")
        .await
        .expect("terminalize exact live fixture request");
    seed_bulky_history(db.node.as_ref(), &agent_did, &session_id).await;

    let blocked_request_id = format!("blocked-{}", uuid::Uuid::new_v4());
    let blocked_doc_id = create_runtime_request(
        db.node.as_ref(),
        agent.agent_did.as_str(),
        &behavior_id,
        &blocked_request_id,
        &session_id,
        GATE_MARKER,
    )
    .await;
    wait_for_terminal_request(db.node.as_ref(), &blocked_doc_id).await;

    assert_eq!(
        backend.observed_requests(COMPACTION_MARKER),
        0,
        "the daemon must not attempt compaction while a response in this session is streaming — \
         removing the gate from BehaviorDaemon::handle_request fails here"
    );

    // The mutable live projection catches up to its already-durable terminal
    // truth; the very next request may reduce.
    terminalize_live_response_projection(db.node.as_ref(), &live_response_doc_id).await;

    let allowed_request_id = format!("allowed-{}", uuid::Uuid::new_v4());
    let allowed_doc_id = create_runtime_request(
        db.node.as_ref(),
        agent.agent_did.as_str(),
        &behavior_id,
        &allowed_request_id,
        &session_id,
        GATE_MARKER,
    )
    .await;
    wait_for_terminal_request(db.node.as_ref(), &allowed_doc_id).await;

    let after_allowed = backend.observed_requests(COMPACTION_MARKER);
    assert!(
        after_allowed >= 1,
        "with every response in the session terminal the gate opens and the daemon reduces; \
         a gate that never opens would starve compaction entirely"
    );

    // A later turn reusing an earlier call id resurrects that earlier
    // announcement in the provider view, so a count recorded now would stop
    // naming the rows the next request drops
    // (`Compaction.reused_call_id_breaks_prefix_stability`). The daemon must
    // check `has_unique_call_ids` and decline rather than record a count it
    // cannot honour.
    seed_reused_call_id_turn(db.node.as_ref(), &agent.agent_did, &session_id).await;

    let reused_request_id = format!("reused-{}", uuid::Uuid::new_v4());
    let reused_doc_id = create_runtime_request(
        db.node.as_ref(),
        agent.agent_did.as_str(),
        &behavior_id,
        &reused_request_id,
        &session_id,
        GATE_MARKER,
    )
    .await;
    wait_for_terminal_request(db.node.as_ref(), &reused_doc_id).await;

    assert_eq!(
        backend.observed_requests(COMPACTION_MARKER),
        after_allowed,
        "a reused tool-call id must stop the daemon reducing — removing the \
         has_unique_call_ids check from BehaviorDaemon::handle_request fails here"
    );

    agent.shutdown().await;
}

/// Appends two turns that announce the *same* call id.
///
/// Both live in the freshly-appended tail, so the duplicate survives wherever
/// the compacted-prefix boundary happens to fall — reusing an id from an
/// already-summarized turn would not, since that announcement is no longer in
/// the view.
async fn seed_reused_call_id_turn(node: &EmbeddedNode, agent_did: &str, session_id: &str) {
    // Past anything the daemon has appended for the requests already run, so
    // these turns sort last in the loaded history.
    let mut sequence = 10_000i64;
    let call_id = "duplicated-call".to_string();

    for round in 0..2 {
        insert_message(
            node,
            agent_did,
            session_id,
            sequence,
            "user",
            &format!("reuse turn {round}: {}", "r".repeat(SEEDED_TURN_BYTES)),
        )
        .await;
        sequence += 1;

        let announcement = Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: call_id.clone(),
                call_id: Some(call_id.clone()),
                function: ToolFunction {
                    name: "read_file".to_string(),
                    arguments: json!({ "path": format!("/seed/reused-{round}.rs") }),
                },
                signature: None,
                additional_params: None,
            })],
        };
        insert_message(
            node,
            agent_did,
            session_id,
            sequence,
            "assistant",
            &serde_json::to_string(&announcement).expect("serialize reused announcement"),
        )
        .await;
        sequence += 1;

        let result = Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: call_id.clone(),
                call_id: Some(call_id.clone()),
                content: vec![ToolResultContent::Text(Text {
                    text: format!("contents of reused-{round}.rs"),
                })],
            })],
        };
        insert_message(
            node,
            agent_did,
            session_id,
            sequence,
            "user",
            &serde_json::to_string(&result).expect("serialize reused result"),
        )
        .await;
        sequence += 1;
    }
}

async fn boot_compaction_gate_agent(db: &support::TestDb, endpoint: &str) -> BootedAgent {
    let identity = db
        .node_identity()
        .expect("compaction gate fixture must use the node signing identity");
    bind_default_behavior_backend(db.node.as_ref(), identity.did(), GATE_BACKEND_ID, endpoint)
        .await;
    let behavior_id = gents::default_behavior_id_for_agent(identity.did());
    let mut behavior = gents::load_agent_behavior(db.node.as_ref(), &behavior_id)
        .await
        .expect("load compaction-gate behavior")
        .expect("compaction-gate behavior exists");
    behavior.model_name = Some(GATE_MODEL.to_string());
    behavior.compaction_threshold = Some(GATE_COMPACTION_THRESHOLD);
    let profile_id = behavior
        .inference_profile_id
        .clone()
        .expect("default behavior has an inference profile");
    gents::upsert_agent_behavior(db.node.as_ref(), &behavior)
        .await
        .expect("update compaction-gate behavior");

    let mut profile = gents::load_inference_profile(db.node.as_ref(), &profile_id)
        .await
        .expect("load compaction-gate inference profile")
        .expect("compaction-gate inference profile exists");
    profile.context_window = Some(GATE_CONTEXT_WINDOW as i64);
    profile.stream_batch_ms = Some(0);
    gents::upsert_inference_profile(db.node.as_ref(), &profile)
        .await
        .expect("update compaction-gate inference profile");

    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build compaction-gate agent");
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_gate_runtime_ready(db.node.as_ref(), &agent_did).await;

    BootedAgent::new(shutdown_tx, handle, agent_did)
}

/// Seeds turns that each carry a paired tool call and result.
///
/// The pairing matters: `safeToReduce` constrains *tool-result* rows, so a
/// transcript with none is vacuously safe to reduce and the gate would never
/// engage. The bulk lives in the surrounding text because `provider_view` stubs
/// tool-result payloads before the threshold is measured.
async fn seed_bulky_history(node: &EmbeddedNode, agent_did: &str, session_id: &str) {
    let mut sequence = 0i64;
    for turn in 0..SEEDED_TURNS {
        let payload = "h".repeat(SEEDED_TURN_BYTES);
        let call_id = format!("seeded-call-{turn}");

        insert_message(
            node,
            agent_did,
            session_id,
            sequence,
            "user",
            &format!("turn {turn}: {payload}"),
        )
        .await;
        sequence += 1;

        let announcement = Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: call_id.clone(),
                call_id: Some(call_id.clone()),
                function: ToolFunction {
                    name: "read_file".to_string(),
                    arguments: json!({ "path": format!("/seed/turn-{turn}.rs") }),
                },
                signature: None,
                additional_params: None,
            })],
        };
        insert_message(
            node,
            agent_did,
            session_id,
            sequence,
            "assistant",
            &serde_json::to_string(&announcement).expect("serialize announcement"),
        )
        .await;
        sequence += 1;

        let result = Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: call_id.clone(),
                call_id: Some(call_id),
                content: vec![ToolResultContent::Text(Text {
                    text: format!("contents of turn-{turn}.rs"),
                })],
            })],
        };
        insert_message(
            node,
            agent_did,
            session_id,
            sequence,
            "user",
            &serde_json::to_string(&result).expect("serialize tool result"),
        )
        .await;
        sequence += 1;

        insert_message(
            node,
            agent_did,
            session_id,
            sequence,
            "assistant",
            &format!("reply {turn}: {payload}"),
        )
        .await;
        sequence += 1;
    }
}

async fn insert_message(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    sequence: i64,
    role: &str,
    content: &str,
) {
    // The daemon persists its own turns into this session while the test runs,
    // so the key must not be derived from the sequence alone.
    let escaped_key =
        escape_graphql_string(&format!("{session_id}:{sequence}:{}", uuid::Uuid::new_v4()));
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_role = escape_graphql_string(role);
    let escaped_content = escape_graphql_string(content);
    let timestamp = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{escaped_key}",
                session_id: "{escaped_session_id}",
                agent_did: "{escaped_agent_did}",
                requester_did: "{escaped_agent_did}",
                request_id: "",
                sequence: {sequence},
                role: "{escaped_role}",
                content: "{escaped_content}",
                timestamp: "{timestamp}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed AgentMessage failed: {:?}",
        response.errors
    );
}

async fn create_exact_live_response(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    behavior_id: &str,
) -> (RequestLifecycle, String, String) {
    for attempt in 0..10 {
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        match try_create_exact_live_response(node.clone(), agent_did, behavior_id, &session_id)
            .await
        {
            Ok(Some((lifecycle, response_doc_id))) => {
                return (lifecycle, response_doc_id, session_id);
            }
            Ok(None) => {
                tokio::time::sleep(Duration::from_millis(10 * (attempt + 1))).await;
            }
            Err(error) => panic!("create exact live fixture failed: {error:#}"),
        }
    }
    panic!("daemon won every exact live fixture claim attempt")
}

async fn try_create_exact_live_response(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    behavior_id: &str,
    session_id: &str,
) -> anyhow::Result<Option<(RequestLifecycle, String)>> {
    let request_id = format!("live-{}", uuid::Uuid::new_v4());
    let request_doc_id = create_runtime_request(
        node.as_ref(),
        agent_did,
        behavior_id,
        &request_id,
        session_id,
        "live compaction-gate fixture",
    )
    .await;
    let request = DefraWatcher::new(node.clone(), agent_did)
        .try_fetch_request(&request_doc_id)
        .await?;
    let Some(request) = request else {
        return Ok(None);
    };
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        behavior_id,
        agent_did,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        GATE_BACKEND_ID,
    );
    match lifecycle.claim_with_identity().await {
        Ok(ClaimOutcome::Claimed) => {}
        Ok(_) => return Ok(None),
        Err(error)
            if error.to_string().contains("transaction conflict")
                || error.to_string().contains("is not pending/pending") =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    }
    let provenance = lifecycle
        .execution_provenance()
        .expect("claimed live fixture has exact provenance");

    let escaped_request_id = escape_graphql_string(&request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_request_doc_id = escape_graphql_string(&provenance.source.version.doc_id);
    let escaped_source_cid = escape_graphql_string(&provenance.source.version.composite_commit_cid);
    let escaped_source_signer = escape_graphql_string(&provenance.source.signer_did);
    let escaped_claim_cid = escape_graphql_string(&provenance.claim.version.composite_commit_cid);
    let escaped_claim_signer = escape_graphql_string(&provenance.claim.signer_did);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                    response_key: "{escaped_request_id}",
                    request_id: "{escaped_request_id}",
                    request_doc_id: "{escaped_request_doc_id}",
                    request_source_composite_commit_cid: "{escaped_source_cid}",
                    request_source_signer_did: "{escaped_source_signer}",
                    request_claim_composite_commit_cid: "{escaped_claim_cid}",
                    request_claim_signer_did: "{escaped_claim_signer}",
                    agent_did: "{escaped_agent_did}",
                    requester_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    session_id: "{escaped_session_id}",
                    content: "partial",
                    reasoning: "",
                    status: "streaming",
                    error_message: "",
                    token_count: 1,
                    progress_seq: 1,
                    reasoning_progress_seq: 0,
                    created_at: "{created_at}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "create exact live AgentResponse failed: {:?}",
            response.errors
        );
    }
    let response = node
        .execute(&format!(
            r#"{{
                AgentResponse(
                    filter: {{ response_key: {{ _eq: "{escaped_request_id}" }} }},
                    limit: 1
                ) {{ _docID }}
            }}"#
        ))
        .await;
    let row = first_row::<support::DocIdRow>(&response, "AgentResponse");
    Ok(Some((lifecycle, row.doc_id)))
}

async fn terminalize_live_response_projection(node: &EmbeddedNode, response_doc_id: &str) {
    let response_doc_id = escape_graphql_string(response_doc_id);
    let completed_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentResponse(
                filter: {{
                    _docID: {{ _eq: "{response_doc_id}" }},
                    status: {{ _eq: "streaming" }}
                }},
                input: {{
                    status: "error",
                    error_message: "fixture_terminal",
                    completed_at: "{completed_at}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "terminalize exact live AgentResponse projection failed: {:?}",
        response.errors
    );
    let data = response.data.expect("update AgentResponse data");
    let rows = data["update_AgentResponse"]
        .as_array()
        .expect("update AgentResponse rows");
    assert_eq!(rows.len(), 1, "live response projection must terminalize");
}

async fn wait_for_gate_runtime_ready(node: &EmbeddedNode, agent_did: &str) {
    let started = std::time::Instant::now();
    loop {
        if let Some(snapshot) = support::snapshots::fetch_runtime_snapshot(node, agent_did).await {
            if snapshot.process_state == "ready" && snapshot.reconcile_phase == "idle" {
                return;
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "timed out waiting for compaction-gate runtime to become ready"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_terminal_request(node: &EmbeddedNode, request_doc_id: &str) {
    let escaped_doc_id = escape_graphql_string(request_doc_id);
    let started = std::time::Instant::now();
    loop {
        let query = format!(
            r#"{{
                AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{
                    lifecycle_state
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        if let Some(row) = first_optional_row::<LifecycleStateRow>(&response, "AgentRequest") {
            if matches!(
                row.lifecycle_state.as_str(),
                "completed" | "failed" | "superseded" | "dead" | "interrupted"
            ) {
                return;
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "timed out waiting for request {request_doc_id} to reach a terminal lifecycle state"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[derive(Debug, Deserialize)]
struct LifecycleStateRow {
    lifecycle_state: String,
}
