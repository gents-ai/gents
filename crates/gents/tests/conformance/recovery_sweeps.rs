use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use std::collections::BTreeMap;
use std::sync::Arc;

const RECOVERY_CREATED_AT: &str = "2026-03-23T00:00:00Z";

pub(super) async fn generated_recovery_sweep_cases_drive_startup_recovery_contract() {
    let cases = lean_recovery_sweep_cases();
    assert_eq!(
        cases.len(),
        29,
        "Lean should emit one row per registered recovery predicate witness"
    );

    let expected_sweep_ids = [
        "request_lifecycle_recover_all_requests",
        "tool_call_lifecycle_recover_all_running_calls",
        "tool_call_lifecycle_reconcile_orphaned_background_tools",
        "tool_call_lifecycle_reconcile_background_completion_side_effects",
        "tool_call_lifecycle_reconcile_terminal_parent_owned_tools",
        "tool_call_lifecycle_recover_session_message_rows",
        "inference_call_recover_all_stale_calls",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let actual_sweep_ids = cases
        .iter()
        .map(|case| case.sweep_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_sweep_ids, expected_sweep_ids,
        "Lean recovery sweep registry drifted"
    );
    assert_periodic_recovery_registry_matches_lean(cases);

    for case in cases {
        assert_recovery_case_metadata(case);
        drive_recovery_sweep_case(case).await;
    }
}

fn assert_recovery_case_metadata(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let expected_cadence = if rust_periodic_recovery_sweep_ids().contains(case.sweep_id.as_str()) {
        "periodic"
    } else {
        "startup"
    };
    assert_eq!(case.cadence.as_str(), expected_cadence, "{}", case.name);
    assert_eq!(
        case.implementation_status.as_str(),
        "implemented",
        "recovery case {} must be implemented before the runtime drive can consume it",
        case.name
    );
    if case.name.ends_with("_deferred") {
        assert_eq!(case.measure_before, 0, "{}", case.name);
        assert_eq!(case.measure_after, 0, "{}", case.name);
        assert_eq!(case.terminal_state, "running", "{}", case.name);
    } else {
        assert!(
            case.measure_before > case.measure_after,
            "recovery case {} must decrease its measure",
            case.name
        );
        assert_ne!(
            case.terminal_state.as_str(),
            "running",
            "recovery case {} must not leave a stale row running",
            case.name
        );
    }
    assert_eq!(
        case.measure_after, 0,
        "recovery case {} must reach zero measure",
        case.name
    );
    assert!(
        !case.deadline_audit_ref.trim().is_empty(),
        "recovery case {} must name its audit reference",
        case.name
    );
}

fn assert_periodic_recovery_registry_matches_lean(
    cases: &[lean_vocab_test::LeanRecoverySweepCase],
) {
    let mut lean_periodic_by_id = BTreeMap::new();
    for case in cases.iter().filter(|case| case.cadence == "periodic") {
        if let Some(previous) =
            lean_periodic_by_id.insert(case.sweep_id.as_str(), case.rust_function.as_str())
        {
            assert_eq!(
                previous,
                case.rust_function.as_str(),
                "Lean emitted conflicting Rust functions for periodic recovery sweep {}",
                case.sweep_id
            );
        }
    }

    let mut rust_periodic_by_id = BTreeMap::new();
    for metadata in gents::periodic_recovery_sweep_metadata() {
        assert!(
            !metadata.sweep_ids.is_empty(),
            "periodic recovery registry entry {} must name at least one Lean sweep id",
            metadata.rust_function
        );
        for sweep_id in metadata.sweep_ids {
            assert!(
                rust_periodic_by_id
                    .insert(*sweep_id, metadata.rust_function)
                    .is_none(),
                "periodic recovery sweep id {sweep_id} registered more than once"
            );
        }
    }

    assert_eq!(
        rust_periodic_by_id.keys().copied().collect::<BTreeSet<_>>(),
        lean_periodic_by_id.keys().copied().collect::<BTreeSet<_>>(),
        "Rust periodic recovery registry drifted from Lean cadence=periodic sweeps"
    );
    for (sweep_id, rust_function) in rust_periodic_by_id {
        assert_eq!(
            Some(&rust_function),
            lean_periodic_by_id.get(sweep_id),
            "periodic recovery registry Rust function drifted for {sweep_id}"
        );
    }
}

fn rust_periodic_recovery_sweep_ids() -> BTreeSet<&'static str> {
    gents::periodic_recovery_sweep_metadata()
        .iter()
        .flat_map(|metadata| metadata.sweep_ids.iter().copied())
        .collect()
}

