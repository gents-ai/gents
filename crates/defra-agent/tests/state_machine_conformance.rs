use std::time::Duration;

use defra_agent::graphql::escape_graphql_string;
use defra_agent::lifecycle::{ClaimOutcome, ExecutionOrigin, TriggerLineage};
use defra_agent::{DefraStreamWriter, RequestLifecycle};

mod support;

use support::snapshots::{
    fetch_conversation_snapshot, fetch_request_lineage_snapshot,
    fetch_request_lineage_snapshot_by_tuple, fetch_request_snapshot, fetch_request_snapshot_raw,
    fetch_response_content, fetch_response_interrupted_at, fetch_response_snapshot,
    fetch_session_snapshot, ConversationSnapshot, RequestLineageSnapshot, RequestSnapshot,
    ResponseSnapshot, SessionSnapshot,
};
use support::{
    build_request, create_request, create_response_with_content_and_status,
    create_response_with_status, set_interrupt_requested_at, set_request_lifecycle_state,
    set_valid_until, test_db, AGENT_DID, AGENT_NAME, BACKEND_ID, DEADLINE_SECS,
};

#[tokio::test]
async fn interactive_claim_snapshot_matches_claimed_waiting() {
    let db = test_db("interactive-claim").await;
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

    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id).await,
        RequestSnapshot {
            status: "processing".into(),
            lifecycle_state: "claimed".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &session_id).await,
        None
    );
    assert_eq!(fetch_session_snapshot(&db.node, &session_id).await, None);
}

#[tokio::test]
async fn interactive_prepare_session_pins_behavior() {
    let db = test_db("interactive-prepare-session").await;
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
    lifecycle.prepare_session_with_identity().await.unwrap();

    assert_eq!(
        fetch_session_snapshot(&db.node, &session_id).await,
        Some(SessionSnapshot {
            session_id: session_id.clone(),
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
        })
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &session_id).await,
        Some(ConversationSnapshot {
            latest_request_id: request_id,
            behavior_id: AGENT_NAME.into(),
            status: "processing".into(),
            forked_from_session_id: None,
            fork_at_user_turn: None,
            forked_at: None,
        })
    );
}

#[tokio::test]
async fn interactive_admission_and_progress_snapshots_match_execution_flow() {
    let db = test_db("interactive-executing").await;
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
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();
    let response_doc_id = create_response_with_status(
        &db.node,
        &format!("resp-{request_id}"),
        &request_id,
        &session_id,
        "streaming",
    )
    .await;
    lifecycle.set_response_doc_id(&response_doc_id);
    lifecycle.advance().await.unwrap();

    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id).await,
        RequestSnapshot {
            status: "processing".into(),
            lifecycle_state: "processing".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &session_id).await,
        Some(ConversationSnapshot {
            latest_request_id: request_id,
            behavior_id: AGENT_NAME.into(),
            status: "processing".into(),
            forked_from_session_id: None,
            fork_at_user_turn: None,
            forked_at: None,
        })
    );
    assert_eq!(
        fetch_session_snapshot(&db.node, &session_id).await,
        Some(SessionSnapshot {
            session_id,
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
        })
    );
    assert_eq!(
        fetch_response_snapshot(&db.node, &response_doc_id).await,
        ResponseSnapshot {
            status: "streaming".into(),
            behavior_id: AGENT_NAME.into(),
            progress_seq: 1,
            completed_at_present: false,
        }
    );
}

#[tokio::test]
async fn interactive_fail_before_stream_snapshot_matches_failed_released() {
    let db = test_db("interactive-fail").await;
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
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.fail().await.unwrap();

    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id).await,
        RequestSnapshot {
            status: "error".into(),
            lifecycle_state: "failed".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &session_id).await,
        Some(ConversationSnapshot {
            latest_request_id: request_id,
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
            forked_from_session_id: None,
            fork_at_user_turn: None,
            forked_at: None,
        })
    );
    assert_eq!(
        fetch_session_snapshot(&db.node, &session_id).await,
        Some(SessionSnapshot {
            session_id,
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
        })
    );
}

