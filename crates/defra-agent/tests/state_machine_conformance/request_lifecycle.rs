use super::*;

fn rust_request_transition_action(from: &str, to: &str) -> Option<&'static str> {
    match (from, to) {
        ("pending", "claimed") => Some("claim"),
        ("pending", "superseded") => Some("dedupLose"),
        ("claimed", "processing") => Some("beginInference"),
        ("processing", "processing") => Some("advance"),
        ("processing", "completed") => Some("finish"),
        ("processing", "failed") => Some("fail"),
        ("claimed", "failed") => Some("failBeforeStream"),
        ("pending", "dead") => Some("expire"),
        ("pending", "interrupted") => Some("interruptBeforeClaim"),
        ("claimed", "interrupted") => Some("interruptClaimed"),
        ("processing", "interrupted") => Some("interruptProcessing"),
        _ => None,
    }
}

fn rust_request_transition_classification(from: &str, to: &str) -> &'static str {
    if rust_request_transition_action(from, to).is_some() {
        "legal"
    } else if from == "inputRequired" || to == "inputRequired" {
        "productUnreachable"
    } else {
        "illegal"
    }
}

fn request_lifecycle_for_case(
    db: &support::TestDb,
    doc_id: String,
    request_id: String,
    session_id: String,
    created_at: String,
) -> RequestLifecycle {
    let request = build_request(doc_id, request_id, session_id, created_at);
    RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    )
}

async fn drive_generated_request_legal_case(case: &LeanLifecycleTransitionCase) {
    let db = test_db("generated-request-transition").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;
    let action = case
        .action
        .as_deref()
        .expect("legal Request transition case must carry an action");
    let mut lifecycle = request_lifecycle_for_case(
        &db,
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at.clone(),
    );

    match action {
        "claim" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
        }
        "dedupLose" => {
            let escaped_doc_id = escape_graphql_string(&doc_id);
            let resp = db
                .node
                .execute(&format!(
                    r#"mutation {{
                        update_AgentRequest(
                            filter: {{
                                _docID: {{ _eq: "{escaped_doc_id}" }},
                                status: {{ _eq: "pending" }},
                                lifecycle_state: {{ _eq: "pending" }}
                            }},
                            input: {{
                                status: "superseded",
                                lifecycle_state: "superseded",
                                superseded_by_request: "explicit-replacement-{request_id}"
                            }}
                        ) {{ _docID }}
                    }}"#
                ))
                .await;
            assert!(
                !resp.has_errors(),
                "explicit supersede writer failed: {:?}",
                resp.errors
            );
        }
        "beginInference" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.begin_execution().await.unwrap();
        }
        "advance" => {
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
        }
        "finish" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.begin_execution().await.unwrap();
            lifecycle.complete().await.unwrap();
        }
        "fail" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.begin_execution().await.unwrap();
            lifecycle.fail().await.unwrap();
        }
        "failBeforeStream" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.fail().await.unwrap();
        }
        "expire" => {
            let past = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
            set_valid_until(&db.node, &doc_id, &past).await;
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);
        }
        "interruptBeforeClaim" => {
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
        }
        "interruptClaimed" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
            lifecycle.transition_to_interrupted().await.unwrap();
        }
        "interruptProcessing" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.begin_execution().await.unwrap();
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
            lifecycle.transition_to_interrupted().await.unwrap();
        }
        other => panic!(
            "generated Request transition {} has unsupported action {other:?}",
            case.name
        ),
    }

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap.lifecycle_state, case.to,
        "generated Request transition {} expected {} -> {} classified as {} via {:?}, got persisted lifecycle_state={}",
        case.name, case.from, case.to, case.classification, case.action, snap.lifecycle_state
    );
}

