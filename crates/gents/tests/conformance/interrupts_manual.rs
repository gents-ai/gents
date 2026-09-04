use super::*;
use gents::lifecycle::RequestTerminalOutcome;
use gents::StreamWriter;
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[tokio::test]
async fn fork_does_not_transition_parent_lifecycle_state() {
    use gents::session::{fork, ForkParams};
    use support::{
        create_agent_behavior, create_agent_conversation, create_agent_message,
        create_agent_session,
    };

    let db = test_db("fork-no-lifecycle-transition").await;

    let parent_session = uuid::Uuid::new_v4().to_string();
    create_agent_session(
        &db.node,
        &parent_session,
        AGENT_NAME,
        "2026-04-21T10:00:00Z",
    )
    .await;
    create_agent_conversation(
        &db.node,
        &parent_session,
        AGENT_NAME,
        "2026-04-21T10:00:00Z",
    )
    .await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_request(
        &db.node,
        &request_id,
        &parent_session,
        "completed",
        "2026-04-21T10:00:02Z",
    )
    .await;
    let response_key = format!("resp-{request_id}");
    let response_doc_id = create_response_with_status(
        &db.node,
        &response_key,
        &request_id,
        &parent_session,
        "complete",
    )
    .await;

    create_agent_message(
        &db.node,
        &parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        &parent_session,
        2,
        "assistant",
        "a1",
        "2026-04-21T10:00:03Z",
    )
    .await;

    let before_request = fetch_request_snapshot(&db.node, &request_doc_id).await;
    let before_response = fetch_response_snapshot(&db.node, &response_doc_id).await;
    let before_conversation = fetch_conversation_snapshot(&db.node, &parent_session).await;
    let before_session = fetch_session_snapshot(&db.node, &parent_session).await;

    let _ = fork(
        &db.node,
        ForkParams {
            source_session_id: &parent_session,
            fork_at_user_turn: 0,
            caller_agent_did: AGENT_DID,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds on idle parent");

    let after_request = fetch_request_snapshot(&db.node, &request_doc_id).await;
    let after_response = fetch_response_snapshot(&db.node, &response_doc_id).await;
    let after_conversation = fetch_conversation_snapshot(&db.node, &parent_session).await;
    let after_session = fetch_session_snapshot(&db.node, &parent_session).await;

    assert_eq!(
        before_request, after_request,
        "parent AgentRequest unchanged"
    );
    assert_eq!(
        before_response, after_response,
        "parent AgentResponse unchanged"
    );
    assert_eq!(
        before_conversation, after_conversation,
        "parent AgentConversation unchanged"
    );
    assert_eq!(
        before_session, after_session,
        "parent AgentSession unchanged"
    );
}

#[tokio::test]
async fn pending_interrupted_via_interrupt_before_claim() {
    let db = test_db("pending-interrupted").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
    assert_lean_transition_is_legal("Request", "pending", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Interrupted);
}

#[tokio::test]
async fn pending_dead_stale_via_expire() {
    let db = test_db("pending-dead-stale").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let valid_until = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let doc_id = create_request_with_valid_until(
        &db.node,
        &request_id,
        &session_id,
        "pending",
        &created_at,
        Some(&valid_until),
    )
    .await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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
    assert_lean_transition_is_legal("Request", "pending", "dead");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Dead);
    assert_eq!(snap.failure_reason, "Stale");
}

#[tokio::test]
async fn transition_to_interrupted_from_claimed() {
    // Validates the lifecycle transition from `claimed` to `interrupted` via
    // `transition_to_interrupted`. This test does NOT exercise the observer or
    // watch channel end-to-end — the full `tokio::select!` arm + observer race
    // is covered at integration level in Task 11.
    let db = test_db("claimed-interrupted").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();
    assert_lean_transition_is_legal("Request", "claimed", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Interrupted);
}

#[tokio::test]
async fn processing_interrupted_preserves_partial_response() {
    let db = test_db("processing-interrupted").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let partial_content = "partial streamed text";
    let stream_writer = DefraStreamWriter::new(db.node.clone(), AGENT_DID, Duration::ZERO);
    let response_doc_id = lifecycle
        .begin_owned_execution(&stream_writer)
        .await
        .unwrap();
    stream_writer
        .write_tokens(&response_doc_id, partial_content)
        .await
        .unwrap();
    stream_writer.flush_pending(&response_doc_id).await.unwrap();

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let stamped = stream_writer
        .write_interrupted_at(&response_doc_id, &interrupt_at)
        .await
        .unwrap();
    assert!(stamped, "expected interrupted_at to be stamped");

    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();
    assert_lean_transition_is_legal("Request", "processing", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Interrupted);

    let content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(
        content, partial_content,
        "partial content must be preserved"
    );

    let interrupted_at = fetch_response_interrupted_at(&db.node, &response_doc_id).await;
    assert_eq!(interrupted_at.as_deref(), Some(interrupt_at.as_str()));
}

#[tokio::test]
async fn input_required_interrupt_is_rejected_without_transition() {
    // `inputRequired` is reserved persisted/client vocabulary. Rust may parse
    // and display it, but the core lifecycle cannot interrupt it until Lean
    // models an external-input loop.
    let db = test_db("input-required-interrupted").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();
    set_request_lifecycle_state(&db.node, &doc_id, "inputRequired").await;

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
    let result = lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();
    assert_eq!(result, gents::lifecycle::TerminalizeResult::Lost);
    assert_lean_transition_is_illegal("Request", "inputRequired", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::InputRequired);

    for outcome in [
        RequestTerminalOutcome::Completed,
        RequestTerminalOutcome::Failed,
    ] {
        assert_eq!(
            lifecycle
                .terminalize_owned_without_stream(outcome, None)
                .await
                .unwrap(),
            gents::lifecycle::TerminalizeResult::Lost
        );
    }
    assert_lean_transition_is_illegal("Request", "inputRequired", "completed");
    assert_lean_transition_is_illegal("Request", "inputRequired", "failed");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::InputRequired);
}