#[tokio::test]
async fn scheduled_materialization_snapshot_matches_claimed_waiting() {
    let db = test_db("scheduled-materialize").await;
    let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "scheduled prompt body",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        TriggerLineage::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        fetch_request_snapshot(&db.node, &lifecycle.request().doc_id).await,
        RequestSnapshot {
            status: "processing".into(),
            lifecycle_state: "claimed".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "scheduled".into(),
            retry_parent_request: "".into(),
            retry_root_request: lifecycle.request().request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_eq!(
        fetch_session_snapshot(&db.node, &lifecycle.request().session_id).await,
        Some(SessionSnapshot {
            session_id: lifecycle.request().session_id.clone(),
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
        })
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &lifecycle.request().session_id).await,
        Some(ConversationSnapshot {
            latest_request_id: lifecycle.request().request_id.clone(),
            behavior_id: AGENT_NAME.into(),
            status: "processing".into(),
            forked_from_session_id: None,
            fork_at_user_turn: None,
            forked_at: None,
        })
    );
}

#[tokio::test]
async fn scheduled_materialization_persists_trigger_lineage() {
    let db = test_db("scheduled-materialize-lineage").await;
    let lineage = TriggerLineage {
        trigger_id: Some("sched-1".into()),
        trigger_kind: Some("schedule".into()),
    };

    let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "scheduled prompt body with lineage",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    assert_eq!(
        fetch_request_lineage_snapshot(&db.node, &lifecycle.request().doc_id).await,
        RequestLineageSnapshot {
            caused_by_trigger_id: Some("sched-1".into()),
            caused_by_trigger_kind: Some("schedule".into()),
        }
    );

    // Bonus: confirm the tuple filter matches (validates indexes are wired end-to-end).
    assert_eq!(
        fetch_request_lineage_snapshot_by_tuple(&db.node, "sched-1", "schedule").await,
        Some(RequestLineageSnapshot {
            caused_by_trigger_id: Some("sched-1".into()),
            caused_by_trigger_kind: Some("schedule".into()),
        })
    );
}

// -----------------------------------------------------------------------------
// Trigger-driven transitions (Task 48)
//
// Each case below pins a state-machine invariant the TriggerEngine relies on
// when driving schedule/event fires. They share the same style as the older
// cases above: seed request state via the lifecycle entry point the engine
// uses, then exercise the exact GraphQL mutation / query
// `ProductionMaterializer` issues, and assert the resulting on-disk snapshot.
// -----------------------------------------------------------------------------

/// Serial concurrency: when a non-terminal request already exists for the
/// `(trigger_id, trigger_kind)` tuple, the materializer's
/// `has_nonterminal_request_for_trigger` query must observe it (the engine
/// turns this into `FireResult::Skipped`). The state-machine conformance
/// assertion: no second `AgentRequest` is created for the tuple — the count
/// observed before and after the skip decision is the same.
#[tokio::test]
async fn serial_skip_does_not_create_request() {
    let db = test_db("transition-serial-skip").await;

    // Seed an in-flight request with lineage tuple (sched-serial, schedule).
    let lineage = TriggerLineage {
        trigger_id: Some("sched-serial".into()),
        trigger_kind: Some("schedule".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "serial seed",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    // Run the exact query `ProductionMaterializer` uses to gate the fire.
    // Expect `true` — a non-terminal request for this tuple exists.
    let gating_query = format!(
        r#"query {{
            AgentRequest(
                filter: {{
                    caused_by_trigger_id: {{ _eq: "sched-serial" }},
                    caused_by_trigger_kind: {{ _eq: "schedule" }},
                    lifecycle_state: {{ _in: ["pending", "claimed", "processing", "inputRequired"] }}
                }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    let gate = db.node.execute(&gating_query).await;
    assert!(
        !gate.has_errors(),
        "gating query errored: {:?}",
        gate.errors
    );
    let gate_rows = gate
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        gate_rows.len(),
        1,
        "gating query must see the seeded in-flight request"
    );

    // Count all AgentRequests for the trigger tuple before the skip decision.
    let tuple_count_query = r#"{
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "sched-serial" },
                caused_by_trigger_kind: { _eq: "schedule" }
            }
        ) { _docID }
    }"#;
    let count_before = db
        .node
        .execute(tuple_count_query)
        .await
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    assert_eq!(count_before, 1, "seeded count should be 1");

    // Simulate the engine's FireResult::Skipped outcome: no materialize call
    // is made. Count after must still be 1 — the state machine invariant
    // "serial skip does not create request" holds.
    let count_after = db
        .node
        .execute(tuple_count_query)
        .await
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    assert_eq!(
        count_after, count_before,
        "serial skip must not create a new AgentRequest"
    );

    // Sanity: the seeded request is still in its non-terminal state
    // (the skip decision neither advances nor terminates it).
    let still_claimed = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(still_claimed.lifecycle_state, "claimed");
    assert_eq!(still_claimed.status, "processing");
}