async fn drive_recovery_sweep_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    if case.collection == "AgentToolCall" {
        // These rows are driven by the crate-private accepted-publication
        // recovery conformance tests, including deferred missing parents.
        return;
    }
    match (case.collection.as_str(), case.sweep_id.as_str()) {
        ("AgentRequest", _) => drive_request_recovery_case(case).await,
        ("InferenceCall", _) => drive_inference_call_recovery_case(case).await,
        (other, _) => panic!("unhandled recovery collection {other} for {}", case.name),
    }
}

/// Issue #1001 defect 2: the startup inference-call sweep is parent-gated, so
/// it must run after request repair. Drives the real ordered startup sweep
/// (`gents::startup_recovery::run_startup_recovery`) over a crash shape — a
/// parent stuck `processing` with a linked `running` call and no live loop —
/// and requires the orphan to terminalize in the FIRST startup pass.
/// Lean: `Recovery.request_before_inference_converges`
/// (`Proofs/Recovery/StartupOrder.lean`).
pub(super) async fn startup_recovery_order_terminalizes_crash_orphaned_calls() {
    let db = test_db("startup-recovery-order-1001").await;
    let request_id = "startup-order-1001-request";
    let session_id = "startup-order-1001-session";
    let request_doc_id = create_request(
        &db.node,
        request_id,
        session_id,
        "pending",
        RECOVERY_CREATED_AT,
    )
    .await;
    support::create_session_document(
        &db.node,
        &gents_protocol::session::AgentSession {
            title: Some(gents_protocol::session::SessionTitle {
                text: "startup recovery".into(),
                source: gents_protocol::session::SessionTitleSource::Placeholder,
            }),
            observation: Some(gents_protocol::session::SessionObservation {
                last_activity_at: RECOVERY_CREATED_AT.into(),
                preview: Some("startup recovery".into()),
                latest_request: Some(gents_protocol::session::SessionRequestObservation {
                    request_doc_id: request_doc_id.clone(),
                    request_id: (request_id).to_string(),
                    lifecycle_state:
                        gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
                }),
            }),
            ..support::session_document(session_id, AGENT_NAME, RECOVERY_CREATED_AT)
        },
    )
    .await;
    let _owner = own_child_fixture(
        &db.node,
        AGENT_DID,
        &request_doc_id,
        request_id,
        session_id,
        true,
    )
    .await;
    seed_expired_execution_tuple(&db.node, &request_doc_id).await;
    insert_inference_call(&db.node, request_id, "running").await;

    let outcome = gents::startup_recovery::run_startup_recovery(&db.node, AGENT_DID).await;
    let requests = outcome.requests.expect("startup request recovery");
    assert!(
        requests.requests_recovered >= 1,
        "crash-stuck parent must terminalize at startup: {requests:?}"
    );
    let calls = outcome.inference_calls.expect("startup inference recovery");
    assert_eq!(
        calls.calls_recovered, 1,
        "crash-orphaned running call must be terminalized in the first startup \
         pass, not survive until the next restart (#1001)"
    );

    let parent = fetch_request_recovery_row(&db.node, request_id).await;
    assert_eq!(
        parent.lifecycle_state.as_str(),
        "failed",
        "crash-stuck parent repairs to failed from its recovery error response"
    );
    let row = fetch_inference_recovery_row(&db.node, request_id).await;
    assert_eq!(
        row.call_state.as_str(),
        "failed",
        "orphaned running call must not keep holding a reconstructed slot"
    );
    let slot_row = InferenceCallSlotRow::new(BACKEND_ID, row.call_state.as_str());
    assert_eq!(
        reconstructed_running_slot_count([slot_row], BACKEND_ID),
        0,
        "post-recovery rows must reconstruct zero held slots"
    );

    let second = gents::startup_recovery::run_startup_recovery(&db.node, AGENT_DID).await;
    assert_eq!(
        second
            .inference_calls
            .expect("second startup inference recovery")
            .calls_recovered,
        0,
        "startup recovery must be idempotent across restarts"
    );
}