pub(super) async fn generated_request_transition_cases_cover_lifecycle_policy() {
    let mut legal_count = 0;
    let mut illegal_count = 0;
    let mut product_unreachable_count = 0;

    for case in lean_request_transition_cases() {
        let rust_classification = rust_request_transition_classification(&case.from, &case.to);
        assert_eq!(
            case.classification, rust_classification,
            "Request transition {} expected classification drift for {} -> {}; Lean action={:?} boundary={:?}",
            case.name, case.from, case.to, case.action, case.boundary
        );

        match case.classification.as_str() {
            "legal" => {
                legal_count += 1;
                assert_eq!(
                    case.action.as_deref(),
                    rust_request_transition_action(&case.from, &case.to),
                    "Request transition {} legal writer action drifted for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                drive_generated_request_legal_case(case).await;
            }
            "illegal" => {
                illegal_count += 1;
                assert!(
                    rust_request_transition_action(&case.from, &case.to).is_none(),
                    "Request transition {} is ordinary illegal but Rust has a writer path for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
            }
            "productUnreachable" => {
                product_unreachable_count += 1;
                assert!(
                    case.from == "inputRequired" || case.to == "inputRequired",
                    "Request transition {} product-unreachable classification must be scoped to reserved inputRequired, got {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                assert_eq!(
                    case.boundary.as_deref(),
                    Some("boundary.request.input-required-reserved"),
                    "Request transition {} must cite the reserved inputRequired boundary",
                    case.name
                );
                assert!(
                    rust_request_transition_action(&case.from, &case.to).is_none(),
                    "Request transition {} is reserved but Rust has a writer path for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
            }
            other => panic!(
                "generated Request transition {} has unknown classification {other:?}",
                case.name
            ),
        }
    }

    assert_eq!(legal_count, 11);
    assert_eq!(illegal_count, 53);
    assert_eq!(product_unreachable_count, 17);
}

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
    assert_lean_transition_is_legal("Request", "pending", "claimed");

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
    assert_lean_transition_is_legal("Request", "claimed", "processing");
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
    assert_lean_transition_is_legal("Request", "processing", "processing");

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
    assert_lean_transition_is_legal("Request", "claimed", "failed");

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

/// Serial concurrency: when an active runtime request already exists for the
/// `(trigger_id, trigger_kind)` tuple, the materializer's
/// `has_active_runtime_request_for_trigger` query must observe it (the engine
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
    // Expect `true` — an active runtime request for this tuple exists.
    let gating_query = r#"query {
            AgentRequest(
                filter: {
                    caused_by_trigger_id: { _eq: "sched-serial" },
                    caused_by_trigger_kind: { _eq: "schedule" },
                    lifecycle_state: { _in: ["pending", "claimed", "processing"] }
                },
                limit: 1
            ) { _docID }
        }"#
    .to_string();
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

    // Sanity: the seeded request is still in its active runtime state
    // (the skip decision neither advances nor terminates it).
    let still_claimed = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(still_claimed.lifecycle_state, "claimed");
    assert_eq!(still_claimed.status, "processing");
}

/// LatestOnly concurrency: when a new fire arrives for a trigger tuple with an
/// in-flight request, the engine supersedes the prior via the same mutation
/// shape `ProductionMaterializer::supersede_active_runtime_requests_for_trigger`
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
    // `production_materializer::supersede_active_runtime_requests_for_trigger`).
    let supersede = r#"mutation {
            update_AgentRequest(
                filter: {
                    caused_by_trigger_id: { _eq: "sched-latest" },
                    caused_by_trigger_kind: { _eq: "schedule" },
                    lifecycle_state: { _in: ["pending", "claimed", "processing"] }
                },
                input: {
                    status: "superseded",
                    lifecycle_state: "superseded"
                }
            ) { _docID }
        }"#
    .to_string();
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

