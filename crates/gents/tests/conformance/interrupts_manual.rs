use super::*;
use gents::request::lifecycle::RequestTerminalOutcome;
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[tokio::test]
async fn fork_does_not_transition_parent_lifecycle_state() {
    use gents::session::{fork_on, ForkParams};
    use gents_protocol::output::MessageRole;
    use support::agent_transcript;

    let db = test_db("fork-no-lifecycle-transition").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;

    let parent_session = uuid::Uuid::new_v4().to_string();
    let parent = agent_transcript::source(&actors, &parent_session).await;
    let request_doc_id = parent.request_doc.clone();
    agent_transcript::import(&actors, &parent, 1, MessageRole::User, "u1", true).await;
    agent_transcript::import(&actors, &parent, 2, MessageRole::Assistant, "a1", true).await;

    let before_request = fetch_request_snapshot(&actors, &request_doc_id).await;
    let before_output = canonical_session_output(&actors, &parent_session).await;
    let before_session = fetch_session_snapshot(&actors, &parent_session).await;
    assert_eq!(
        before_request.lifecycle_state,
        RequestLifecycleState::Completed
    );

    // The session's working Agent forks its own session.
    fork_on(
        &actors.agent_access,
        ForkParams {
            source_session_id: &parent_session,
            boundary: gents::session::ForkBoundary::UserTurn(0),
            caller_node_did: actors.node_did(),
            caller_requester_did: Some(actors.agent_did()),
            target_agent_did: None,
        },
    )
    .await
    .expect("fork succeeds on idle parent");

    let after_request = fetch_request_snapshot(&actors, &request_doc_id).await;
    let after_output = canonical_session_output(&actors, &parent_session).await;
    let after_session = fetch_session_snapshot(&actors, &parent_session).await;

    assert_eq!(
        before_request, after_request,
        "parent AgentRequest unchanged"
    );
    assert_eq!(
        before_output, after_output,
        "fork leaves the parent's immutable headers and output segments unchanged"
    );
    assert_eq!(
        before_session, after_session,
        "parent AgentSession unchanged"
    );
}

async fn canonical_session_output(
    actors: &support::agent_requests::RequestActors,
    session_id: &str,
) -> serde_json::Value {
    use gents_protocol::output::rows::{AGENT_MESSAGE_FIELDS, AGENT_OUTPUT_SEGMENT_FIELDS};
    let session_id = escape_graphql_string(session_id);
    let response = actors
        .query(&format!(
            r#"{{
                AgentMessage(filter: {{session_id: {{_eq: "{session_id}"}}}}, order: {{_docID: ASC}}) {{ {AGENT_MESSAGE_FIELDS} }}
                AgentOutputSegment(filter: {{session_id: {{_eq: "{session_id}"}}}}, order: {{_docID: ASC}}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "canonical output snapshot: {:?}",
        response.errors
    );
    let data = response.data.expect("canonical output snapshot data");
    assert_eq!(
        data["AgentMessage"].as_array().map(Vec::len),
        Some(2),
        "the parent session's history is visible to its Agent"
    );
    data
}
#[tokio::test]
async fn pending_tie_break_prefers_interrupt_over_expire() {
    let db = test_db("tie-break-pending").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let past =
        support::agent_requests::canonical_at(chrono::Utc::now() - chrono::Duration::seconds(1));
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&request_id, &session_id, &created_at)
                .state("pending")
                .valid_until(Some(&past)),
        )
        .await;
    actors
        .set_interrupt_requested_at(&doc_id, &interrupt_at)
        .await;

    let request = actors.build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = actors.lifecycle(
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
    assert_lean_transition_is_legal("Request", "pending", "interrupted");

    let snap = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Interrupted);
}
#[tokio::test]
async fn fail_after_interrupt_latch_prefers_interrupted() {
    let db = test_db("fail-after-interrupt-latch").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&request_id, &session_id, &created_at)
                .state("pending"),
        )
        .await;

    let request = actors.build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = actors.lifecycle(
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
    actors
        .set_interrupt_requested_at(&doc_id, &interrupt_at)
        .await;

    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Failed,
            TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let snap = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Interrupted);
    assert_lean_transition_is_legal("Request", "processing", "interrupted");
    assert_lean_transition_is_illegal("Request", "interrupted", "failed");
}