/// Lean: `Recovery.deferred_startup_then_expired_periodic_converges`.
/// An ungraceful restart may precede lease expiry, so startup ordering alone
/// cannot converge the linked inference row. Drive the real periodic registry.
#[tokio::test]
async fn live_startup_lease_expiry_converges_inference_rows_through_periodic_registry() {
    for (initial_call_state, terminal_call_state) in
        [("running", "failed"), ("queued", "cancelled")]
    {
        let db = test_db(&format!("deferred-inference-{initial_call_state}")).await;
        let request_id = format!("deferred-inference-{initial_call_state}");
        let session_id = format!("deferred-inference-{initial_call_state}-session");
        let doc_id = create_request(
            &db.node,
            &request_id,
            &session_id,
            "pending",
            RECOVERY_CREATED_AT,
        )
        .await;
        create_agent_session(&db.node, &session_id, AGENT_NAME, RECOVERY_CREATED_AT).await;
        support::seed_session_observation(
            &db.node,
            &session_id,
            &gents_protocol::session::SessionObservation {
                last_activity_at: RECOVERY_CREATED_AT.into(),
                preview: Some("deferred recovery".into()),
                latest_request: Some(gents_protocol::session::SessionRequestObservation {
                    request_doc_id: doc_id.clone(),
                    request_id: request_id.clone(),
                    lifecycle_state:
                        gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
                }),
            },
        )
        .await;
        // Retain the fixture owner: dropping it would relinquish the lease,
        // unlike the hard process loss represented by these persisted rows.
        let _owner =
            own_child_fixture(&db.node, AGENT_DID, &doc_id, &request_id, &session_id, true).await;
        insert_inference_call(&db.node, &request_id, initial_call_state).await;
        let startup = gents::startup_recovery::run_startup_recovery(&db.node, AGENT_DID).await;
        startup.tool_calls.expect("startup tool recovery");
        assert_eq!(
            startup
                .requests
                .expect("startup request recovery")
                .requests_recovered,
            0
        );
        assert_eq!(
            startup
                .inference_calls
                .expect("startup inference recovery")
                .calls_recovered,
            0
        );
        let registry = gents::BackgroundExecutionRegistry::default();
        let live =
            gents::periodic_recovery::run_periodic_recovery_sweeps(&db.node, AGENT_DID, &registry)
                .await
                .expect("live periodic recovery");
        assert!(
            live.iter().all(|run| run.is_noop()),
            "live lease must preserve both rows: {live:?}"
        );
        assert_eq!(
            fetch_request_recovery_row(&db.node, &request_id)
                .await
                .lifecycle_state
                .as_str(),
            "processing"
        );
        assert_eq!(
            fetch_inference_recovery_row(&db.node, &request_id)
                .await
                .call_state,
            initial_call_state
        );

        // Change only the expiry; preserve the generation and progress tuple
        // that the recovery owner must fence when it wins terminalization.
        let expired = db.node.execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_lease_expires_at: "{RECOVERY_CREATED_AT}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&doc_id),
        )).await;
        assert!(
            !expired.has_errors(),
            "expire fixture lease: {:?}",
            expired.errors
        );
        let repaired =
            gents::periodic_recovery::run_periodic_recovery_sweeps(&db.node, AGENT_DID, &registry)
                .await
                .expect("expired periodic recovery");
        assert!(
            repaired.iter().any(|run| !run.is_noop()),
            "expired pair requires recovery"
        );
        assert_eq!(
            fetch_request_recovery_row(&db.node, &request_id)
                .await
                .lifecycle_state
                .as_str(),
            "failed"
        );
        let call = fetch_inference_recovery_row(&db.node, &request_id).await;
        assert_eq!(
            call.call_state, terminal_call_state,
            "periodic registry must recover the inference row after its parent"
        );
        assert!(!gents::call_state_holds_backend_slot(&call.call_state));
        let second =
            gents::periodic_recovery::run_periodic_recovery_sweeps(&db.node, AGENT_DID, &registry)
                .await
                .expect("second periodic recovery");
        assert!(
            second.iter().all(|run| run.is_noop()),
            "recovery must be idempotent: {second:?}"
        );
    }
}