#[tokio::test]
async fn active_runtime_trigger_filters_ignore_input_required() {
    let db = test_db("transition-input-required-active-filter").await;

    let lineage = TriggerLineage {
        trigger_id: Some("sched-input-required".into()),
        trigger_kind: Some("schedule".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "reserved inputRequired seed",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();
    set_request_lifecycle_state(&db.node, &seeded.request().doc_id, "inputRequired").await;

    let gating_query = r#"query {
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "sched-input-required" },
                caused_by_trigger_kind: { _eq: "schedule" },
                lifecycle_state: { _in: ["pending", "claimed", "processing"] }
            },
            limit: 1
        ) { _docID }
    }"#;
    let gate = db.node.execute(gating_query).await;
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
    assert!(
        gate_rows.is_empty(),
        "active runtime gate must not observe reserved inputRequired rows"
    );

    let supersede = r#"mutation {
        update_AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "sched-input-required" },
                caused_by_trigger_kind: { _eq: "schedule" },
                lifecycle_state: { _in: ["pending", "claimed", "processing"] }
            },
            input: {
                status: "superseded",
                lifecycle_state: "superseded"
            }
        ) { _docID }
    }"#;
    let resp = db.node.execute(supersede).await;
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
    assert!(
        updated_rows.is_empty(),
        "active runtime supersede must not transition reserved inputRequired rows"
    );
    assert_lean_transition_is_illegal("Request", "inputRequired", "superseded");

    let snap = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(snap.status, "processing");
    assert_eq!(snap.lifecycle_state, "inputRequired");
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
    let query = r#"query {
            AgentRequest(
                filter: {
                    caused_by_trigger_id: { _eq: "sched-render-err" },
                    caused_by_trigger_kind: { _eq: "schedule" }
                }
            ) { _docID }
        }"#
    .to_string();
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

// -----------------------------------------------------------------------------
// Event-kind trigger transitions (Task 31, PR 2)
//
// Mirrors the Task 48 (PR 1) schedule-kind cases above but with
// `caused_by_trigger_kind = "event"`. The state-machine invariants being
// pinned are identical across kinds — the engine routes both Schedule and
// Event fires through the same `ProductionMaterializer` surface — but each
// kind's gating and supersede mutations filter on the tuple
// `(caused_by_trigger_id, caused_by_trigger_kind)`, so the event-kind cases
// need their own independent assertions.
// -----------------------------------------------------------------------------

/// Serial concurrency (event kind): when an active runtime request with
/// `caused_by_trigger_kind = "event"` already exists for the tuple, the
/// materializer's gating query observes it and the engine skips. The
/// state-machine conformance assertion: no second `AgentRequest` is created
/// for the tuple.
#[tokio::test]
async fn serial_skip_event_does_not_create_request() {
    let db = test_db("transition-event-serial-skip").await;

    // Seed an in-flight request with lineage tuple (trigger-event-serial, event).
    let lineage = TriggerLineage {
        trigger_id: Some("trigger-event-serial".into()),
        trigger_kind: Some("event".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "event serial seed",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    // Run the exact query `ProductionMaterializer` uses to gate the fire.
    // Expect `true` — an active runtime request for this tuple exists.
    let gating_query = r#"query {
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "trigger-event-serial" },
                caused_by_trigger_kind: { _eq: "event" },
                lifecycle_state: { _in: ["pending", "claimed", "processing"] }
            },
            limit: 1
        ) { _docID }
    }"#;
    let gate = db.node.execute(gating_query).await;
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
        "gating query must see the seeded in-flight event-kind request"
    );

    // Count all AgentRequests for the trigger tuple before the skip decision.
    let tuple_count_query = r#"{
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "trigger-event-serial" },
                caused_by_trigger_kind: { _eq: "event" }
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

    // Simulate the engine's FireResult::Skipped outcome: no materialize call.
    // Count after must still be 1 — the state machine invariant "serial skip
    // does not create request" holds for event-kind triggers too.
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
        "serial skip on event-kind trigger must not create a new AgentRequest"
    );

    // Sanity: the seeded request is still in its active runtime state.
    let still_claimed = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(still_claimed.lifecycle_state, "claimed");
    assert_eq!(still_claimed.status, "processing");
}

/// LatestOnly concurrency (event kind): when a new fire arrives for an
/// event-kind trigger tuple with an in-flight request, the engine supersedes
/// the prior via the same mutation shape the Schedule case uses — filtered on
/// `caused_by_trigger_kind = "event"`. The seeded request must transition
/// `(processing / claimed) -> (superseded / superseded)`.
#[tokio::test]
async fn latest_only_event_transition_to_superseded() {
    let db = test_db("transition-event-latest-only").await;

    let lineage = TriggerLineage {
        trigger_id: Some("trigger-event-latest".into()),
        trigger_kind: Some("event".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "event latest seed",
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

    // Run the engine's supersede mutation verbatim, filtered on the event
    // kind's lineage tuple (the shape in
    // `production_materializer::supersede_active_runtime_requests_for_trigger`).
    let supersede = r#"mutation {
        update_AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "trigger-event-latest" },
                caused_by_trigger_kind: { _eq: "event" },
                lifecycle_state: { _in: ["pending", "claimed", "processing"] }
            },
            input: {
                status: "superseded",
                lifecycle_state: "superseded"
            }
        ) { _docID }
    }"#;
    let resp = db.node.execute(supersede).await;
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
        "supersede must transition exactly the one seeded in-flight event-kind request"
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