#[tokio::test]
async fn interrupt_request_is_idempotent() {
    let db = test_db("interrupt-idempotent").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&request_id, &session_id, &created_at)
                .state("pending"),
        )
        .await;

    gents::interrupt_request(&db.node, actors.agent().as_ref(), &request_id)
        .await
        .expect("first interrupt should succeed");
    let after_first = actors.request_row(&doc_id, "interrupt_requested_at").await
        ["interrupt_requested_at"]
        .as_str()
        .map(str::to_owned);
    assert!(
        after_first.is_some(),
        "first interrupt should latch the field"
    );

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    gents::interrupt_request(&db.node, actors.agent().as_ref(), &request_id)
        .await
        .expect("second interrupt should be a no-op");
    let after_second = actors.request_row(&doc_id, "interrupt_requested_at").await
        ["interrupt_requested_at"]
        .as_str()
        .map(str::to_owned);
    assert_eq!(
        after_first, after_second,
        "second call must not rewrite the latched timestamp"
    );
}

#[tokio::test]
async fn interrupt_request_errors_on_unknown_request_id() {
    let db = test_db("interrupt-unknown").await;
    let err = gents::interrupt_request(
        &db.node,
        support::managed_agent(&db).await.as_ref(),
        "bogus-id-that-does-not-exist",
    )
    .await;
    assert!(
        err.is_err(),
        "interrupting unknown request_id must error, got Ok"
    );
    let message = err.unwrap_err().to_string();
    assert!(
        message.contains("missing"),
        "error must report a missing scoped request; got: {message}"
    );
}

#[tokio::test]
async fn interrupt_on_already_terminal_is_noop() {
    let db = test_db("interrupt-terminal-noop").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&request_id, &session_id, &created_at)
                .state("pending"),
        )
        .await;

    let request = actors.build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = actors.lifecycle(
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
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let before = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(before.lifecycle_state, RequestLifecycleState::Completed);

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    actors
        .set_interrupt_requested_at(&doc_id, &interrupt_at)
        .await;
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Interrupted,
            TerminalOutput::NoMessage,
            Some("interrupted"),
        )
        .await
        .unwrap();

    let after = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(
        after.lifecycle_state,
        RequestLifecycleState::Completed,
        "terminal lifecycle_state must not regress"
    );
}
#[tokio::test]
async fn s7_interrupt_requested_at_is_latch_never_rewritten() {
    let db = test_db("s7-interrupt-latch").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let t0 = "2026-04-20T12:00:00+00:00".to_string();

    let request_id_a = uuid::Uuid::new_v4().to_string();
    let session_id_a = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let doc_id_a = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&request_id_a, &session_id_a, &created_at)
                .state("pending"),
        )
        .await;

    actors.set_interrupt_requested_at(&doc_id_a, &t0).await;
    let snap0 = fetch_request_snapshot_raw(&actors, &doc_id_a).await;
    assert_eq!(snap0.interrupt_requested_at.as_deref(), Some(t0.as_str()));

    let request_a = actors.build_request(
        doc_id_a.clone(),
        request_id_a.clone(),
        session_id_a.clone(),
        created_at.clone(),
    );
    let mut lifecycle_a = actors.lifecycle(
        request_a,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(
        lifecycle_a.claim().await.unwrap(),
        ClaimOutcome::Interrupted
    );

    let snap_a = fetch_request_snapshot_raw(&actors, &doc_id_a).await;
    assert_eq!(
        snap_a.interrupt_requested_at.as_deref(),
        Some(t0.as_str()),
        "S7: interrupt_before_claim must not rewrite interrupt_requested_at"
    );

    let request_id_b = uuid::Uuid::new_v4().to_string();
    let session_id_b = uuid::Uuid::new_v4().to_string();
    let doc_id_b = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&request_id_b, &session_id_b, &created_at)
                .state("pending"),
        )
        .await;

    let request_b = actors.build_request(
        doc_id_b.clone(),
        request_id_b.clone(),
        session_id_b.clone(),
        created_at.clone(),
    );
    let mut lifecycle_b = actors.lifecycle(
        request_b,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle_b.claim().await.unwrap(), ClaimOutcome::Claimed);

    actors.set_interrupt_requested_at(&doc_id_b, &t0).await;
    let snap_b_pre = fetch_request_snapshot_raw(&actors, &doc_id_b).await;
    assert_eq!(
        snap_b_pre.interrupt_requested_at.as_deref(),
        Some(t0.as_str())
    );
    lifecycle_b
        .terminalize_owned(
            RequestTerminalOutcome::Interrupted,
            TerminalOutput::NoMessage,
            Some("interrupted"),
        )
        .await
        .unwrap();

    let snap_b = fetch_request_snapshot_raw(&actors, &doc_id_b).await;
    assert_eq!(
        snap_b.interrupt_requested_at.as_deref(),
        Some(t0.as_str()),
        "S7: transition_to_interrupted must not rewrite interrupt_requested_at"
    );
}