async fn drive_request_recovery_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let db = test_db(&format!("recovery-sweep-{}", case.name)).await;
    let request_id = format!("{}-request", case.name);
    let session_id = format!("{}-session", case.name);
    let doc_id = create_request(
        &db.node,
        &request_id,
        &session_id,
        "processing",
        RECOVERY_CREATED_AT,
    )
    .await;
    seed_expired_execution_tuple(&db.node, &doc_id).await;
    support::create_session_document(
        &db.node,
        &gents_protocol::session::AgentSession {
            title: Some(gents_protocol::session::SessionTitle {
                text: "recovery request".into(),
                source: gents_protocol::session::SessionTitleSource::Placeholder,
            }),
            observation: Some(gents_protocol::session::SessionObservation {
                last_activity_at: RECOVERY_CREATED_AT.into(),
                preview: Some("recovery request".into()),
                latest_request: Some(gents_protocol::session::SessionRequestObservation {
                    request_doc_id: doc_id.clone(),
                    request_id: (&request_id).to_string(),
                    lifecycle_state:
                        gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
                }),
            }),
            ..support::session_document(&session_id, AGENT_NAME, RECOVERY_CREATED_AT)
        },
    )
    .await;
    set_request_lifecycle_state(&db.node, &doc_id, case.pre_state.as_str()).await;
    if case.terminal_state == "interrupted" {
        set_interrupt_requested_at(&db.node, &doc_id, "2026-07-09T00:00:00Z").await;
    }

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "request recovery case {} should recover one request",
        case.name
    );

    let row = fetch_request_recovery_row(&db.node, &request_id).await;
    assert_eq!(
        row.lifecycle_state.as_str(),
        case.terminal_state.as_str(),
        "request recovery case {} terminal state drifted",
        case.name
    );
}

async fn drive_inference_call_recovery_case(case: &lean_vocab_test::LeanRecoverySweepCase) {
    let db = test_db(&format!("recovery-sweep-{}", case.name)).await;
    let request_id = format!("{}-request", case.name);
    let session_id = format!("{}-session", case.name);
    let parent_doc_id = create_request(
        &db.node,
        &request_id,
        &session_id,
        "processing",
        RECOVERY_CREATED_AT,
    )
    .await;
    match case.name.as_str() {
        "inference_interrupted_parent_to_cancelled" => {
            set_request_lifecycle_state(&db.node, &parent_doc_id, "interrupted").await;
        }
        "inference_queued_stale_to_cancelled" | "inference_running_stale_to_failed" => {
            set_request_lifecycle_state(&db.node, &parent_doc_id, "completed").await;
        }
        other => panic!("unhandled inference recovery case {other}"),
    }
    insert_inference_call(&db.node, &request_id, case.pre_state.as_str()).await;

    let report = InferenceCall::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.calls_recovered, 1,
        "inference recovery case {} should recover one call",
        case.name
    );

    let row = fetch_inference_recovery_row(&db.node, &request_id).await;
    assert_eq!(
        row.call_state.as_str(),
        case.terminal_state.as_str(),
        "inference recovery case {} terminal state drifted",
        case.name
    );
    let terminal_row = InferenceCallSlotRow::new(BACKEND_ID, row.call_state.as_str());
    assert_eq!(slot_contribution(terminal_row, BACKEND_ID), 0);
    assert_eq!(
        reconstructed_running_slot_count([terminal_row], BACKEND_ID),
        0,
        "terminal InferenceCall recovery case {} must reconstruct zero running slots",
        case.name
    );
}