#[tokio::test]
async fn pending_tie_break_prefers_interrupt_over_expire() {
    let db = test_db("tie-break-pending").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let past = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request_with_valid_until(
        &db.node,
        &request_id,
        &session_id,
        "pending",
        &created_at,
        Some(&past),
    )
    .await;
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
    assert_lean_transition_is_legal("Request", "pending", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Interrupted);
}

#[tokio::test]
async fn transition_to_interrupted_from_processing() {
    let db = test_db("processing-tie-break").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();
    assert_lean_transition_is_legal("Request", "processing", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Interrupted);
}

#[tokio::test]
async fn fail_after_interrupt_latch_prefers_interrupted() {
    let db = test_db("fail-after-interrupt-latch").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Failed, None)
        .await
        .unwrap();

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Interrupted);
    assert_lean_transition_is_legal("Request", "processing", "interrupted");
    assert_lean_transition_is_illegal("Request", "interrupted", "failed");
}

#[tokio::test]
async fn interrupt_request_is_idempotent() {
    let db = test_db("interrupt-idempotent").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let _doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    gents::interrupt_request(&db.node, &request_id)
        .await
        .expect("first interrupt should succeed");
    let after_first = gents::fetch_interrupt_requested_at(&db.node, &request_id)
        .await
        .expect("fetch after first interrupt");
    assert!(
        after_first.is_some(),
        "first interrupt should latch the field"
    );

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    gents::interrupt_request(&db.node, &request_id)
        .await
        .expect("second interrupt should be a no-op");
    let after_second = gents::fetch_interrupt_requested_at(&db.node, &request_id)
        .await
        .expect("fetch after second interrupt");
    assert_eq!(
        after_first, after_second,
        "second call must not rewrite the latched timestamp"
    );
}

#[tokio::test]
async fn interrupt_request_errors_on_unknown_request_id() {
    let db = test_db("interrupt-unknown").await;
    let err = gents::interrupt_request(&db.node, "bogus-id-that-does-not-exist").await;
    assert!(
        err.is_err(),
        "interrupting unknown request_id must error, got Ok"
    );
    let message = err.unwrap_err().to_string();
    assert!(
        message.contains("not found"),
        "error must mention not found; got: {message}"
    );
}