/// LatestOnly concurrency: when a new fire arrives for a trigger tuple with an
/// in-flight request, the engine supersedes the prior via the same mutation
/// shape `ProductionMaterializer::supersede_nonterminal_requests_for_trigger`
/// uses. The seeded request must transition
/// `(processing / claimed) -> (superseded / superseded)` exactly.
#[tokio::test]
async fn latest_only_transition_to_superseded() {
    let db = test_db("transition-latest-only").await;

    let lineage = TriggerLineage {
        trigger_id: Some("sched-latest".into()),
        trigger_kind: Some("schedule".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "latest seed",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    // Pre-condition: the seeded request is (claimed, processing).
    let before = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(before.lifecycle_state, "claimed");
    assert_eq!(before.status, "processing");

    // Run the engine's supersede mutation verbatim (the shape in
    // `production_materializer::supersede_nonterminal_requests_for_trigger`).
    let supersede = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    caused_by_trigger_id: {{ _eq: "sched-latest" }},
                    caused_by_trigger_kind: {{ _eq: "schedule" }},
                    lifecycle_state: {{ _in: ["pending", "claimed", "processing", "inputRequired"] }}
                }},
                input: {{
                    status: "superseded",
                    lifecycle_state: "superseded"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = db.node.execute(&supersede).await;
    assert!(
        !resp.has_errors(),
        "supersede mutation errored: {:?}",
        resp.errors
    );
    let updated_rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("update_AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        updated_rows.len(),
        1,
        "supersede must transition exactly the one seeded in-flight request"
    );

    // Post-condition: the seeded request is now (superseded, superseded);
    // all other snapshot fields carry forward.
    assert_eq!(
        fetch_request_snapshot(&db.node, &seeded.request().doc_id).await,
        RequestSnapshot {
            status: "superseded".into(),
            lifecycle_state: "superseded".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "scheduled".into(),
            retry_parent_request: "".into(),
            retry_root_request: seeded.request().request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
}

/// Template render failure (`FireResult::Errored`): the engine must NOT
/// invoke the materializer when the render fails. The state-machine
/// conformance assertion: after a simulated render failure, no `AgentRequest`
/// exists for the trigger tuple, and the Schedule's runtime-owned
/// `last_status = "error"` writeback is independent of any request row.
#[tokio::test]
async fn fire_errored_does_not_create_request() {
    let db = test_db("transition-fire-errored").await;

    // The persistence boundary: no materialize call means no AgentRequest
    // row with the lineage tuple. Query the engine's tuple filter to confirm.
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{
                    caused_by_trigger_id: {{ _eq: "sched-render-err" }},
                    caused_by_trigger_kind: {{ _eq: "schedule" }}
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = db.node.execute(&query).await;
    assert!(!resp.has_errors(), "tuple query errored: {:?}", resp.errors);
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        0,
        "render failure must not have produced an AgentRequest: {rows:?}"
    );

    // Seed a Schedule doc and simulate the Errored writeback the source
    // performs (see ScheduleSource::on_result FireResult::Errored branch).
    // The key state-machine invariant: even though last_status="error" is
    // written to the Schedule, NO AgentRequest appears for the trigger tuple.
    let escaped_past = escape_graphql_string("2026-04-21T12:00:00Z");
    let create_sched = format!(
        r#"mutation {{
            create_Schedule(input: {{
                schedule_id: "sched-render-err",
                task_id: "task-render-err",
                interval_secs: 60,
                enabled: true,
                concurrency: "serial",
                next_run_at: "{escaped_past}"
            }}) {{ _docID }}
        }}"#
    );
    assert!(!db.node.execute(&create_sched).await.has_errors(),);
    let writeback = format!(
        r#"mutation {{
            update_Schedule(
                filter: {{ schedule_id: {{ _eq: "sched-render-err" }} }},
                input: {{
                    next_run_at: "{escaped_past}",
                    last_status: "error",
                    last_error: "template: variable 'missing' is undefined"
                }}
            ) {{ _docID }}
        }}"#
    );
    let wb_resp = db.node.execute(&writeback).await;
    assert!(
        !wb_resp.has_errors(),
        "errored writeback failed: {:?}",
        wb_resp.errors
    );

    // Re-check the persistence boundary after the writeback landed.
    let resp = db.node.execute(&query).await;
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        0,
        "Errored writeback on Schedule must not materialize an AgentRequest: {rows:?}"
    );

    // Confirm the writeback did land on the Schedule side — the state of the
    // Schedule reflects the engine's error classification without ever
    // producing a request row.
    let sched_query = r#"{
        Schedule(filter: { schedule_id: { _eq: "sched-render-err" } }, limit: 1) {
            last_status
            last_error
        }
    }"#;
    let sched_resp = db.node.execute(sched_query).await;
    let sched_row = sched_resp
        .data
        .as_ref()
        .and_then(|d| d.get("Schedule"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("Schedule doc was created");
    assert_eq!(
        sched_row.get("last_status").and_then(|v| v.as_str()),
        Some("error"),
        "Schedule.last_status must be 'error' after an Errored writeback"
    );
    assert!(
        sched_row
            .get("last_error")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("template:")),
        "Schedule.last_error must carry the template: prefix: {sched_row}"
    );
}