#[tokio::test]
async fn s8_valid_until_never_rewritten_by_transitions() {
    let db = test_db("s8-valid-until-preserved").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let t0 = support::agent_requests::canonical_at(chrono::Utc::now() + chrono::Duration::hours(1));
    let doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&request_id, &session_id, &created_at)
                .state("pending")
                .valid_until(Some(&t0)),
        )
        .await;
    let snap0 = fetch_request_snapshot_raw(&actors, &doc_id).await;
    assert_eq!(snap0.valid_until.as_deref(), Some(t0.as_str()));

    let request = actors.build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = actors.lifecycle(
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let snap1 = fetch_request_snapshot_raw(&actors, &doc_id).await;
    assert_eq!(
        snap1.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: claim must not rewrite valid_until"
    );
    crate::support::begin_owned_execution(&mut lifecycle, &db.node)
        .await
        .unwrap();
    let snap2 = fetch_request_snapshot_raw(&actors, &doc_id).await;
    assert_eq!(
        snap2.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: begin_execution must not rewrite valid_until"
    );

    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Interrupted,
            TerminalOutput::NoMessage,
            Some("interrupted"),
        )
        .await
        .unwrap();
    let snap3 = fetch_request_snapshot_raw(&actors, &doc_id).await;
    assert_eq!(
        snap3.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: transition_to_interrupted must not rewrite valid_until"
    );
}

#[tokio::test]
async fn s1_interrupted_is_terminal_subsequent_transitions_are_no_ops() {
    let db = test_db("s1-interrupted-terminal").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&request_id, &session_id, &created_at)
                .state("pending"),
        )
        .await;

    let request = actors.build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = actors.lifecycle(
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Interrupted,
            TerminalOutput::NoMessage,
            Some("interrupted"),
        )
        .await
        .unwrap();

    let snap0 = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(snap0.lifecycle_state, RequestLifecycleState::Interrupted);
    assert_lean_transition_is_illegal("Request", "interrupted", "completed");
    assert_lean_transition_is_illegal("Request", "interrupted", "failed");
    assert_lean_transition_is_illegal("Request", "interrupted", "processing");

    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Interrupted,
            TerminalOutput::NoMessage,
            Some("interrupted"),
        )
        .await
        .unwrap();
    let snap1 = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(
        snap1.lifecycle_state,
        RequestLifecycleState::Interrupted,
        "S1: repeated transition_to_interrupted must stay interrupted"
    );

    let _complete_result = lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            TerminalOutput::NoMessage,
            None,
        )
        .await;
    let snap2 = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(
        snap2.lifecycle_state,
        RequestLifecycleState::Interrupted,
        "S1: complete() on interrupted must not reverse the terminal"
    );

    let _fail_result = lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Failed,
            TerminalOutput::NoMessage,
            None,
        )
        .await;
    let snap3 = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(
        snap3.lifecycle_state,
        RequestLifecycleState::Interrupted,
        "S1: fail() on interrupted must not reverse the terminal"
    );
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
    };
    assert_eq!(derive_attempt(&view), ClientTurnState::Interrupted);
    assert!(ClientTurnState::Interrupted.is_terminal());
    assert_eq!(ClientTurnState::Interrupted.rank(), 2);
}

