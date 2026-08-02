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

/// Production writers for edges no single `RequestContext.Action` takes, but
/// that registered recovery sweeps perform on persisted rows (Lean:
/// `requestRecoverySweepReachable`, cited as
/// `boundary.request.recovery-sweep-reachable`).
///
/// These were previously published as `illegal`, which made the emitted
/// contract assert that Rust has no writer for edges the product performs.
fn rust_request_recovery_sweep_writer(from: &str, to: &str) -> Option<&'static str> {
    match (from, to) {
        ("claimed", "completed") => Some("RequestLifecycle::complete"),
        ("claimed", "dead") | ("processing", "dead") => {
            Some("ToolCallLifecycle::reconcile_subagent_liveness")
        }
        _ => None,
    }
}

fn rust_request_transition_classification(from: &str, to: &str) -> &'static str {
    if rust_request_transition_action(from, to).is_some() {
        "legal"
    } else if from == "inputRequired" || to == "inputRequired" {
        "productUnreachable"
    } else if rust_request_recovery_sweep_writer(from, to).is_some() {
        "recoveryReachable"
    } else {
        "illegal"
    }
}

/// Drive the real production writer for a recovery-reachable edge and assert it
/// persists the modelled post-state.
///
/// Only `claimed -> completed` is driven here: it is reachable through the
/// ordinary `RequestLifecycle` surface. The two `-> dead` edges run inside
/// `reconcile_subagent_liveness`, which needs a running-bridge plus expired-child
/// fixture; driving them is tracked in #994.
async fn drive_generated_request_recovery_reachable_case(case: &LeanLifecycleTransitionCase) {
    if !(case.from == "claimed" && case.to == "completed") {
        return;
    }

    let db = test_db("generated-request-recovery-reachable").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;
    let mut lifecycle = request_lifecycle_for_case(
        &db,
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at.clone(),
    );

    // Claim, then complete WITHOUT begin_execution: this is terminal repair
    // finishing a claimed request whose response already landed, and it
    // exercises `complete()`'s persisted from-set including `claimed`.
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.complete().await.unwrap();

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap.lifecycle_state, case.to,
        "recovery-reachable Request transition {} expected {} -> {} via {:?}, got persisted lifecycle_state={}",
        case.name,
        case.from,
        case.to,
        rust_request_recovery_sweep_writer(&case.from, &case.to),
        snap.lifecycle_state
    );
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
    let mut recovery_reachable_count = 0;

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
                // NOTE: this consults the writer inventory above, not production
                // behaviour — an unlisted writer is invisible here. Inverting the
                // fence (drive every production writer from every state and assert
                // the observed edge set is a subset of legal + recoveryReachable)
                // is tracked in #994.
                assert!(
                    rust_request_transition_action(&case.from, &case.to).is_none()
                        && rust_request_recovery_sweep_writer(&case.from, &case.to).is_none(),
                    "Request transition {} is ordinary illegal but Rust has a writer path for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
            }
            "recoveryReachable" => {
                recovery_reachable_count += 1;
                assert!(
                    case.action.is_none(),
                    "Request transition {} is recovery-reachable and must be taken by no single action, got {:?}",
                    case.name,
                    case.action
                );
                assert!(
                    rust_request_recovery_sweep_writer(&case.from, &case.to).is_some(),
                    "Request transition {} is recovery-reachable but no Rust sweep writer is registered for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                assert_eq!(
                    case.boundary.as_deref(),
                    Some("boundary.request.recovery-sweep-reachable"),
                    "Request transition {} must cite the recovery-sweep boundary",
                    case.name
                );
                drive_generated_request_recovery_reachable_case(case).await;
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
    assert_eq!(illegal_count, 50);
    assert_eq!(product_unreachable_count, 17);
    assert_eq!(recovery_reachable_count, 3);
}

/// Persisted `(status, lifecycle_state)` pair for a terminal request, mirroring
/// the bridge documented in `proofs/README.md`: terminal work carries a terminal
/// `lifecycle_state`, and `failed` is persisted with `status="error"`.
fn terminal_persisted_pair(lifecycle_state: &str) -> (&'static str, &'static str) {
    match lifecycle_state {
        "completed" => ("completed", "completed"),
        "failed" => ("error", "failed"),
        "superseded" => ("superseded", "superseded"),
        "dead" => ("dead", "dead"),
        "interrupted" => ("interrupted", "interrupted"),
        other => panic!("not a terminal lifecycle_state: {other}"),
    }
}

