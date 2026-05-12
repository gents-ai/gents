//! Integration tests for request interruption + TTL.
//!
//! These tests exercise the DB-level interactions that span multiple
//! `RequestLifecycle` instances and the resend chain, plus the full
//! `BehaviorDaemon` interrupt path against a deterministic local streaming
//! backend. The daemon tests assert the cross-layer bridge from a mid-stream
//! request interrupt to linked `InferenceCall.call_state = "cancelled"`.

mod support;

use std::sync::Arc;

use defra_agent::graphql::escape_graphql_string;
use defra_agent::lifecycle::{ClaimOutcome, ExecutionOrigin};
use defra_agent::{interrupt_request, AgentIdentity, DefraAgent, RequestLifecycle, ToolCeiling};
use defra_agent_protocol::transcript::present_persisted_message;

use support::fixtures::test_identity;
use support::interrupt::{
    create_runtime_request, wait_for_inference_call_state, wait_for_request_lifecycle_state,
    wait_for_response_content_contains, wait_for_response_doc_id, wait_for_runtime_ready,
    BootedAgent,
};
use support::snapshots::{
    fetch_message_snapshots_for_session, fetch_request_snapshot, fetch_response_content,
    fetch_response_interrupted_at, fetch_response_snapshot,
};
use support::streaming_backend::{MockStreamingBackend, StreamScript};
use support::{
    build_request, create_request, create_retry_request, set_valid_until, test_db, AGENT_DID,
    AGENT_NAME, BACKEND_ID, DEADLINE_SECS,
};

const STREAM_MODEL: &str = "default";
const STREAM_BACKEND_ID: &str = "backend-stream";
const PRIMARY_BEHAVIOR: &str = "general";
const SECONDARY_BEHAVIOR: &str = "code";
const TARGET_MARKER: &str = "interrupt-target";
const TARGET_PARTIAL: &str = "partial response content ";
const SURVIVOR_MARKER: &str = "survivor-target";
const SURVIVOR_PARTIAL: &str = "survivor partial content ";

// --- DB-level integration tests ---

/// Offline replay: if a large batch of pre-existing `AgentRequest` rows have
/// `valid_until` in the past (e.g. agent was offline and is catching up), each
/// `RequestLifecycle::claim()` should short-circuit to `Expired` and transition
/// the row to `dead`/`Stale`. No inference call ever fires because the
/// expiration check runs before any backend interaction.
///
/// This guards the TTL safety property: stale work never consumes backend
/// quota or side-effects on replay.
#[tokio::test]
async fn offline_replay_of_stale_requests_does_not_call_backend() {
    let db = test_db("offline-replay-stale").await;
    let past = (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();
    let created_at = chrono::Utc::now().to_rfc3339();

    const BATCH: usize = 20;
    let mut request_doc_ids = Vec::with_capacity(BATCH);
    for _ in 0..BATCH {
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let doc_id =
            create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;
        set_valid_until(&db.node, &doc_id, &past).await;
        request_doc_ids.push((doc_id, request_id, session_id));
    }

    // Claim each row sequentially — this matches the "offline agent catching
    // up after coming back online" shape the test is modelling, and avoids
    // the embedded-datastore transaction-conflict retry limit we'd hit with
    // fully parallel claims on the shared AgentRequest secondary indexes.
    for (doc_id, request_id, session_id) in request_doc_ids.clone() {
        let request = build_request(doc_id, request_id, session_id, created_at.clone());
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            db.node.clone(),
            AGENT_NAME,
            AGENT_DID,
            request,
            DEADLINE_SECS,
            ExecutionOrigin::Interactive,
            BACKEND_ID,
        );
        assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);
    }

    // All rows should now be dead/Stale with no backend binding present.
    for (doc_id, _, _) in &request_doc_ids {
        let snap = fetch_request_snapshot(&db.node, doc_id).await;
        assert_eq!(snap.lifecycle_state, "dead");
        assert_eq!(snap.failure_reason, "Stale");
        assert_eq!(
            snap.backend_id, "",
            "stale request must not be bound to a backend"
        );
        assert!(
            !snap.claimed_at_present,
            "stale request must not be claimed"
        );
    }
}