#[tokio::test]
async fn interrupt_on_already_terminal_is_noop() {
    let db = test_db("interrupt-terminal-noop").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();
    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Completed, None)
        .await
        .unwrap();

    let before = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(before.lifecycle_state, RequestLifecycleState::Completed);

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();

    let after = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        after.lifecycle_state,
        RequestLifecycleState::Completed,
        "terminal lifecycle_state must not regress"
    );
}

#[tokio::test]
async fn valid_until_is_immutable_after_claim_and_cached_value_is_preserved() {
    let db = test_db("s8-cached-at-claim").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let future = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
    let doc_id = create_request_with_valid_until(
        &db.node,
        &request_id,
        &session_id,
        "pending",
        &created_at,
        Some(&future),
    )
    .await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let much_later = (chrono::Utc::now() + chrono::Duration::hours(10)).to_rfc3339();
    let response = try_set_valid_until(&db.node, &doc_id, &much_later).await;
    assert!(
        response.has_errors(),
        "signed valid_until must reject post-create mutation"
    );

    let expected = chrono::DateTime::parse_from_rfc3339(&future)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(lifecycle.valid_until_at_claim_for_test(), Some(expected));
}

#[tokio::test]
async fn s7_interrupt_requested_at_is_latch_never_rewritten() {
    let db = test_db("s7-interrupt-latch").await;
    let t0 = "2026-04-20T12:00:00+00:00".to_string();

    let request_id_a = uuid::Uuid::new_v4().to_string();
    let session_id_a = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id_a = create_request(
        &db.node,
        &request_id_a,
        &session_id_a,
        "pending",
        &created_at,
    )
    .await;

    set_interrupt_requested_at(&db.node, &doc_id_a, &t0).await;
    let snap0 = fetch_request_snapshot_raw(&db.node, &doc_id_a).await;
    assert_eq!(snap0.interrupt_requested_at.as_deref(), Some(t0.as_str()));

    let request_a = build_request(
        doc_id_a.clone(),
        request_id_a.clone(),
        session_id_a.clone(),
        created_at.clone(),
    );
    let mut lifecycle_a = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request_a,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(
        lifecycle_a.claim().await.unwrap(),
        ClaimOutcome::Interrupted
    );

    let snap_a = fetch_request_snapshot_raw(&db.node, &doc_id_a).await;
    assert_eq!(
        snap_a.interrupt_requested_at.as_deref(),
        Some(t0.as_str()),
        "S7: interrupt_before_claim must not rewrite interrupt_requested_at"
    );

    let request_id_b = uuid::Uuid::new_v4().to_string();
    let session_id_b = uuid::Uuid::new_v4().to_string();
    let doc_id_b = create_request(
        &db.node,
        &request_id_b,
        &session_id_b,
        "pending",
        &created_at,
    )
    .await;

    let request_b = build_request(
        doc_id_b.clone(),
        request_id_b.clone(),
        session_id_b.clone(),
        created_at.clone(),
    );
    let mut lifecycle_b = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request_b,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle_b.claim().await.unwrap(), ClaimOutcome::Claimed);

    set_interrupt_requested_at(&db.node, &doc_id_b, &t0).await;
    let snap_b_pre = fetch_request_snapshot_raw(&db.node, &doc_id_b).await;
    assert_eq!(
        snap_b_pre.interrupt_requested_at.as_deref(),
        Some(t0.as_str())
    );
    lifecycle_b
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();

    let snap_b = fetch_request_snapshot_raw(&db.node, &doc_id_b).await;
    assert_eq!(
        snap_b.interrupt_requested_at.as_deref(),
        Some(t0.as_str()),
        "S7: transition_to_interrupted must not rewrite interrupt_requested_at"
    );
}

#[tokio::test]
async fn s8_valid_until_never_rewritten_by_transitions() {
    let db = test_db("s8-valid-until-preserved").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let t0 = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let doc_id = create_request_with_valid_until(
        &db.node,
        &request_id,
        &session_id,
        "pending",
        &created_at,
        Some(&t0),
    )
    .await;
    let snap0 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(snap0.valid_until.as_deref(), Some(t0.as_str()));

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let snap1 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(
        snap1.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: claim must not rewrite valid_until"
    );
    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();
    let snap2 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(
        snap2.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: begin_execution must not rewrite valid_until"
    );

    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();
    let snap3 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(
        snap3.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: transition_to_interrupted must not rewrite valid_until"
    );
}