async fn insert_inference_call(node: &EmbeddedNode, request_id: &str, call_state: &str) {
    let call_id = format!("{request_id}-call");
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            add_InferenceCall(input: {{
                call_id: "{call_id}",
                runtime_instance_id: "runtime-recovery-test",
                request_id: "{request_id}",
                call_seq: 1,
                backend_id: "{BACKEND_ID}",
                behavior_id: "{AGENT_NAME}",
                agent_did: "{AGENT_DID}",
                call_kind: "inference",
                attempt: 1,
                call_state: "{call_state}",
                queued_at: "{now}",
                started_at: "{now}",
                priority: 0,
                queue_depth_at_enqueue: 0,
                controller_generation: 0,
                backend_config_fingerprint: "test"
            }}) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call_id),
        request_id = escape_graphql_string(request_id),
        call_state = escape_graphql_string(call_state),
        now = escape_graphql_string(&now),
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "insert inference call failed: {:?}",
        resp.errors
    );
}

#[derive(Debug)]
struct RequestRecoveryRow {
    lifecycle_state: RequestLifecycleState,
    failure_reason: Option<String>,
    terminal_output: Option<gents_protocol::output::TerminalOutput>,
    terminalized_at: Option<String>,
}

async fn fetch_request_recovery_row(node: &EmbeddedNode, request_id: &str) -> RequestRecoveryRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                request_id
                lifecycle_state failure_reason terminal_output terminalized_at
            }}
        }}"#
    );
    let row: gents_protocol::row::AgentRequestRow =
        first_row(&node.execute(&query).await, "AgentRequest");
    RequestRecoveryRow {
        lifecycle_state: row
            .lifecycle_state
            .expect("AgentRequest.lifecycle_state must be present"),
        failure_reason: row.failure_reason,
        terminal_output: row.terminal_output,
        terminalized_at: row.terminalized_at,
    }
}

#[derive(Debug, Deserialize)]
struct InferenceRecoveryRow {
    call_state: String,
}

async fn fetch_inference_recovery_row(
    node: &EmbeddedNode,
    request_id: &str,
) -> InferenceRecoveryRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                call_state
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "InferenceCall")
}

async fn own_child_fixture(
    node: &Arc<defra_node::EmbeddedNode>,
    agent_did: &str,
    doc_id: &str,
    request_id: &str,
    session_id: &str,
    processing: bool,
) -> RequestLifecycle {
    let mut request = build_request(
        doc_id.to_owned(),
        request_id.to_owned(),
        session_id.to_owned(),
        RECOVERY_CREATED_AT.to_owned(),
    );
    request.agent_did = agent_did.to_owned();
    let mut owner = RequestLifecycle::new_with_agent_did(
        node.clone(),
        AGENT_NAME,
        agent_did,
        request,
        DEADLINE_SECS,
    );
    assert_eq!(owner.claim().await.unwrap(), ClaimOutcome::Claimed);
    if processing {
        crate::support::begin_owned_execution(&mut owner, node)
            .await
            .unwrap();
    }
    owner
}

async fn seed_expired_execution_tuple(node: &EmbeddedNode, request_doc_id: &str) {
    let response = node.execute(&format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{
            execution_generation: "{}", execution_lease_expires_at: "{RECOVERY_CREATED_AT}", execution_progress_seq: 0
        }}) {{ _docID }} }}"#,
        escape_graphql_string(request_doc_id), uuid::Uuid::new_v4(),
    )).await;
    assert!(
        !response.has_errors(),
        "seed expired execution tuple: {:?}",
        response.errors
    );
}