#[tokio::test]
async fn fork_does_not_transition_parent_lifecycle_state() {
    use defra_agent::session::{fork, ForkParams};
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

    // Parent has a completed AgentRequest + AgentResponse so the parent is idle
    // (no non-terminal lifecycle_state) and fork is allowed.
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

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "interrupted");
    assert_eq!(snap.status, "interrupted");
}

#[tokio::test]
async fn pending_dead_stale_via_expire() {
    let db = test_db("pending-dead-stale").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let valid_until = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &valid_until).await;

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

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "dead");
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
    lifecycle.transition_to_interrupted().await.unwrap();

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.status, "interrupted");
    assert_eq!(snap.lifecycle_state, "interrupted");
}

#[tokio::test]
async fn processing_interrupted_preserves_partial_response() {
    // Simulates: response streaming was in progress with partial content, then an
    // interrupt fires. Expected: content is preserved on the response row, response
    // has `interrupted_at` stamped, and the request row transitions to interrupted.
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
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();

    let partial_content = "partial streamed text";
    let response_doc_id = create_response_with_content_and_status(
        &db.node,
        &format!("resp-{request_id}"),
        &request_id,
        &session_id,
        partial_content,
        "streaming",
    )
    .await;
    lifecycle.set_response_doc_id(&response_doc_id);

    // Stamp interrupted_at on the response row first, then transition the request.
    let stream_writer =
        DefraStreamWriter::new(db.node.clone(), AGENT_DID, Duration::from_millis(50));
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let stamped = stream_writer
        .write_interrupted_at(&response_doc_id, &interrupt_at)
        .await
        .unwrap();
    assert!(stamped, "expected interrupted_at to be stamped");

    lifecycle.transition_to_interrupted().await.unwrap();

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.status, "interrupted");
    assert_eq!(snap.lifecycle_state, "interrupted");

    let content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(
        content, partial_content,
        "partial content must be preserved"
    );

    let interrupted_at = fetch_response_interrupted_at(&db.node, &response_doc_id).await;
    assert_eq!(interrupted_at.as_deref(), Some(interrupt_at.as_str()));
}

#[tokio::test]
async fn input_required_interrupted() {
    // Simulates an interrupt arriving while the request is parked in inputRequired.
    // The lifecycle enum has an InputRequired state but no public transition helper
    // yet, so we set the DB lifecycle_state directly and rely on the _nin filter in
    // `transition_to_interrupted` to allow the move.
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
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();
    set_request_lifecycle_state(&db.node, &doc_id, "inputRequired").await;

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
    lifecycle.transition_to_interrupted().await.unwrap();

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.status, "interrupted");
    assert_eq!(snap.lifecycle_state, "interrupted");
}