#[tokio::test]
async fn s1_interrupted_is_terminal_subsequent_transitions_are_no_ops() {
    let db = test_db("s1-interrupted-terminal").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();

    let snap0 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap0.lifecycle_state, RequestLifecycleState::Interrupted);
    assert_lean_transition_is_illegal("Request", "interrupted", "completed");
    assert_lean_transition_is_illegal("Request", "interrupted", "failed");
    assert_lean_transition_is_illegal("Request", "interrupted", "processing");

    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();
    let snap1 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap1.lifecycle_state,
        RequestLifecycleState::Interrupted,
        "S1: repeated transition_to_interrupted must stay interrupted"
    );

    let _complete_result = lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Completed, None)
        .await;
    let snap2 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap2.lifecycle_state,
        RequestLifecycleState::Interrupted,
        "S1: complete() on interrupted must not reverse the terminal"
    );

    let _fail_result = lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Failed, None)
        .await;
    let snap3 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap3.lifecycle_state,
        RequestLifecycleState::Interrupted,
        "S1: fail() on interrupted must not reverse the terminal"
    );
}

#[tokio::test]
async fn ordering_response_interrupted_at_before_request_lifecycle_flip() {
    // The 6-step interrupt flow writes `AgentResponse.interrupted_at` BEFORE
    // `AgentRequest.lifecycle_state=interrupted`, per the spec's persistence-
    // ordering invariant: any subscriber observing the terminal lifecycle
    // also observes the marked partial response.
    //
    // DefraDB doesn't expose commit timestamps at query time, so we assert
    // the weaker observable: after the handler returns, BOTH writes exist.
    // This protects against the regression where the lifecycle flips but
    // `interrupted_at` is null. A stronger ordering assertion requires a
    // subscription-based observer (covered end-to-end in Task 11).

    let db = test_db("ordering-response-before-request").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let partial_content = "Hello wor";
    let stream_writer = DefraStreamWriter::new(db.node.clone(), AGENT_DID, Duration::ZERO);
    let response_doc_id = lifecycle
        .begin_owned_execution(&stream_writer)
        .await
        .unwrap();
    stream_writer
        .write_tokens(&response_doc_id, partial_content)
        .await
        .unwrap();
    stream_writer.flush_pending(&response_doc_id).await.unwrap();

    let intent_at = chrono::Utc::now().to_rfc3339();
    let stamped = stream_writer
        .write_interrupted_at(&response_doc_id, &intent_at)
        .await
        .unwrap();
    assert!(stamped, "ordering: interrupted_at must be stamped");
    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Interrupted, Some("interrupted"))
        .await
        .unwrap();

    let response_interrupted_at = fetch_response_interrupted_at(&db.node, &response_doc_id).await;
    let request_snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        request_snap.lifecycle_state,
        RequestLifecycleState::Interrupted
    );
    assert_eq!(
        response_interrupted_at.as_deref(),
        Some(intent_at.as_str()),
        "ordering: if request.lifecycle_state=interrupted, response.interrupted_at must also be set"
    );
    let response_content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(response_content, partial_content);
}

#[test]
fn conformance_mapping_all_10_lifecycle_states_round_trip() {
    use gents_protocol::client_protocol::RequestLifecycleState;

    let lean_states = lean_vocabulary_values("RequestState");
    assert_eq!(
        lean_states.len(),
        10,
        "RequestState contract should be finite"
    );
    for s in lean_states {
        let parsed = RequestLifecycleState::try_from(s)
            .unwrap_or_else(|e| panic!("failed to parse '{}': {:?}", s, e));
        assert_eq!(
            parsed.as_str(),
            s,
            "as_str must round-trip to the source string"
        );
    }
    assert_eq!(
        RequestLifecycleState::try_from("inputRequired")
            .expect("reserved vocabulary should parse")
            .as_str(),
        "inputRequired"
    );

    assert!(RequestLifecycleState::try_from("bogus").is_err());
    assert!(RequestLifecycleState::try_from("").is_err());
    assert!(RequestLifecycleState::try_from("INTERRUPTED").is_err());
}