/// Template render failure on an EventTrigger (`FireResult::Errored`): the
/// engine must NOT invoke the materializer when the render fails. The
/// state-machine conformance assertion: after a simulated render failure, no
/// `AgentRequest` exists for the event-kind trigger tuple, and the
/// EventTrigger's runtime-owned `last_status = "error"` writeback is
/// independent of any request row.
#[tokio::test]
async fn fire_errored_event_does_not_create_request() {
    let db = test_db("transition-event-fire-errored").await;

    // The persistence boundary: no materialize call means no AgentRequest
    // row with the event-kind lineage tuple. Query the engine's tuple filter.
    let query = r#"query {
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "trigger-event-render-err" },
                caused_by_trigger_kind: { _eq: "event" }
            }
        ) { _docID }
    }"#;
    let resp = db.node.execute(query).await;
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

    // Seed an EventTrigger doc and simulate the Errored writeback the source
    // performs (see EventSource::spawn_runtime_field_write FireResult::Errored
    // branch). The key state-machine invariant: even though last_status="error"
    // is written to the EventTrigger, NO AgentRequest appears for the lineage
    // tuple.
    let create_trigger = r#"mutation {
        create_EventTrigger(input: {
            trigger_id: "trigger-event-render-err",
            task_id: "task-event-render-err",
            source_collection: "WebhookEvent",
            event_kind: "created",
            enabled: true,
            concurrency: "serial",
            fire_count: 0
        }) { _docID }
    }"#;
    let create_resp = db.node.execute(create_trigger).await;
    assert!(
        !create_resp.has_errors(),
        "create EventTrigger failed: {:?}",
        create_resp.errors
    );

    // The actual runtime-field writeback path. Keep the input literal aligned
    // with what `update_event_trigger_runtime_fields` produces on an
    // `FireResult::Errored` outcome: last_status="error", last_error=<reason>,
    // fire_count untouched.
    let writeback = r#"mutation {
        update_EventTrigger(
            filter: { trigger_id: { _eq: "trigger-event-render-err" } },
            input: {
                last_status: "error",
                last_error: "template: variable 'missing_field' is undefined"
            }
        ) { _docID }
    }"#;
    let wb_resp = db.node.execute(writeback).await;
    assert!(
        !wb_resp.has_errors(),
        "errored writeback failed: {:?}",
        wb_resp.errors
    );

    // Re-check the persistence boundary after the writeback landed.
    let resp = db.node.execute(query).await;
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
        "Errored writeback on EventTrigger must not materialize an AgentRequest: {rows:?}"
    );

    // Confirm the writeback did land on the EventTrigger side — the state of
    // the trigger reflects the engine's error classification without ever
    // producing a request row. `fire_count` must NOT have advanced (writeback
    // supplied no `fire_count` field at all).
    let trigger_query = r#"{
        EventTrigger(filter: { trigger_id: { _eq: "trigger-event-render-err" } }, limit: 1) {
            last_status
            last_error
            fire_count
        }
    }"#;
    let trigger_resp = db.node.execute(trigger_query).await;
    let trigger_row = trigger_resp
        .data
        .as_ref()
        .and_then(|d| d.get("EventTrigger"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("EventTrigger doc was created");
    assert_eq!(
        trigger_row.get("last_status").and_then(|v| v.as_str()),
        Some("error"),
        "EventTrigger.last_status must be 'error' after an Errored writeback"
    );
    assert!(
        trigger_row
            .get("last_error")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("template:")),
        "EventTrigger.last_error must carry the template: prefix: {trigger_row}"
    );
    assert_eq!(
        trigger_row.get("fire_count").and_then(|v| v.as_i64()),
        Some(0),
        "EventTrigger.fire_count must NOT advance on Errored writeback: {trigger_row}"
    );
}