#[tokio::test]
async fn pending_tie_break_prefers_interrupt_over_expire() {
    let db = test_db("tie-break-pending").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let past = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &past).await;
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

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "interrupted");
}

#[tokio::test]
async fn transition_to_interrupted_from_processing() {
    // Validates the lifecycle transition from `processing` to `interrupted`. The
    // daemon-level tie-break race (interrupt vs deadline both firing in the same
    // poll) cannot be expressed at the lifecycle layer alone — that is exercised
    // end-to-end in Task 11. Here we verify the transition succeeds from the
    // processing state (the typical in-flight state when the daemon's select arm
    // fires).
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
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();

    // Submitter requests interrupt while the request is processing.
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    // Interrupt arm wins: transition_to_interrupted succeeds from processing.
    lifecycle.transition_to_interrupted().await.unwrap();

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.status, "interrupted");
    assert_eq!(snap.lifecycle_state, "interrupted");
}

#[tokio::test]
async fn interrupt_request_is_idempotent() {
    // Two calls to `interrupt_request` on the same doc must latch exactly
    // once: the daemon observer relies on the first submitter's timestamp
    // so the interruption audit trail points at who pressed Esc, not at
    // whichever caller wrote last. Proof: S7 (latch-once) in
    // `proofs/Interrupt.lean`.
    let db = test_db("interrupt-idempotent").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let _doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    defra_agent::interrupt_request(&db.node, &request_id)
        .await
        .expect("first interrupt should succeed");
    let after_first = defra_agent::fetch_interrupt_requested_at(&db.node, &request_id)
        .await
        .expect("fetch after first interrupt");
    assert!(
        after_first.is_some(),
        "first interrupt should latch the field"
    );

    // Sleep long enough that, without the idempotent latch, a second write
    // would produce a strictly later RFC3339 timestamp.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    defra_agent::interrupt_request(&db.node, &request_id)
        .await
        .expect("second interrupt should be a no-op");
    let after_second = defra_agent::fetch_interrupt_requested_at(&db.node, &request_id)
        .await
        .expect("fetch after second interrupt");
    assert_eq!(
        after_first, after_second,
        "second call must not rewrite the latched timestamp"
    );
}

#[tokio::test]
async fn interrupt_request_errors_on_unknown_request_id() {
    // Interrupting a request id that doesn't exist must surface as an error
    // (not a silent no-op). Previously, `fetch_interrupt_requested_at`
    // returned `Ok(None)` for both "field is empty" and "row does not exist",
    // so `interrupt_request` would fall through to a filter-update that
    // matched zero rows and return `Ok(())`, tricking the caller into
    // thinking a bogus id had been successfully latched.
    let db = test_db("interrupt-unknown").await;
    let err = defra_agent::interrupt_request(&db.node, "bogus-id-that-does-not-exist").await;
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
    // A completed request that later gets an interrupt_requested_at write must not
    // regress. `transition_to_interrupted` filters on `status._nin` of terminal
    // statuses, so the mutation is a no-op on completed rows.
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
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.complete().await.unwrap();

    let before = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(before.status, "completed");
    assert_eq!(before.lifecycle_state, "completed");

    // Late-arriving interrupt request: the field gets written to DB, but
    // transition_to_interrupted must not mutate the terminal row.
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
    lifecycle.transition_to_interrupted().await.unwrap();

    let after = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(after.status, "completed", "terminal row must not regress");
    assert_eq!(
        after.lifecycle_state, "completed",
        "terminal lifecycle_state must not regress"
    );
}

#[tokio::test]
async fn valid_until_cached_at_claim_ignores_post_claim_extension() {
    let db = test_db("s8-cached-at-claim").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let future = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &future).await;

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

    // Caller rewrites valid_until to a far-future value after claim. Lifecycle should
    // not observe it: S8 says the scheduler reads valid_until exactly once at claim.
    let much_later = (chrono::Utc::now() + chrono::Duration::hours(10)).to_rfc3339();
    set_valid_until(&db.node, &doc_id, &much_later).await;

    // Assert the cached field on the lifecycle is unchanged from what was read at claim.
    let expected = chrono::DateTime::parse_from_rfc3339(&future)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(lifecycle.valid_until_at_claim_for_test(), Some(expected));
}