/// Resend chain: after a request goes stale, a resend should populate
/// `retry_parent_request = <previous>` and `retry_root_request = <original>`.
/// Chaining further must keep `retry_root_request` stable across the chain
/// while `retry_parent_request` advances — this is the invariant the UI
/// relies on to render the root-level grouping of retry attempts.
///
/// We exercise this against the DB directly rather than calling
/// `resend_request` (which lives in `defra-agent-desktop` and would
/// introduce a dev-dep cycle). The `create_retry_request` helper mirrors
/// exactly the fields that the `resend_request` helper writes.
#[tokio::test]
async fn resend_from_stale_populates_retry_chain() {
    let db = test_db("resend-chain").await;

    let created_at = chrono::Utc::now().to_rfc3339();
    let past = (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339();

    // --- Step 1: original request goes stale. ---
    let original_request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let original_doc_id = create_request(
        &db.node,
        &original_request_id,
        &session_id,
        "pending",
        &created_at,
    )
    .await;
    set_valid_until(&db.node, &original_doc_id, &past).await;

    let request = build_request(
        original_doc_id.clone(),
        original_request_id.clone(),
        session_id.clone(),
        created_at.clone(),
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);

    // --- Step 2: first resend chains from the original. ---
    let resend_1_id = uuid::Uuid::new_v4().to_string();
    let resend_1_created_at = chrono::Utc::now().to_rfc3339();
    let resend_1_doc_id = create_retry_request(
        &db.node,
        &resend_1_id,
        &session_id,
        &original_request_id, // retry_parent
        &original_request_id, // retry_root == original (original is the root)
        "hello",
        &resend_1_created_at,
    )
    .await;

    let snap_1 = fetch_request_snapshot(&db.node, &resend_1_doc_id).await;
    assert_eq!(snap_1.retry_parent_request, original_request_id);
    assert_eq!(snap_1.retry_root_request, original_request_id);

    // --- Step 3: resend_1 also goes stale; second resend chains from resend_1
    // but root must remain the original. ---
    set_valid_until(&db.node, &resend_1_doc_id, &past).await;
    let request_1 = build_request(
        resend_1_doc_id.clone(),
        resend_1_id.clone(),
        session_id.clone(),
        resend_1_created_at.clone(),
    );
    let mut lifecycle_1 = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request_1,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle_1.claim().await.unwrap(), ClaimOutcome::Expired);

    let resend_2_id = uuid::Uuid::new_v4().to_string();
    let resend_2_created_at = chrono::Utc::now().to_rfc3339();
    let resend_2_doc_id = create_retry_request(
        &db.node,
        &resend_2_id,
        &session_id,
        &resend_1_id,         // retry_parent = previous resend
        &original_request_id, // retry_root STAYS original
        "hello",
        &resend_2_created_at,
    )
    .await;

    let snap_2 = fetch_request_snapshot(&db.node, &resend_2_doc_id).await;
    assert_eq!(snap_2.retry_parent_request, resend_1_id);
    assert_eq!(
        snap_2.retry_root_request, original_request_id,
        "retry_root_request must be stable across the chain"
    );
}

#[tokio::test]
async fn inference_call_wait_observes_latest_attempt() {
    let db = test_db("inference-call-wait-latest").await;
    let request_id = "req-inference-call-wait-latest";

    insert_inference_call(
        db.node.as_ref(),
        request_id,
        1,
        "failed",
        Some("ProviderError: transient connect failure"),
    )
    .await;
    // Regression guard: a failed historical attempt must not hide the current retry.
    insert_inference_call(db.node.as_ref(), request_id, 2, "running", None).await;

    let call = wait_for_inference_call_state(db.node.as_ref(), request_id, "running").await;
    assert_eq!(call.call_seq, 2);
    assert_eq!(call.call_state, "running");
}