#[tokio::test]
async fn manual_run_materializes_pending_request() {
    let db = test_db("manual-run-materializes-pending").await;
    let agent = support::managed_agent(&db).await;
    let agent_access = gents::ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();

    let doc_id = write_manual_agent_request(
        &db.node,
        ::identity::Did::new(agent.did()).expect("fixture creator DID"),
        agent.as_ref(),
        gents::request::lifecycle::ManualRequestTarget {
            node_did: db.node_identity.did(),
            agent_did: agent.did(),
            task_id: "task-manual-pending",
        },
        "manual prompt body",
        serde_json::json!({}),
    )
    .await
    .expect("write_manual_agent_request should succeed on a fresh node");

    let snap = fetch_request_snapshot(&agent_access, &doc_id).await;
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
    assert_eq!(snap.agent_did, agent.did());

    let lineage = fetch_request_lineage_snapshot(&agent_access, &doc_id).await;
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
    let agent = support::managed_agent(&db).await;
    let agent_access = gents::ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();

    let doc_id = write_manual_agent_request(
        &db.node,
        ::identity::Did::new(agent.did()).expect("fixture creator DID"),
        agent.as_ref(),
        gents::request::lifecycle::ManualRequestTarget {
            node_did: db.node_identity.did(),
            agent_did: agent.did(),
            task_id: "task-manual-claim",
        },
        "manual prompt body",
        serde_json::json!({}),
    )
    .await
    .expect("write_manual_agent_request should succeed");

    let pre_claim_lineage = fetch_request_lineage_snapshot(&agent_access, &doc_id).await;
    assert_eq!(
        pre_claim_lineage,
        RequestLineageSnapshot {
            caused_by_trigger_id: None,
            caused_by_trigger_kind: Some("manual".to_string()),
        }
    );
    assert_eq!(
        fetch_request_snapshot(&agent_access, &doc_id)
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
                requester_did
                session_id
                created_at
            }}
        }}"#
    );
    let resp = agent_access
        .execute(&query)
        .await
        .expect("AgentRequest query");
    let row = resp
        .get("data")
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

    let mut request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    request.node_did = db.node_identity.did().to_string();
    request.agent_did = agent.did().to_string();
    request.requester_did = row
        .get("requester_did")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        agent.did(),
        db.node_identity.did(),
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed,
        "manual pending row must be claimable exactly once"
    );
    assert_lean_transition_is_legal("Request", "pending", "claimed");

    let snap = fetch_request_snapshot(&agent_access, &doc_id).await;
    assert_eq!(snap.lifecycle_state, RequestLifecycleState::Claimed);
    assert!(snap.claimed_at_present, "claim must stamp claimed_at");
    assert!(snap.deadline_present, "claim must stamp deadline");
    assert_eq!(
        snap.execution_origin, "interactive",
        "claim must not rewrite execution_origin"
    );

    let post_claim_lineage = fetch_request_lineage_snapshot(&agent_access, &doc_id).await;
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