#[test]
fn conformance_interrupted_lifecycle_maps_to_interrupted_client_turn() {
    use gents_protocol::client_protocol::{
        derive_attempt, AttemptView, ClientTurnState, RequestLifecycleState, RequestSnapshot,
    };

    let view = AttemptView {
        request: RequestSnapshot {
            request_id: "r1".into(),
            retry_parent_request: None,
            lifecycle_state: RequestLifecycleState::Interrupted,
            is_superseded: false,
        },
        response: None,
    };
    assert_eq!(derive_attempt(&view), ClientTurnState::Interrupted);
    assert!(ClientTurnState::Interrupted.is_terminal());
    assert_eq!(ClientTurnState::Interrupted.rank(), 2);
}

#[tokio::test]
async fn manual_run_materializes_pending_request() {
    let db = test_db("manual-run-materializes-pending").await;

    let doc_id = write_manual_agent_request(
        &db.node,
        db.node_identity.did(),
        AGENT_NAME,
        "task-manual-pending",
        "manual prompt body",
        serde_json::json!({}),
    )
    .await
    .expect("write_manual_agent_request should succeed on a fresh node");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap.lifecycle_state, RequestLifecycleState::Pending,
        "manual run must persist lifecycle_state=pending for the intake watcher to claim (not claimed)"
    );
    assert!(
        !snap.claimed_at_present,
        "pending manual row must NOT have claimed_at set"
    );
    assert!(
        !snap.deadline_present,
        "pending manual row must NOT have a deadline — claim sets it"
    );
    assert_eq!(
        snap.execution_origin, "interactive",
        "manual runs inherit the interactive execution origin"
    );
    assert_eq!(snap.behavior_id, AGENT_NAME);

    let lineage = fetch_request_lineage_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        lineage,
        RequestLineageSnapshot {
            caused_by_trigger_id: None,
            caused_by_trigger_kind: Some("manual".to_string()),
        },
        "manual helper must set (null, \"manual\") on the pending row"
    );
}

#[tokio::test]
async fn manual_run_preserves_lineage_through_claim_transition() {
    let db = test_db("manual-run-lineage-through-claim").await;

    let doc_id = write_manual_agent_request(
        &db.node,
        db.node_identity.did(),
        AGENT_NAME,
        "task-manual-claim",
        "manual prompt body",
        serde_json::json!({}),
    )
    .await
    .expect("write_manual_agent_request should succeed");

    let pre_claim_lineage = fetch_request_lineage_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        pre_claim_lineage,
        RequestLineageSnapshot {
            caused_by_trigger_id: None,
            caused_by_trigger_kind: Some("manual".to_string()),
        }
    );
    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id)
            .await
            .lifecycle_state,
        RequestLifecycleState::Pending
    );

    let escaped_doc_id = escape_graphql_string(&doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                created_at
            }}
        }}"#
    );
    let resp = db.node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "AgentRequest query failed: {:?}",
        resp.errors
    );
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("manual AgentRequest row exists");
    let request_id = row
        .get("request_id")
        .and_then(|v| v.as_str())
        .expect("request_id present")
        .to_string();
    let session_id = row
        .get("session_id")
        .and_then(|v| v.as_str())
        .expect("session_id present")
        .to_string();
    let created_at = row
        .get("created_at")
        .and_then(|v| v.as_str())
        .expect("created_at present")
        .to_string();

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
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
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed,
        "manual pending row must be claimable exactly once"
    );
    assert_lean_transition_is_legal("Request", "pending", "claimed");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Claimed);
    assert!(snap.claimed_at_present, "claim must stamp claimed_at");
    assert!(snap.deadline_present, "claim must stamp deadline");
    assert_eq!(
        snap.execution_origin, "interactive",
        "claim must not rewrite execution_origin"
    );

    let post_claim_lineage = fetch_request_lineage_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        post_claim_lineage,
        RequestLineageSnapshot {
            caused_by_trigger_id: None,
            caused_by_trigger_kind: Some("manual".to_string()),
        },
        "Pending → Claimed transition must preserve the (null, \"manual\") lineage tuple"
    );
    assert_eq!(
        post_claim_lineage, pre_claim_lineage,
        "lineage must be byte-identical before and after claim"
    );
}