async fn force_terminal_persisted_state(
    node: &EmbeddedNode,
    doc_id: &str,
    lifecycle_state: &str,
) {
    let (status, lifecycle_state) = terminal_persisted_pair(lifecycle_state);
    let escaped_doc_id = escape_graphql_string(doc_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                input: {{ status: "{status}", lifecycle_state: "{lifecycle_state}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "forcing terminal persisted state failed: {:?}",
        resp.errors
    );
}

/// S1 (`terminal_irreversibility`) asserted against PRODUCTION writers rather
/// than against the writer inventory at the top of this file.
///
/// This models the race the persisted CAS filters exist for: this runtime still
/// holds a live `RequestLifecycle` that believes it owns the request, while
/// another actor — a recovery sweep, a replicated peer, an operator interrupt —
/// has already terminalized the row. Every terminal writer must leave the
/// persisted document untouched, so no `terminal -> *` edge is reachable.
///
/// Unlike the `illegal` branch of the generated-case test (which consults the
/// hand-written inventory and so cannot see an unlisted writer), this drives the
/// real writers and asserts on persisted state. Extending the same treatment to
/// the whole illegal partition is #994.
#[tokio::test]
async fn terminal_persisted_requests_reject_every_live_lifecycle_writer() {
    const TERMINAL_STATES: [&str; 5] = [
        "completed",
        "failed",
        "superseded",
        "dead",
        "interrupted",
    ];
    const WRITERS: [&str; 4] = ["complete", "fail", "interrupt", "advance"];

    for terminal in TERMINAL_STATES {
        let db = test_db(&format!("terminal-irreversibility-{terminal}")).await;

        for writer in WRITERS {
            let request_id = uuid::Uuid::new_v4().to_string();
            let session_id = uuid::Uuid::new_v4().to_string();
            let created_at = chrono::Utc::now().to_rfc3339();
            let doc_id =
                create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;
            let mut lifecycle = request_lifecycle_for_case(
                &db,
                doc_id.clone(),
                request_id.clone(),
                session_id.clone(),
                created_at.clone(),
            );

            // Take real ownership first, so the local state machine permits the
            // call and the persisted CAS filter is what actually decides.
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            if writer == "advance" {
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
            }

            // Another actor terminalizes the row underneath the live lifecycle.
            force_terminal_persisted_state(&db.node, &doc_id, terminal).await;

            // The writer may return Ok (no rows matched) or Err; the contract is
            // about the persisted document, not the return value.
            let _ = match writer {
                "complete" => lifecycle.complete().await,
                "fail" => lifecycle.fail().await,
                "interrupt" => lifecycle.transition_to_interrupted().await,
                "advance" => lifecycle.advance().await,
                other => panic!("unhandled writer {other}"),
            };

            let snap = fetch_request_snapshot(&db.node, &doc_id).await;
            let (expected_status, expected_lifecycle_state) = terminal_persisted_pair(terminal);
            assert_eq!(
                snap.lifecycle_state, expected_lifecycle_state,
                "writer {writer} moved a persisted {terminal} request to {} — terminal states must be irreversible (S1)",
                snap.lifecycle_state
            );
            assert_eq!(
                snap.status, expected_status,
                "writer {writer} changed the persisted status of a {terminal} request"
            );
        }
    }
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
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
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
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
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
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
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
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
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

#[tokio::test]
async fn serial_skip_does_not_create_request() {
    let db = test_db("transition-serial-skip").await;

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

    let still_claimed = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(still_claimed.lifecycle_state, "claimed");
    assert_eq!(still_claimed.status, "processing");
}

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

    let before = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(before.lifecycle_state, "claimed");
    assert_eq!(before.status, "processing");

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
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
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

#[tokio::test]
async fn fire_errored_does_not_create_request() {
    let db = test_db("transition-fire-errored").await;

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
async fn serial_skip_event_does_not_create_request() {
    let db = test_db("transition-event-serial-skip").await;

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

    let still_claimed = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(still_claimed.lifecycle_state, "claimed");
    assert_eq!(still_claimed.status, "processing");
}

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

    let before = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(before.lifecycle_state, "claimed");
    assert_eq!(before.status, "processing");

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
            max_retries: gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
}

#[tokio::test]
async fn fire_errored_event_does_not_create_request() {
    let db = test_db("transition-event-fire-errored").await;

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

use gents::background_completion::{
    project_background_subagent_completion, BackgroundCompletionOutcome,
};
use gents::tool_call_lifecycle::{
    create_subagent_request_with_request_id, AwaitMode, CancelPolicy, ToolCallLifecycle,
};
use gents::{AgentBehaviorDocument, ToolSelectionDocument};

pub(super) async fn generated_queue_deadline_cases_pin_r4a_contract_rows() {
    let cases = lean_queue_deadline_cases();
    assert_eq!(cases.len(), 5);

    let emitted_names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        emitted_names,
        [
            "active_request_blocks_later_same_session_claim",
            "terminal_active_allows_next_pending_same_session_claim",
            "background_completion_notification_creates_no_agent_request",
            "cancel_drains_automated_wakeups_preserves_user_pending",
            "claim_preserves_explicit_deadline",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    for case in cases {
        assert_eq!(case.session_id, 900, "{}", case.name);
        assert!(
            case.superseded_request_ids.is_empty(),
            "{} must be a queue/deadline contract, not a supersession contract",
            case.name
        );
    }

    let blocked = lean_queue_deadline_case("active_request_blocks_later_same_session_claim");
    assert_eq!(blocked.group, "queue_admission");
    assert_eq!(blocked.action, "claimNext");
    assert!(!blocked.legal);
    assert!(blocked.blocked_by_active);
    assert_eq!(blocked.pre_active_request_id, Some(100));
    assert_eq!(blocked.post_active_request_id, Some(100));
    assert_eq!(blocked.pre_pending_request_ids, vec![101]);
    assert_eq!(blocked.post_pending_request_ids, vec![101]);
    assert_eq!(blocked.claimed_request_id, None);
    assert!(blocked.post_terminal_request_ids.is_empty());

    let terminal =
        lean_queue_deadline_case("terminal_active_allows_next_pending_same_session_claim");
    assert_eq!(terminal.group, "queue_admission");
    assert_eq!(terminal.action, "finishActive_then_claimNext");
    assert!(terminal.legal);
    assert!(!terminal.blocked_by_active);
    assert_eq!(terminal.pre_active_request_id, Some(100));
    assert_eq!(terminal.pre_pending_request_ids, vec![101]);
    assert_eq!(terminal.post_active_request_id, Some(101));
    assert_eq!(terminal.claimed_request_id, Some(101));
    assert!(terminal.post_pending_request_ids.is_empty());
    assert_eq!(terminal.post_terminal_request_ids, vec![100]);

    let completion =
        lean_queue_deadline_case("background_completion_notification_creates_no_agent_request");
    assert_eq!(completion.group, "completion_delivery");
    assert_eq!(completion.action, "appendNotification");
    assert!(completion.legal);
    assert_eq!(completion.queue_key, None);
    assert!(completion.pre_pending_request_ids.is_empty());
    assert!(completion.post_pending_request_ids.is_empty());
    assert_eq!(completion.post_coalesced_pending_count, 0);
    assert!(completion.post_terminal_request_ids.is_empty());

    let cancel = lean_queue_deadline_case("cancel_drains_automated_wakeups_preserves_user_pending");
    assert_eq!(cancel.group, "queue_cancel");
    assert_eq!(cancel.action, "drainAutomated");
    assert!(cancel.legal);
    assert_eq!(
        cancel.queue_key.as_deref(),
        Some("background_completion:900")
    );
    assert_eq!(cancel.pre_pending_request_ids, vec![301, 302]);
    assert_eq!(cancel.post_pending_request_ids, vec![302]);
    assert_eq!(cancel.automated_drained_request_ids, vec![301]);
    assert_eq!(cancel.preserved_user_pending_request_ids, vec![302]);
    assert_eq!(cancel.post_terminal_request_ids, vec![301]);
    assert_eq!(cancel.post_coalesced_pending_count, 0);

    let deadline = lean_queue_deadline_case("claim_preserves_explicit_deadline");
    assert_eq!(deadline.group, "claim_deadline");
    assert_eq!(deadline.action, "claim");
    assert!(deadline.legal);
    assert_eq!(deadline.claimed_request_id, Some(401));
    assert_eq!(deadline.pre_request_deadline, Some(50));
    assert_eq!(deadline.synthesized_claim_deadline, Some(51));
    assert_eq!(deadline.post_deadline, Some(50));
    assert!(
        deadline.post_deadline < deadline.synthesized_claim_deadline,
        "explicit request deadline should remain tighter than the synthesized claim deadline"
    );
    assert!(deadline.explicit_deadline_preserved);

    for case in cases {
        drive_queue_deadline_case(case).await;
    }
}

async fn drive_queue_deadline_case(case: &lean_vocab_test::LeanQueueDeadlineConformanceCase) {
    match case.name.as_str() {
        "active_request_blocks_later_same_session_claim" => {
            drive_active_request_blocks_later_same_session_claim(case).await;
        }
        "terminal_active_allows_next_pending_same_session_claim" => {
            drive_terminal_active_allows_next_pending_same_session_claim(case).await;
        }
        "background_completion_notification_creates_no_agent_request" => {
            drive_background_completion_notification_creates_no_agent_request(case).await;
        }
        "cancel_drains_automated_wakeups_preserves_user_pending" => {
            drive_cancel_drains_automated_wakeups_preserves_user_pending(case).await;
        }
        "claim_preserves_explicit_deadline" => {
            drive_claim_preserves_explicit_deadline(case).await;
        }
        other => panic!("unhandled queue/deadline conformance case {other}"),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct QueueRuntimeRow {
    request_id: String,
    status: String,
    lifecycle_state: Option<String>,
    metadata: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueueRuntimeSnapshot {
    active_request_id: Option<usize>,
    pending_request_ids: Vec<usize>,
    terminal_request_ids: Vec<usize>,
    coalesced_pending_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct DeadlineRuntimeRow {
    status: String,
    lifecycle_state: Option<String>,
    deadline: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionIdRow {
    session_id: String,
}

fn symbolic_request_id(
    runtime_request_id: &str,
    generated_ids: &std::collections::BTreeMap<String, usize>,
) -> Option<usize> {
    runtime_request_id
        .parse::<usize>()
        .ok()
        .or_else(|| generated_ids.get(runtime_request_id).copied())
}

fn row_is_pending(row: &QueueRuntimeRow) -> bool {
    row.status == "pending" && row.lifecycle_state.as_deref() == Some("pending")
}

fn row_is_active(row: &QueueRuntimeRow) -> bool {
    row.status == "processing"
        && matches!(
            row.lifecycle_state.as_deref(),
            Some("claimed" | "processing")
        )
}

fn row_is_terminal(row: &QueueRuntimeRow) -> bool {
    matches!(
        row.lifecycle_state.as_deref(),
        Some("completed" | "failed" | "superseded" | "dead" | "interrupted")
    ) || matches!(
        row.status.as_str(),
        "completed" | "error" | "superseded" | "dead" | "interrupted"
    )
}

fn row_matches_coalesced_key(row: &QueueRuntimeRow, queue_key: Option<&str>) -> bool {
    let Some(queue_key) = queue_key else {
        return false;
    };
    if !row_is_pending(row) {
        return false;
    }
    let Some(metadata) = row.metadata.as_deref() else {
        return false;
    };
    let Ok(metadata) = serde_json::from_str::<serde_json::Value>(metadata) else {
        return false;
    };
    let Some(queue) = metadata.get("queue") else {
        return false;
    };
    queue.get("source").and_then(serde_json::Value::as_str) == Some("background_completion")
        && queue.get("policy").and_then(serde_json::Value::as_str) == Some("coalesce")
        && queue.get("key").and_then(serde_json::Value::as_str) == Some(queue_key)
}

async fn fetch_queue_runtime_snapshot(
    node: &EmbeddedNode,
    session_id: &str,
    queue_key: Option<&str>,
    generated_ids: &std::collections::BTreeMap<String, usize>,
) -> QueueRuntimeSnapshot {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
            ) {{
                request_id
                status
                lifecycle_state
                metadata
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "queue snapshot query failed: {:?}",
        response.errors
    );
    let rows: Vec<QueueRuntimeRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();

    let active_request_ids = rows
        .iter()
        .filter(|row| row_is_active(row))
        .filter_map(|row| symbolic_request_id(&row.request_id, generated_ids))
        .collect::<Vec<_>>();
    assert!(
        active_request_ids.len() <= 1,
        "queue snapshot should expose at most one active request: {active_request_ids:?}"
    );

    let pending_request_ids = rows
        .iter()
        .filter(|row| row_is_pending(row))
        .filter_map(|row| symbolic_request_id(&row.request_id, generated_ids))
        .collect::<Vec<_>>();
    let terminal_request_ids = rows
        .iter()
        .filter(|row| row_is_terminal(row))
        .filter_map(|row| symbolic_request_id(&row.request_id, generated_ids))
        .collect::<Vec<_>>();
    let coalesced_pending_count = rows
        .iter()
        .filter(|row| row_matches_coalesced_key(row, queue_key))
        .count();

    QueueRuntimeSnapshot {
        active_request_id: active_request_ids.first().copied(),
        pending_request_ids,
        terminal_request_ids,
        coalesced_pending_count,
    }
}

fn assert_pre_queue_snapshot(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
    snapshot: &QueueRuntimeSnapshot,
) {
    assert_eq!(
        snapshot.active_request_id, case.pre_active_request_id,
        "{} pre active request drifted",
        case.name
    );
    assert_eq!(
        snapshot.pending_request_ids, case.pre_pending_request_ids,
        "{} pre pending queue drifted",
        case.name
    );
}

fn assert_post_queue_snapshot(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
    snapshot: &QueueRuntimeSnapshot,
) {
    assert_eq!(
        snapshot.active_request_id, case.post_active_request_id,
        "{} post active request drifted",
        case.name
    );
    assert_eq!(
        snapshot.pending_request_ids, case.post_pending_request_ids,
        "{} post pending queue drifted",
        case.name
    );
    assert_eq!(
        snapshot.terminal_request_ids, case.post_terminal_request_ids,
        "{} terminalized requests drifted",
        case.name
    );
    assert_eq!(
        snapshot.coalesced_pending_count, case.post_coalesced_pending_count,
        "{} coalesced pending count drifted",
        case.name
    );
}

fn request_from_parts(
    doc_id: String,
    request_id: usize,
    session_id: &str,
    created_at: &str,
    deadline: Option<String>,
) -> gents::AgentRequest {
    let mut request = build_request(
        doc_id,
        request_id.to_string(),
        session_id.to_string(),
        created_at.to_string(),
    );
    request.deadline = deadline;
    request
}

fn lifecycle_for(
    node: &std::sync::Arc<EmbeddedNode>,
    request: gents::AgentRequest,
    deadline_duration_secs: u64,
) -> RequestLifecycle {
    RequestLifecycle::new_with_agent_did(
        node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        deadline_duration_secs,
    )
}

async fn drive_active_request_blocks_later_same_session_claim(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-active-blocks").await;
    let session_id = case.session_id.to_string();
    let active_id = case.pre_active_request_id.expect("active request id");
    let pending_id = case
        .pre_pending_request_ids
        .first()
        .copied()
        .expect("pending request id");
    let active_created_at = "2026-03-23T00:00:10Z";
    let pending_created_at = "2026-03-23T00:00:20Z";

    let active_doc_id = create_request(
        &db.node,
        &active_id.to_string(),
        &session_id,
        "pending",
        active_created_at,
    )
    .await;
    let pending_doc_id = create_request(
        &db.node,
        &pending_id.to_string(),
        &session_id,
        "pending",
        pending_created_at,
    )
    .await;

    let active_request = request_from_parts(
        active_doc_id,
        active_id,
        &session_id,
        active_created_at,
        None,
    );
    let mut active_lifecycle = lifecycle_for(&db.node, active_request, DEADLINE_SECS);
    assert_eq!(
        active_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_pre_queue_snapshot(case, &pre);

    let pending_request = request_from_parts(
        pending_doc_id,
        pending_id,
        &session_id,
        pending_created_at,
        None,
    );
    let mut pending_lifecycle = lifecycle_for(&db.node, pending_request, DEADLINE_SECS);
    assert_eq!(
        pending_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Queued
    );
    assert_eq!(case.claimed_request_id, None);

    let post = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_terminal_active_allows_next_pending_same_session_claim(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-terminal-allows").await;
    let session_id = case.session_id.to_string();
    let active_id = case.pre_active_request_id.expect("active request id");
    let pending_id = case.claimed_request_id.expect("claimed request id");
    let active_created_at = "2026-03-23T00:00:10Z";
    let pending_created_at = "2026-03-23T00:00:20Z";

    let active_doc_id = create_request(
        &db.node,
        &active_id.to_string(),
        &session_id,
        "pending",
        active_created_at,
    )
    .await;
    let pending_doc_id = create_request(
        &db.node,
        &pending_id.to_string(),
        &session_id,
        "pending",
        pending_created_at,
    )
    .await;

    let active_request = request_from_parts(
        active_doc_id,
        active_id,
        &session_id,
        active_created_at,
        None,
    );
    let mut active_lifecycle = lifecycle_for(&db.node, active_request, DEADLINE_SECS);
    assert_eq!(
        active_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_pre_queue_snapshot(case, &pre);

    active_lifecycle.complete().await.unwrap();

    let pending_request = request_from_parts(
        pending_doc_id,
        pending_id,
        &session_id,
        pending_created_at,
        None,
    );
    let mut pending_lifecycle = lifecycle_for(&db.node, pending_request, DEADLINE_SECS);
    assert_eq!(
        pending_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let post = fetch_queue_runtime_snapshot(&db.node, &session_id, None, &generated_ids).await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_background_completion_notification_creates_no_agent_request(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-coalesce").await;
    let session_id = case.session_id.to_string();
    let parent_request_id = "queue-deadline-coalesce-parent";
    install_background_completion_fixture(db.node.as_ref()).await;
    create_queue_request(
        db.node.as_ref(),
        parent_request_id,
        &session_id,
        "completed",
        "2026-03-23T00:00:00Z",
        "interactive",
        None,
        Some(&(chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()),
    )
    .await;

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_pre_queue_snapshot(case, &pre);

    let (child_a, child_session_a) = create_background_child_bridge(
        &db.node,
        parent_request_id,
        &session_id,
        "queue-deadline-coalesce-a",
        1,
    )
    .await;
    let (child_b, child_session_b) = create_background_child_bridge(
        &db.node,
        parent_request_id,
        &session_id,
        "queue-deadline-coalesce-b",
        2,
    )
    .await;
    persist_child_completion(
        db.node.as_ref(),
        &child_a,
        &child_session_a,
        "child A complete",
    )
    .await;
    persist_child_completion(
        db.node.as_ref(),
        &child_b,
        &child_session_b,
        "child B complete",
    )
    .await;

    let first = project_background_subagent_completion(db.node.clone(), &child_a, AGENT_DID)
        .await
        .unwrap();
    let second = project_background_subagent_completion(db.node.clone(), &child_b, AGENT_DID)
        .await
        .unwrap();
    assert!(matches!(
        first,
        BackgroundCompletionOutcome::Projected { .. }
    ));
    assert!(matches!(
        second,
        BackgroundCompletionOutcome::Projected { .. }
    ));

    let generated_ids = std::collections::BTreeMap::new();
    let post = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_cancel_drains_automated_wakeups_preserves_user_pending(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-cancel-drain").await;
    let session_id = case.session_id.to_string();
    let parent_request_id = "queue-deadline-cancel-parent";
    create_queue_request(
        db.node.as_ref(),
        parent_request_id,
        &session_id,
        "completed",
        "2026-03-23T00:00:00Z",
        "interactive",
        None,
        None,
    )
    .await;

    let automated_id = case.automated_drained_request_ids[0];
    let user_id = case.preserved_user_pending_request_ids[0];
    create_queue_request(
        db.node.as_ref(),
        &automated_id.to_string(),
        &session_id,
        "pending",
        "2026-03-23T00:00:10Z",
        "scheduled",
        Some(&automated_queue_metadata(
            case.queue_key.as_deref().expect("queue key"),
            parent_request_id,
        )),
        None,
    )
    .await;
    create_queue_request(
        db.node.as_ref(),
        &user_id.to_string(),
        &session_id,
        "pending",
        "2026-03-23T00:00:20Z",
        "scheduled",
        Some(&user_queue_metadata()),
        None,
    )
    .await;

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_pre_queue_snapshot(case, &pre);

    gents::interrupt_request(db.node.as_ref(), parent_request_id)
        .await
        .unwrap();

    let post = fetch_queue_runtime_snapshot(
        &db.node,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_post_queue_snapshot(case, &post);
    assert_eq!(
        post.pending_request_ids, case.preserved_user_pending_request_ids,
        "{} should preserve only the user pending row",
        case.name
    );
}

async fn drive_claim_preserves_explicit_deadline(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-explicit-deadline").await;
    let session_id = case.session_id.to_string();
    let request_id = case.claimed_request_id.expect("claimed request id");
    let created_at = chrono::Utc::now().to_rfc3339();
    let explicit_deadline_at =
        chrono::Utc::now() + chrono::Duration::seconds(case.pre_request_deadline.unwrap() as i64);
    let explicit_deadline = explicit_deadline_at.to_rfc3339();
    let doc_id = create_queue_request(
        db.node.as_ref(),
        &request_id.to_string(),
        &session_id,
        "pending",
        &created_at,
        "interactive",
        None,
        Some(&explicit_deadline),
    )
    .await;
    let request = request_from_parts(
        doc_id,
        request_id,
        &session_id,
        &created_at,
        Some(explicit_deadline.clone()),
    );

    let before_claim = chrono::Utc::now();
    let mut lifecycle = lifecycle_for(
        &db.node,
        request,
        case.synthesized_claim_deadline.unwrap() as u64,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let row = fetch_deadline_runtime_row(db.node.as_ref(), request_id).await;
    assert_eq!(row.status, "processing");
    assert_eq!(row.lifecycle_state.as_deref(), Some("claimed"));

    let persisted_deadline = chrono::DateTime::parse_from_rfc3339(&row.deadline).unwrap();
    assert_eq!(
        persisted_deadline,
        chrono::DateTime::parse_from_rfc3339(&explicit_deadline).unwrap(),
        "{} should preserve the request's explicit deadline",
        case.name
    );
    assert!(
        persisted_deadline.with_timezone(&chrono::Utc)
            < before_claim
                + chrono::Duration::seconds(case.synthesized_claim_deadline.unwrap() as i64),
        "{} should keep the tighter explicit deadline instead of the synthesized claim deadline",
        case.name
    );
    assert!(case.explicit_deadline_preserved);
}

#[allow(clippy::too_many_arguments)]
async fn create_queue_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    status: &str,
    created_at: &str,
    execution_origin: &str,
    metadata: Option<&str>,
    deadline: Option<&str>,
) -> String {
    let lifecycle_state = match status {
        "pending" => "pending",
        "processing" => "processing",
        "completed" => "completed",
        "interrupted" => "interrupted",
        "superseded" => "superseded",
        "dead" => "dead",
        "error" => "failed",
        other => panic!("unsupported queue request status {other}"),
    };
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_created_at = escape_graphql_string(created_at);
    let escaped_execution_origin = escape_graphql_string(execution_origin);
    let metadata_field = metadata
        .map(|metadata| format!(r#", metadata: "{}""#, escape_graphql_string(metadata)))
        .unwrap_or_default();
    let deadline_field = deadline
        .map(|deadline| format!(r#", deadline: "{}""#, escape_graphql_string(deadline)))
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "queue deadline conformance",
                status: "{status}",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "",
                execution_origin: "{escaped_execution_origin}",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: 0{metadata_field}{deadline_field}
            }}) {{ _docID }}
        }}"#,
        max_retries = gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create queue request failed: {:?}",
        response.errors
    );

    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    support::first_row::<support::DocIdRow>(&node.execute(&query).await, "AgentRequest").doc_id
}

fn automated_queue_metadata(queue_key: &str, queued_after_request_id: &str) -> String {
    json!({
        "queue": {
            "source": "background_completion",
            "policy": "coalesce",
            "key": queue_key,
            "queued_after_request_id": queued_after_request_id,
        }
    })
    .to_string()
}

fn user_queue_metadata() -> String {
    json!({
        "queue": {
            "source": "user",
            "policy": "append",
            "key": null,
            "queued_after_request_id": null,
        }
    })
    .to_string()
}

async fn install_background_completion_fixture(node: &EmbeddedNode) {
    const TOOL_SELECTION_ID: &str = "queue-deadline-tools";
    const CHILD_BEHAVIOR_ID: &str = "queue-deadline-child";

    gents::upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: TOOL_SELECTION_ID.to_string(),
            agent_did: AGENT_DID.to_string(),
            subagent_targets: Some(vec![gents::subagent_target_entry(
                CHILD_BEHAVIOR_ID,
                AGENT_DID,
                CHILD_BEHAVIOR_ID,
                None,
            )]),
            subagent_spawn_enabled: Some(true),
            subagent_background_enabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    gents::upsert_agent_behavior(
        node,
        &AgentBehaviorDocument {
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            behavior_id: AGENT_NAME.to_string(),
            agent_did: AGENT_DID.to_string(),
            display_name: Some("Queue deadline parent".to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            request_context_template: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: Some(TOOL_SELECTION_ID.to_string()),
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            created_at: Some("2026-03-23T00:00:00Z".to_string()),
        },
    )
    .await
    .unwrap();
    gents::upsert_agent_behavior(
        node,
        &AgentBehaviorDocument {
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            behavior_id: CHILD_BEHAVIOR_ID.to_string(),
            agent_did: AGENT_DID.to_string(),
            display_name: Some("Queue deadline child".to_string()),
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
            enabled: true,
            created_at: Some("2026-03-23T00:00:01Z".to_string()),
        },
    )
    .await
    .unwrap();
}

async fn create_background_child_bridge(
    node: &std::sync::Arc<EmbeddedNode>,
    parent_request_id: &str,
    parent_session_id: &str,
    tool_call_id: &str,
    message_sequence: u32,
) -> (String, String) {
    const CHILD_BEHAVIOR_ID: &str = "queue-deadline-child";

    let child_request_id = format!("{parent_request_id}-{tool_call_id}-child");
    create_subagent_request_with_request_id(
        node.as_ref(),
        child_request_id.clone(),
        parent_request_id.to_string(),
        tool_call_id.to_string(),
        0,
        AGENT_DID.to_string(),
        CHILD_BEHAVIOR_ID.to_string(),
        format!("prompt for {tool_call_id}"),
        Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
    )
    .await
    .unwrap();
    let child_session_id = child_session_id(node.as_ref(), &child_request_id).await;

    let mut lifecycle = ToolCallLifecycle::new_subagent(
        node.clone(),
        parent_request_id.to_string(),
        parent_session_id.to_string(),
        "did:test:test".to_string(),
        tool_call_id.to_string(),
        message_sequence,
        "spawn_subagent".to_string(),
        json!({
            "behavior_id": CHILD_BEHAVIOR_ID,
            "prompt": format!("prompt for {tool_call_id}"),
            "await_mode": AwaitMode::Background.as_str(),
        })
        .to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        child_request_id.clone(),
        AGENT_DID.to_string(),
    );
    lifecycle.start_running().await.unwrap();

    (child_request_id, child_session_id)
}

async fn child_session_id(node: &EmbeddedNode, child_request_id: &str) -> String {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{ session_id }}
        }}"#
    );
    support::first_row::<SessionIdRow>(&node.execute(&query).await, "AgentRequest").session_id
}

async fn persist_child_completion(
    node: &EmbeddedNode,
    child_request_id: &str,
    child_session_id: &str,
    final_response: &str,
) {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let update_request = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                input: {{ status: "completed", lifecycle_state: "completed" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&update_request).await;
    assert!(
        !response.has_errors(),
        "update child AgentRequest completed failed: {:?}",
        response.errors
    );

    let assistant = Message::Assistant {
        id: None,
        content: vec![AssistantContent::Text(Text {
            text: final_response.to_string(),
        })],
    };
    let escaped_message = escape_graphql_string(&serde_json::to_string(&assistant).unwrap());
    let escaped_child_session_id = escape_graphql_string(child_session_id);
    let now = chrono::Utc::now().to_rfc3339();
    let create_message = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{escaped_child_session_id}:1",
                session_id: "{escaped_child_session_id}",
                sequence: 1,
                role: "assistant",
                content: "{escaped_message}",
                timestamp: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&create_message).await;
    assert!(
        !response.has_errors(),
        "create child AgentMessage failed: {:?}",
        response.errors
    );

    let create_response = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{escaped_child_request_id}",
                request_id: "{escaped_child_request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "queue-deadline-child",
                session_id: "{escaped_child_session_id}",
                content: "",
                reasoning: "",
                status: "completed",
                error_message: "",
                token_count: 0,
                progress_seq: 0,
                materialized_message_sequence: 1,
                materialized_at: "{now}",
                created_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&create_response).await;
    assert!(
        !response.has_errors(),
        "create child AgentResponse failed: {:?}",
        response.errors
    );
}

async fn fetch_deadline_runtime_row(node: &EmbeddedNode, request_id: usize) -> DeadlineRuntimeRow {
    let escaped_request_id = escape_graphql_string(&request_id.to_string());
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ status lifecycle_state deadline }}
        }}"#
    );
    support::first_row::<DeadlineRuntimeRow>(&node.execute(&query).await, "AgentRequest")
}