#[tokio::test]
async fn interrupt_request_drains_automated_wakeups_in_owner_scope() {
    let db = test_db("interrupt-drain-wakeups").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let session_id = "interrupt-drain-session";
    let foreign = tokio::task::spawn_blocking(|| support::fixtures::test_identity("foreign-drain"))
        .await
        .unwrap();
    let foreign_node_did = gents::NodeIdentity::did(&foreign).to_owned();
    let foreign_node_did = foreign_node_did.as_str();
    let wakeup_input = format!(
        r#"{{"queue":{{"source":"background_completion","policy":"coalesce","key":"background_completion:{session_id}","queued_after_request_id":null}}}}"#
    );
    let non_wakeup_input = r#"{"queue":{"source":"user","policy":"append","key":null,"queued_after_request_id":null}}"#;

    let parent_doc = create_pending_queue_row(
        &db,
        &actors,
        "drain-parent",
        session_id,
        actors.node_did(),
        "interactive",
        non_wakeup_input,
    )
    .await;
    actors
        .set_lifecycle_state(&parent_doc, RequestLifecycleState::Processing)
        .await;

    create_pending_queue_row(
        &db,
        &actors,
        "drain-wakeup-owner",
        session_id,
        actors.node_did(),
        "scheduled",
        &wakeup_input,
    )
    .await;
    create_pending_queue_row(
        &db,
        &actors,
        "drain-wakeup-foreign",
        session_id,
        foreign_node_did,
        "scheduled",
        &wakeup_input,
    )
    .await;
    create_pending_queue_row(
        &db,
        &actors,
        "drain-wakeup-interactive",
        session_id,
        actors.node_did(),
        "interactive",
        &wakeup_input,
    )
    .await;
    create_pending_queue_row(
        &db,
        &actors,
        "drain-scheduled-user",
        session_id,
        actors.node_did(),
        "scheduled",
        non_wakeup_input,
    )
    .await;

    gents::interrupt_request(&db.node, actors.agent().as_ref(), "drain-parent")
        .await
        .expect("interrupt_request should latch and drain");

    let latched = actors
        .request_row(&parent_doc, "interrupt_requested_at")
        .await["interrupt_requested_at"]
        .as_str()
        .map(str::to_owned);
    assert!(
        latched.is_some(),
        "interrupt_request must latch interrupt_requested_at before draining"
    );

    let drained = fetch_drain_row(&actors, "drain-wakeup-owner").await;
    assert_eq!(
        drained.lifecycle_state,
        RequestLifecycleState::Interrupted,
        "the owner's own scheduled wake-up must be drained by the interrupt owner"
    );
    assert_eq!(
        drained.failure_reason.as_deref(),
        Some("automated wake-up drained because active request was interrupted"),
        "the drain owner must record its own reason, got {:?}",
        drained.failure_reason
    );

    let foreign = fetch_drain_row(&actors, "drain-wakeup-foreign").await;
    assert_eq!(foreign.node_did, foreign_node_did);
    assert_eq!(
        foreign.lifecycle_state,
        RequestLifecycleState::Pending,
        "a foreign-node replica sharing the session must never be drained by this owner"
    );

    let interactive = fetch_drain_row(&actors, "drain-wakeup-interactive").await;
    assert_eq!(
        interactive.lifecycle_state,
        RequestLifecycleState::Pending,
        "interactive-origin rows are user turns, not automated wake-ups"
    );

    let scheduled_user = fetch_drain_row(&actors, "drain-scheduled-user").await;
    assert_eq!(
        scheduled_user.lifecycle_state,
        RequestLifecycleState::Pending,
        "scheduled rows without background-completion wake input must survive the drain"
    );
}

/// The runtime root writes the queued replica for the fixture Agent and grants
/// it as working Agent; only the node scope and execution origin differ.
async fn create_pending_queue_row(
    db: &support::TestDb,
    actors: &support::agent_requests::RequestActors,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    execution_origin: &str,
    input: &str,
) -> String {
    let input = serde_json::from_str::<Value>(input).expect("request input JSON");
    let value = json!({
        "request_id": request_id,
        "purpose": "normal",
        "node_did": node_did,
        "agent_did": actors.agent_did(),
        "session_id": session_id,
        "retry_root_request": request_id,
        "content": "queued wake-up",
        "input": input,
        "lifecycle_state": "pending",
        "execution_origin": execution_origin,
        "created_at": "2026-03-23T00:00:00Z",
        "retry_count": 0,
        "max_retries": 3,
    });
    let agent = ::identity::Did::new(actors.agent_did()).unwrap();
    gents::ConfigAccess::local_as(
        db.node.clone(),
        ::identity::Did::new(db.node_identity.did()).unwrap(),
    )
    .transact_write("test.interrupt.queue_row", |txn| {
        let (value, agent) = (&value, &agent);
        Box::pin(async move {
            let response = txn
                .execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({"input": value}),
                )
                .await?;
            let doc = gents::store::graphql::created_doc_id(&response, "AgentRequest")?;
            txn.add_actor_relationship("AgentRequest", &doc, "working_agent", agent)
                .await?;
            Ok::<_, anyhow::Error>(doc)
        })
    })
    .await
    .expect("create pending queue row as runtime root")
}

#[derive(Debug, Deserialize)]
struct DrainRow {
    lifecycle_state: RequestLifecycleState,
    #[serde(default)]
    failure_reason: Option<String>,
    node_did: String,
}

async fn fetch_drain_row(
    actors: &support::agent_requests::RequestActors,
    request_id: &str,
) -> DrainRow {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                lifecycle_state
                failure_reason
                node_did
            }}
        }}"#
    );
    let response = actors
        .agent_access
        .execute(&query)
        .await
        .expect("drain row");
    serde_json::from_value(response["data"]["AgentRequest"][0].clone()).expect("decode drain row")
}