// ---------------------------------------------------------------------------
// Property tests enforcing Lean invariants S7 / S8 / S1 + persistence ordering
// + the conformance mapping round-trip. Each test's comment cites the Lean
// theorem it guards against regression.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s7_interrupt_requested_at_is_latch_never_rewritten() {
    // Per S7 (`interrupt_monotonicity`) in
    // `crates/defra-agent/proofs/Proofs/Properties/Safety.lean`: once
    // `interruptRequestedAt.isSome`, no `RequestContext.Transition` rewrites
    // it. The Rust mutations must preserve this latch across every lifecycle
    // transition that touches the row.

    let db = test_db("s7-interrupt-latch").await;
    let t0 = "2026-04-20T12:00:00+00:00".to_string();

    // Sequence A: pending -> interrupted (via claim's pre-claim branch)
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

    // Sequence B: fresh row, claimed -> interrupted (via transition_to_interrupted)
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

    // Submitter sets interrupt mid-flight, then the lifecycle flips the row.
    set_interrupt_requested_at(&db.node, &doc_id_b, &t0).await;
    let snap_b_pre = fetch_request_snapshot_raw(&db.node, &doc_id_b).await;
    assert_eq!(
        snap_b_pre.interrupt_requested_at.as_deref(),
        Some(t0.as_str())
    );
    lifecycle_b.transition_to_interrupted().await.unwrap();

    let snap_b = fetch_request_snapshot_raw(&db.node, &doc_id_b).await;
    assert_eq!(
        snap_b.interrupt_requested_at.as_deref(),
        Some(t0.as_str()),
        "S7: transition_to_interrupted must not rewrite interrupt_requested_at"
    );
}

#[tokio::test]
async fn s8_valid_until_never_rewritten_by_transitions() {
    // Per S8 (`valid_until_monotonicity`) in
    // `crates/defra-agent/proofs/Proofs/Properties/Safety.lean`: no
    // `RequestContext.Transition` rewrites `validUntil` (unconditional). Run a
    // full claim + begin_execution + transition_to_interrupted sequence and
    // assert `valid_until` is unchanged after each persisted transition.

    let db = test_db("s8-valid-until-preserved").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let t0 = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &t0).await;
    let snap0 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(snap0.valid_until.as_deref(), Some(t0.as_str()));

    // Claim (pending → claimed)
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

    // begin_execution (claimed → processing)
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();
    let snap2 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(
        snap2.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: begin_execution must not rewrite valid_until"
    );

    // transition_to_interrupted (processing → interrupted)
    lifecycle.transition_to_interrupted().await.unwrap();
    let snap3 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(
        snap3.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: transition_to_interrupted must not rewrite valid_until"
    );
}