// --- Full BehaviorDaemon streaming interruption tests ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_mid_stream_preserves_partial_and_cancels_inference_call() {
    let db = test_db("daemon-interrupt-mid-stream").await;
    let backend = MockStreamingBackend::start(
        STREAM_MODEL,
        vec![StreamScript::paused(TARGET_MARKER, [TARGET_PARTIAL])],
    )
    .unwrap();
    let agent = boot_streaming_agent(
        &db,
        "daemon-interrupt-mid-stream",
        backend.endpoint(),
        &[PRIMARY_BEHAVIOR],
        2,
    )
    .await;

    let request_id = "req-daemon-interrupt-mid-stream";
    let session_id = "session-daemon-interrupt-mid-stream";
    let request_doc_id = create_runtime_request(
        db.node.as_ref(),
        agent.agent_did.as_str(),
        PRIMARY_BEHAVIOR,
        request_id,
        session_id,
        TARGET_MARKER,
    )
    .await;

    backend.wait_for_chunks(TARGET_MARKER, 1).await;
    let response_doc_id = wait_for_response_doc_id(db.node.as_ref(), request_id).await;
    wait_for_response_content_contains(db.node.as_ref(), &response_doc_id, TARGET_PARTIAL).await;

    interrupt_request(db.node.as_ref(), request_id)
        .await
        .expect("interrupt_request should latch interrupt_requested_at");

    wait_for_request_lifecycle_state(db.node.as_ref(), &request_doc_id, "interrupted").await;
    let call = wait_for_inference_call_state(db.node.as_ref(), request_id, "cancelled").await;
    assert_eq!(call.failure_reason.as_deref(), Some("Cancelled"));

    let content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(
        content, "",
        "daemon interrupt must clear the live tail after persisting partial content"
    );
    let messages = fetch_message_snapshots_for_session(&db.node, session_id).await;
    assert!(
        messages.iter().any(|message| {
            message.role == "assistant"
                && present_persisted_message(&message.role, &message.content).body_markdown
                    == TARGET_PARTIAL.trim()
        }),
        "daemon interrupt must preserve already streamed response content in AgentMessage"
    );
    assert!(
        fetch_response_interrupted_at(&db.node, &response_doc_id)
            .await
            .is_some(),
        "daemon interrupt must stamp AgentResponse.interrupted_at"
    );

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupting_one_request_does_not_affect_another() {
    let db = test_db("daemon-interrupt-isolation").await;
    let backend = MockStreamingBackend::start(
        STREAM_MODEL,
        vec![
            StreamScript::paused(TARGET_MARKER, [TARGET_PARTIAL]),
            StreamScript::paused(SURVIVOR_MARKER, [SURVIVOR_PARTIAL]),
        ],
    )
    .unwrap();
    let agent = boot_streaming_agent(
        &db,
        "daemon-interrupt-isolation",
        backend.endpoint(),
        &[PRIMARY_BEHAVIOR, SECONDARY_BEHAVIOR],
        4,
    )
    .await;

    let target_request_id = "req-daemon-interrupt-target";
    let target_session_id = "session-daemon-interrupt-target";
    let target_doc_id = create_runtime_request(
        db.node.as_ref(),
        agent.agent_did.as_str(),
        PRIMARY_BEHAVIOR,
        target_request_id,
        target_session_id,
        TARGET_MARKER,
    )
    .await;

    let survivor_request_id = "req-daemon-survivor";
    let survivor_session_id = "session-daemon-survivor";
    let survivor_doc_id = create_runtime_request(
        db.node.as_ref(),
        agent.agent_did.as_str(),
        SECONDARY_BEHAVIOR,
        survivor_request_id,
        survivor_session_id,
        SURVIVOR_MARKER,
    )
    .await;

    backend.wait_for_chunks(TARGET_MARKER, 1).await;
    backend.wait_for_chunks(SURVIVOR_MARKER, 1).await;

    let target_response_doc_id =
        wait_for_response_doc_id(db.node.as_ref(), target_request_id).await;
    let survivor_response_doc_id =
        wait_for_response_doc_id(db.node.as_ref(), survivor_request_id).await;
    wait_for_response_content_contains(db.node.as_ref(), &target_response_doc_id, TARGET_PARTIAL)
        .await;
    wait_for_response_content_contains(
        db.node.as_ref(),
        &survivor_response_doc_id,
        SURVIVOR_PARTIAL,
    )
    .await;

    interrupt_request(db.node.as_ref(), target_request_id)
        .await
        .expect("interrupt_request should latch interrupt_requested_at");

    wait_for_request_lifecycle_state(db.node.as_ref(), &target_doc_id, "interrupted").await;
    let target_call =
        wait_for_inference_call_state(db.node.as_ref(), target_request_id, "cancelled").await;
    assert_eq!(target_call.failure_reason.as_deref(), Some("Cancelled"));

    let survivor_running =
        wait_for_inference_call_state(db.node.as_ref(), survivor_request_id, "running").await;
    assert_eq!(
        survivor_running.call_state, "running",
        "unrelated concurrent inference call must remain live after target interrupt"
    );

    backend.release(SURVIVOR_MARKER);
    wait_for_request_lifecycle_state(db.node.as_ref(), &survivor_doc_id, "completed").await;
    let survivor_call =
        wait_for_inference_call_state(db.node.as_ref(), survivor_request_id, "completed").await;
    assert_eq!(survivor_call.failure_reason.as_deref(), None);

    let survivor_response = fetch_response_snapshot(&db.node, &survivor_response_doc_id).await;
    assert_eq!(survivor_response.status, "complete");
    let survivor_content = fetch_response_content(&db.node, &survivor_response_doc_id).await;
    assert_eq!(
        survivor_content, "",
        "completed response must leave AgentResponse.content as an empty live tail"
    );
    let survivor_messages =
        fetch_message_snapshots_for_session(&db.node, survivor_session_id).await;
    assert!(
        survivor_messages.iter().any(|message| {
            message.role == "assistant"
                && present_persisted_message(&message.role, &message.content).body_markdown
                    == SURVIVOR_PARTIAL.trim()
        }),
        "completed survivor response must be preserved in AgentMessage"
    );
    assert!(
        fetch_response_interrupted_at(&db.node, &survivor_response_doc_id)
            .await
            .is_none(),
        "unrelated response must not be stamped as interrupted"
    );

    agent.shutdown().await;
}