/// The generated request-recovery witnesses exercise the canonical request
/// owner directly. This regression pins the pre-output crash shape: the
/// owner must persist an explicit `NoMessage` terminal selection, and a
/// latched interrupt must still outrank the default Failed disposition.
#[tokio::test]
async fn request_recovery_before_output_persists_canonical_terminal_selection() {
    // Sub-case A: no published output and no interrupt latch -> Failed with
    // the canonical recovery reason and an explicit NoMessage selection.
    let db = test_db("recovery-no-response-failed").await;
    let request_id = "recovery-no-response-request";
    let session_id = "recovery-no-response-session";
    let doc_id = create_request(
        &db.node,
        request_id,
        session_id,
        "processing",
        RECOVERY_CREATED_AT,
    )
    .await;
    seed_expired_execution_tuple(&db.node, &doc_id).await;
    support::create_session_document(
        &db.node,
        &gents_protocol::session::AgentSession {
            title: Some(gents_protocol::session::SessionTitle {
                text: "recovery no response".into(),
                source: gents_protocol::session::SessionTitleSource::Placeholder,
            }),
            observation: Some(gents_protocol::session::SessionObservation {
                last_activity_at: RECOVERY_CREATED_AT.into(),
                preview: Some("recovery no response".into()),
                latest_request: Some(gents_protocol::session::SessionRequestObservation {
                    request_doc_id: doc_id.clone(),
                    request_id: (request_id).to_string(),
                    lifecycle_state:
                        gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
                }),
            }),
            ..support::session_document(session_id, AGENT_NAME, RECOVERY_CREATED_AT)
        },
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "an expired request without published output must still terminalize"
    );

    let row = fetch_request_recovery_row(&db.node, request_id).await;
    assert_eq!(
        row.lifecycle_state.as_str(),
        "failed",
        "absent output and no interrupt latch must default to failed"
    );
    assert_eq!(
        row.failure_reason.as_deref(),
        Some("execution lease expired"),
        "canonical recovery must carry its terminal reason, got {row:?}"
    );
    assert_eq!(
        row.terminal_output,
        Some(gents_protocol::output::TerminalOutput::NoMessage),
        "pre-output recovery must explicitly select no terminal message"
    );
    assert!(row.terminalized_at.is_some());

    let second = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        second.requests_recovered, 0,
        "a repaired terminal request must leave the active-recovery scope"
    );

    // Sub-case B: no published output but a latched interrupt -> Interrupted
    // wins over the Failed default without fabricating an assistant message.
    let db = test_db("recovery-no-response-interrupted").await;
    let request_id = "recovery-no-response-interrupted-request";
    let session_id = "recovery-no-response-interrupted-session";
    let doc_id = create_request(
        &db.node,
        request_id,
        session_id,
        "processing",
        RECOVERY_CREATED_AT,
    )
    .await;
    seed_expired_execution_tuple(&db.node, &doc_id).await;
    set_interrupt_requested_at(&db.node, &doc_id, "2026-03-23T00:00:30Z").await;
    support::create_session_document(
        &db.node,
        &gents_protocol::session::AgentSession {
            title: Some(gents_protocol::session::SessionTitle {
                text: "recovery no response interrupted".into(),
                source: gents_protocol::session::SessionTitleSource::Placeholder,
            }),
            observation: Some(gents_protocol::session::SessionObservation {
                last_activity_at: RECOVERY_CREATED_AT.into(),
                preview: Some("recovery no response interrupted".into()),
                latest_request: Some(gents_protocol::session::SessionRequestObservation {
                    request_doc_id: doc_id.clone(),
                    request_id: (request_id).to_string(),
                    lifecycle_state:
                        gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
                }),
            }),
            ..support::session_document(session_id, AGENT_NAME, RECOVERY_CREATED_AT)
        },
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "a latched response-less interrupt must terminalize as interrupted"
    );

    let row = fetch_request_recovery_row(&db.node, request_id).await;
    assert_eq!(
        row.lifecycle_state.as_str(),
        "interrupted",
        "a latched interrupt must outrank the absent-response Failed default"
    );
    assert_eq!(
        row.failure_reason.as_deref(),
        Some("execution lease expired"),
        "interrupted recovery retains the canonical recovery reason"
    );
    assert_eq!(
        row.terminal_output,
        Some(gents_protocol::output::TerminalOutput::NoMessage),
        "an interrupted pre-output request must not fabricate an answer"
    );
    assert!(row.terminalized_at.is_some());
}
