//! Integration tests for request interruption + TTL.
//!
//! These tests exercise the DB-level interactions that span multiple
//! `RequestLifecycle` instances and the resend chain. Tests that require
//! a running `BehaviorDaemon` with a mock streaming backend (mid-stream
//! interrupt, concurrent-request isolation) are stubbed with `#[ignore]`
//! until that fixture infrastructure exists — see the `todo!()` stubs
//! at the bottom of this file for pointers.

mod support;

use defra_agent::lifecycle::{ClaimOutcome, ExecutionOrigin};
use defra_agent::RequestLifecycle;

use support::snapshots::fetch_request_snapshot;
use support::{
    build_request, create_request, create_retry_request, set_valid_until, test_db, AGENT_DID,
    AGENT_NAME, BACKEND_ID, DEADLINE_SECS,
};

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
        let request = build_request(
            doc_id,
            request_id,
            session_id,
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

// --- Stubs for daemon-fixture-dependent tests ---
//
// These cases require spinning up a full `BehaviorDaemon` wired to a mock
// streaming backend so we can observe the `tokio::select!` race between the
// inference stream and the interrupt watch channel. Building that fixture is
// an independent infrastructure task (see the Task 11 follow-up notes); the
// state-level transitions these would verify are already covered piecewise
// by `state_machine_conformance.rs`.

#[tokio::test]
#[ignore = "requires mock streaming backend fixture; follow-up after Task 11"]
async fn interrupt_mid_stream_preserves_partial_and_cancels_inference_call() {
    todo!("needs BehaviorDaemon + mock streaming backend fixture");
}

#[tokio::test]
#[ignore = "requires mock streaming backend fixture; follow-up after Task 11"]
async fn interrupting_one_request_does_not_affect_another() {
    todo!("needs BehaviorDaemon + mock streaming backend fixture");
}