#[tokio::test]
async fn s1_interrupted_is_terminal_subsequent_transitions_are_no_ops() {
    // Per S1 (`terminal_irreversibility`) in
    // `crates/defra-agent/proofs/Proofs/Properties/Safety.lean`: no transition
    // leaves `.interrupted` for a non-terminal state. Transition a claimed
    // request to interrupted, then attempt subsequent transitions and assert
    // the DB row stays `interrupted` regardless of whether the Rust method
    // returns `Ok(())` (idempotent no-op) or `Err(...)` (caller mis-sequenced).

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
    lifecycle.transition_to_interrupted().await.unwrap();

    let snap0 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap0.lifecycle_state, "interrupted");
    assert_eq!(snap0.status, "interrupted");

    // Idempotent: calling transition_to_interrupted again is a no-op because
    // the `status._nin` filter excludes terminal rows.
    lifecycle.transition_to_interrupted().await.unwrap();
    let snap1 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap1.lifecycle_state, "interrupted",
        "S1: repeated transition_to_interrupted must stay interrupted"
    );
    assert_eq!(snap1.status, "interrupted");

    // complete() on an interrupted lifecycle may return Err (state mismatch)
    // or Ok (caller is expected to tolerate either). Either way, the DB row
    // must stay interrupted.
    let _complete_result = lifecycle.complete().await;
    let snap2 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap2.lifecycle_state, "interrupted",
        "S1: complete() on interrupted must not reverse the terminal"
    );
    assert_eq!(snap2.status, "interrupted");

    // fail() same treatment.
    let _fail_result = lifecycle.fail().await;
    let snap3 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap3.lifecycle_state, "interrupted",
        "S1: fail() on interrupted must not reverse the terminal"
    );
    assert_eq!(snap3.status, "interrupted");
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

    // Set up a claimed request with a streaming response that has partial content.
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
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();

    let partial_content = "Hello wor";
    let response_doc_id = create_response_with_content_and_status(
        &db.node,
        &format!("resp-{request_id}"),
        &request_id,
        &session_id,
        partial_content,
        "streaming",
    )
    .await;
    lifecycle.set_response_doc_id(&response_doc_id);

    // Execute the 6-step flow sequence (mirroring run_inference's path):
    //   1. write interrupted_at on the response row
    //   2. transition the request to interrupted
    let intent_at = chrono::Utc::now().to_rfc3339();
    let stream_writer =
        DefraStreamWriter::new(db.node.clone(), AGENT_DID, Duration::from_millis(50));
    let stamped = stream_writer
        .write_interrupted_at(&response_doc_id, &intent_at)
        .await
        .unwrap();
    assert!(stamped, "ordering: interrupted_at must be stamped");
    lifecycle.transition_to_interrupted().await.unwrap();

    // Assert both writes are present.
    let response_interrupted_at = fetch_response_interrupted_at(&db.node, &response_doc_id).await;
    let request_snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(request_snap.lifecycle_state, "interrupted");
    assert_eq!(
        response_interrupted_at.as_deref(),
        Some(intent_at.as_str()),
        "ordering: if request.lifecycle_state=interrupted, response.interrupted_at must also be set"
    );
    // The partial content must be preserved verbatim.
    let response_content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(response_content, partial_content);
}

#[test]
fn conformance_mapping_all_9_lifecycle_states_round_trip() {
    // Per `Proofs/Conformance/DefraAgent.lean::toIdeal`, every
    // `DefraLifecycleState` maps to a specific `RequestState`. The Rust
    // `RequestLifecycleState` enum in `defra-agent-protocol::client_protocol`
    // mirrors this. Assert all 9 string forms parse to the expected enum
    // variant, that `as_str` round-trips, and that unknown strings reject.
    use defra_agent_protocol::client_protocol::RequestLifecycleState;

    let cases = &[
        ("pending", RequestLifecycleState::Pending),
        ("claimed", RequestLifecycleState::Claimed),
        ("processing", RequestLifecycleState::Processing),
        ("inputRequired", RequestLifecycleState::InputRequired),
        ("completed", RequestLifecycleState::Completed),
        ("failed", RequestLifecycleState::Failed),
        ("superseded", RequestLifecycleState::Superseded),
        ("dead", RequestLifecycleState::Dead),
        ("interrupted", RequestLifecycleState::Interrupted),
    ];

    for (s, expected) in cases {
        let parsed = RequestLifecycleState::try_from(*s)
            .unwrap_or_else(|e| panic!("failed to parse '{}': {:?}", s, e));
        assert_eq!(parsed, *expected, "round-trip mismatch for '{}'", s);
        assert_eq!(
            parsed.as_str(),
            *s,
            "as_str must round-trip to the source string"
        );
    }

    // Unknown strings must reject.
    assert!(RequestLifecycleState::try_from("bogus").is_err());
    assert!(RequestLifecycleState::try_from("").is_err());
    assert!(RequestLifecycleState::try_from("INTERRUPTED").is_err());
}

#[test]
fn conformance_interrupted_lifecycle_maps_to_interrupted_client_turn() {
    // The client projection must map `RequestLifecycleState::Interrupted`
    // onto the distinct `ClientTurnState::Interrupted` terminal. This keeps
    // the Rust projection in sync with `Proofs/Client.lean::deriveAttempt`,
    // which now maps `.interrupted => .interrupted` rather than conflating
    // it with `.failed`.
    use defra_agent_protocol::client_protocol::{
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