async fn boot_streaming_agent(
    db: &support::TestDb,
    test_name: &str,
    endpoint: &str,
    behavior_ids: &[&str],
    max_concurrent: i64,
) -> BootedAgent {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    upsert_streaming_backend(
        db.node.as_ref(),
        STREAM_BACKEND_ID,
        endpoint,
        max_concurrent,
    )
    .await;

    let mut builder = DefraAgent::builder()
        .node(db.node.clone())
        .identity(identity.clone())
        .default_behavior_id(behavior_ids[0])
        .tool_ceiling(ToolCeiling::meta_only());
    for behavior_id in behavior_ids {
        builder = builder
            .behavior(*behavior_id)
            .backend_id(STREAM_BACKEND_ID)
            .model_name(STREAM_MODEL)
            .stream_batch_ms(0)
            .done();
    }

    let agent = builder.build().await.unwrap();
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;

    BootedAgent::new(shutdown_tx, handle, agent_did)
}

async fn upsert_streaming_backend(
    node: &defra_agent::defra_node::EmbeddedNode,
    backend_id: &str,
    endpoint: &str,
    max_concurrent: i64,
) {
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(endpoint);
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
                    max_concurrent: {max_concurrent},
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{STREAM_MODEL}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: {max_concurrent},
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{STREAM_MODEL}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert streaming backend failed: {:?}",
        response.errors
    );
}

async fn insert_inference_call(
    node: &defra_agent::defra_node::EmbeddedNode,
    request_id: &str,
    call_seq: i64,
    call_state: &str,
    failure_reason: Option<&str>,
) {
    let call_id = format!("call-{request_id}-{call_seq}");
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_call_id = escape_graphql_string(&call_id);
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_call_state = escape_graphql_string(call_state);
    let escaped_now = escape_graphql_string(&now);
    let failure_reason_field = failure_reason
        .map(|reason| format!(r#"failure_reason: "{}","#, escape_graphql_string(reason)))
        .unwrap_or_default();
    let ended_at_field = if matches!(call_state, "failed" | "completed" | "cancelled") {
        format!(r#"ended_at: "{escaped_now}","#)
    } else {
        String::new()
    };

    // These links are plain string fields in the test schema; the helper does
    // not need to create matching backend or behavior rows.
    let mutation = format!(
        r#"mutation {{
            add_InferenceCall(input: {{
                call_id: "{escaped_call_id}",
                runtime_instance_id: "runtime-test",
                request_id: "{escaped_request_id}",
                call_seq: {call_seq},
                backend_id: "{STREAM_BACKEND_ID}",
                behavior_id: "{PRIMARY_BEHAVIOR}",
                agent_did: "{AGENT_DID}",
                call_kind: "inference",
                attempt: {call_seq},
                call_state: "{escaped_call_state}",
                {failure_reason_field}
                queued_at: "{escaped_now}",
                started_at: "{escaped_now}",
                {ended_at_field}
                priority: 0,
                queue_depth_at_enqueue: 0,
                controller_generation: 0,
                backend_config_fingerprint: "test"
            }}) {{ _docID }}
        }}"#,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "insert inference call failed: {:?}",
        response.errors
    );
}
