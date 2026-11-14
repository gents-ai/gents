use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use support::agent_requests::{RequestActors, RequestSeed};

const RECOVERY_CREATED_AT: &str = "2026-03-23T00:00:00Z";

pub(super) async fn startup_recovery_order_terminalizes_crash_orphaned_calls() {
    let db = test_db("startup-recovery-order-1001").await;
    let actors = RequestActors::new(&db).await;
    let agent = actors.agent().clone();
    let request_id = "startup-order-1001-request";
    let session_id = "startup-order-1001-session";
    let request_doc_id = seed_scoped_request(&actors, request_id, session_id, "pending").await;
    seed_recovery_session(
        &actors,
        session_id,
        Some("startup recovery"),
        &request_doc_id,
        request_id,
    )
    .await;
    let _owner = own_child_fixture(&actors, &request_doc_id, request_id, session_id, true).await;
    seed_expired_execution_tuple(&actors, &request_doc_id).await;
    insert_scoped_inference_call(&actors, &request_doc_id, request_id, session_id, "running").await;

    let outcome = gents::runtime::startup_recovery::run_startup_recovery_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await;
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

    let parent = fetch_request_recovery_row(&actors, request_id).await;
    assert_eq!(
        parent.lifecycle_state.as_str(),
        "failed",
        "crash-stuck parent repairs to failed from its recovery error response"
    );
    let row = fetch_inference_recovery_row(&actors, request_id).await;
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

    let second = gents::runtime::startup_recovery::run_startup_recovery_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await;
    assert_eq!(
        second
            .inference_calls
            .expect("second startup inference recovery")
            .calls_recovered,
        0,
        "startup recovery must be idempotent across restarts"
    );
}

#[tokio::test]
async fn live_startup_lease_expiry_converges_inference_rows_through_periodic_registry() {
    for (initial_call_state, terminal_call_state) in
        [("running", "failed"), ("queued", "cancelled")]
    {
        let db = test_db(&format!("deferred-inference-{initial_call_state}")).await;
        let actors = RequestActors::new(&db).await;
        let agent = actors.agent().clone();
        let request_id = format!("deferred-inference-{initial_call_state}");
        let session_id = format!("deferred-inference-{initial_call_state}-session");
        let doc_id = seed_scoped_request(&actors, &request_id, &session_id, "pending").await;
        seed_recovery_session(&actors, &session_id, None, &doc_id, &request_id).await;
        let _owner = own_child_fixture(&actors, &doc_id, &request_id, &session_id, true).await;
        insert_scoped_inference_call(
            &actors,
            &doc_id,
            &request_id,
            &session_id,
            initial_call_state,
        )
        .await;
        let startup = gents::runtime::startup_recovery::run_startup_recovery_with_identities(
            &db.node,
            db.node_identity.did(),
            std::slice::from_ref(&agent),
        )
        .await;
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
        let registry = gents_toolcalls::call::hook::BackgroundExecutionRegistry::default();
        let live =
            gents_toolcalls::call::periodic_recovery::run_periodic_recovery_sweeps_with_identities(
                &db.node,
                db.node_identity.did(),
                std::slice::from_ref(&agent),
                &registry,
                &gents_automation::goal::session_message::SessionGoals,
            )
            .await
            .expect("live periodic recovery");
        assert!(
            live.iter().all(|run| run.is_noop()),
            "live lease must preserve both rows: {live:?}"
        );
        assert_eq!(
            fetch_request_recovery_row(&actors, &request_id)
                .await
                .lifecycle_state
                .as_str(),
            "processing"
        );
        assert_eq!(
            fetch_inference_recovery_row(&actors, &request_id)
                .await
                .call_state,
            initial_call_state
        );

        actors
            .update_request(
                &doc_id,
                json!({"execution_lease_expires_at": RECOVERY_CREATED_AT}),
            )
            .await;
        let repaired =
            gents_toolcalls::call::periodic_recovery::run_periodic_recovery_sweeps_with_identities(
                &db.node,
                db.node_identity.did(),
                std::slice::from_ref(&agent),
                &registry,
                &gents_automation::goal::session_message::SessionGoals,
            )
            .await
            .expect("expired periodic recovery");
        assert!(
            repaired.iter().any(|run| !run.is_noop()),
            "expired pair requires recovery"
        );
        assert_eq!(
            fetch_request_recovery_row(&actors, &request_id)
                .await
                .lifecycle_state
                .as_str(),
            "failed"
        );
        let call = fetch_inference_recovery_row(&actors, &request_id).await;
        assert_eq!(
            call.call_state, terminal_call_state,
            "periodic registry must recover the inference row after its parent"
        );
        assert!(!gents_inference::admission::call_state_holds_backend_slot(
            &call.call_state
        ));
        let second =
            gents_toolcalls::call::periodic_recovery::run_periodic_recovery_sweeps_with_identities(
                &db.node,
                db.node_identity.did(),
                std::slice::from_ref(&agent),
                &registry,
                &gents_automation::goal::session_message::SessionGoals,
            )
            .await
            .expect("second periodic recovery");
        assert!(
            second.iter().all(|run| run.is_noop()),
            "recovery must be idempotent: {second:?}"
        );
    }
}

fn recovered(count: usize) -> u64 {
    u64::try_from(count).expect("a recovery count fits u64")
}

pub(super) async fn generated_request_recovery_cases_drive_request_owner() {
    for case in gents_lean_vocab::families::recovery_sweeps_request::cases() {
        let input = &case.input;
        let db = test_db(&format!("recovery-sweep-{}", input.name)).await;
        let actors = RequestActors::new(&db).await;
        let request_id = format!("{}-request", input.name);
        let session_id = format!("{}-session", input.name);
        let doc_id = seed_scoped_request(&actors, &request_id, &session_id, "processing").await;
        seed_expired_execution_tuple(&actors, &doc_id).await;
        seed_recovery_session(
            &actors,
            &session_id,
            Some("recovery request"),
            &doc_id,
            &request_id,
        )
        .await;
        actors
            .set_lifecycle_state(
                &doc_id,
                RequestLifecycleState::parse(&input.pre_state).expect("Lean pre-state"),
            )
            .await;
        if input.interrupt_requested {
            actors
                .set_interrupt_requested_at(&doc_id, "2026-07-09T00:00:00Z")
                .await;
        }
        let recover = || {
            RequestLifecycle::recover_all_with_identities(
                &db.node,
                db.node_identity.did(),
                std::slice::from_ref(actors.agent()),
            )
        };
        let first = recover().await.unwrap();
        let row = fetch_request_recovery_row(&actors, &request_id).await;
        let second = recover().await.unwrap();
        case.check(gents_lean_vocab::families::recovery_sweeps_request::Swept {
            terminal_state: row.lifecycle_state.as_str().to_owned(),
            measure_before: recovered(first.requests_recovered),
            measure_after: recovered(second.requests_recovered),
        });
    }
}

pub(super) async fn generated_inference_recovery_cases_drive_inference_owner() {
    for case in gents_lean_vocab::families::recovery_sweeps_inference::cases() {
        let input = &case.input;
        let db = test_db(&format!("recovery-sweep-{}", input.name)).await;
        let actors = RequestActors::new(&db).await;
        let request_id = format!("{}-request", input.name);
        let session_id = format!("{}-session", input.name);
        let parent_doc_id =
            seed_scoped_request(&actors, &request_id, &session_id, "processing").await;
        insert_scoped_inference_call(
            &actors,
            &parent_doc_id,
            &request_id,
            &session_id,
            input.pre_state.as_str(),
        )
        .await;
        let parent_state = if input.cause == "interruptedParent" {
            RequestLifecycleState::Interrupted
        } else {
            RequestLifecycleState::Completed
        };
        actors
            .set_lifecycle_state(&parent_doc_id, parent_state)
            .await;
        let recover = || {
            InferenceCall::recover_all_with_identities(
                &db.node,
                db.node_identity.did(),
                std::slice::from_ref(actors.agent()),
            )
        };
        let first = recover().await.unwrap();
        let row = fetch_inference_recovery_row(&actors, &request_id).await;
        let second = recover().await.unwrap();
        let terminal_row = InferenceCallSlotRow::new(BACKEND_ID, row.call_state.as_str());
        assert_eq!(slot_contribution(terminal_row, BACKEND_ID), 0);
        assert_eq!(
            reconstructed_running_slot_count([terminal_row], BACKEND_ID),
            0,
            "terminal InferenceCall recovery case {} must reconstruct zero running slots",
            input.name
        );
        case.check(
            gents_lean_vocab::families::recovery_sweeps_inference::Swept {
                terminal_state: row.call_state,
                measure_before: recovered(first.calls_recovered),
                measure_after: recovered(second.calls_recovered),
            },
        );
    }
}

async fn seed_scoped_request(
    actors: &RequestActors,
    request_id: &str,
    session_id: &str,
    state: &str,
) -> String {
    actors
        .create_request(RequestSeed::new(request_id, session_id, RECOVERY_CREATED_AT).state(state))
        .await
}

/// Titles the Agent-owned session and projects the processing request into it.
async fn seed_recovery_session(
    actors: &RequestActors,
    session_id: &str,
    title: Option<&str>,
    request_doc_id: &str,
    request_id: &str,
) {
    if let Some(text) = title {
        let session_doc = actors
            .session_doc(session_id)
            .await
            .expect("seeded request created its session");
        let title = gents_protocol::session::SessionTitle {
            text: text.into(),
            source: gents_protocol::session::SessionTitleSource::Placeholder,
        };
        let input = gents_protocol::graphql::graphql_input_literal(&json!({"title": title}))
            .expect("render session title fixture");
        actors
            .agent_access
            .write(
                "test.recovery_sweeps.session_title",
                &format!(
                    r#"mutation {{ update_AgentSession(docID: "{}", input: {input}) {{ _docID }} }}"#,
                    escape_graphql_string(&session_doc)
                ),
            )
            .await
            .expect("title session as its Agent");
    }
    let observation = gents_protocol::session::SessionObservation {
        reply_preview: None,
        last_activity_at: RECOVERY_CREATED_AT.into(),
        preview: Some(title.unwrap_or("recovery request").into()),
        latest_request: Some(gents_protocol::session::SessionRequestObservation {
            request_doc_id: request_doc_id.to_owned(),
            request_id: request_id.to_owned(),
            lifecycle_state: RequestLifecycleState::Processing,
        }),
    };
    actors
        .seed_session_observation(session_id, &observation)
        .await;
}

/// Writes the call as its working Agent, related to the request's session as
/// the admission owner relates it.
async fn insert_scoped_inference_call(
    actors: &RequestActors,
    request_doc_id: &str,
    request_id: &str,
    session_id: &str,
    call_state: &str,
) {
    let now = support::agent_requests::canonical_now();
    let call_id = format!("{request_id}-call");
    let mutation = format!(
        r#"mutation {{ add_InferenceCall(input: {{
            call_id: "{}", runtime_instance_id: "runtime-recovery-test",
            request_id: "{}", request_doc_id: "{}", session_id: "{}", call_seq: 1,
            backend_id: "{}", agent_did: "{}", node_did: "{}",
            call_kind: "inference", attempt: 1, call_state: "{}",
            queued_at: "{}", started_at: "{}", priority: 0,
            queue_depth_at_enqueue: 0, controller_generation: 0,
            backend_config_fingerprint: "test"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&call_id),
        escape_graphql_string(request_id),
        escape_graphql_string(request_doc_id),
        escape_graphql_string(session_id),
        escape_graphql_string(BACKEND_ID),
        escape_graphql_string(actors.agent_did()),
        escape_graphql_string(actors.node_did()),
        escape_graphql_string(call_state),
        escape_graphql_string(&now),
        escape_graphql_string(&now),
    );
    let session_doc = actors
        .session_doc(session_id)
        .await
        .expect("inference call session");
    actors
        .agent_access
        .transact_write("test.recovery_sweeps.inference_call", |txn| {
            let mutation = &mutation;
            let session_doc = &session_doc;
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let created = &response["data"]["add_InferenceCall"];
                let doc = created
                    .get("_docID")
                    .or_else(|| created.get(0).and_then(|row| row.get("_docID")))
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("add_InferenceCall returned no _docID"))?
                    .to_owned();
                txn.add_resource_relationship(
                    "InferenceCall",
                    &doc,
                    "parent",
                    "sessions",
                    session_doc,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .expect("insert inference call as its Agent");
}

#[derive(Debug)]
struct RequestRecoveryRow {
    lifecycle_state: RequestLifecycleState,
    failure_reason: Option<String>,
    terminal_output: Option<gents_protocol::output::TerminalOutput>,
    terminalized_at: Option<String>,
}

async fn fetch_request_recovery_row(
    actors: &RequestActors,
    request_id: &str,
) -> RequestRecoveryRow {
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
        first_row(&actors.query(&query).await, "AgentRequest");
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
    actors: &RequestActors,
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
    first_row(&actors.query(&query).await, "InferenceCall")
}

async fn own_child_fixture(
    actors: &RequestActors,
    doc_id: &str,
    request_id: &str,
    session_id: &str,
    processing: bool,
) -> RequestLifecycle {
    let request = actors.build_request(
        doc_id.to_owned(),
        request_id.to_owned(),
        session_id.to_owned(),
        RECOVERY_CREATED_AT.to_owned(),
    );
    let mut owner = actors.lifecycle(
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(owner.claim().await.unwrap(), ClaimOutcome::Claimed);
    if processing {
        let writer = gents_session::request::streaming::DefraStreamWriter::new_managed(
            actors.node.clone(),
            actors.agent().as_ref(),
            std::time::Duration::ZERO,
        )
        .unwrap();
        owner.begin_owned_execution(&writer).await.unwrap();
    }
    owner
}

async fn seed_expired_execution_tuple(actors: &RequestActors, request_doc_id: &str) {
    actors
        .update_request(
            request_doc_id,
            json!({
                "execution_generation": uuid::Uuid::new_v4().to_string(),
                "execution_lease_expires_at": RECOVERY_CREATED_AT,
            }),
        )
        .await;
}

#[tokio::test]
async fn request_recovery_before_output_persists_canonical_terminal_selection() {
    let db = test_db("recovery-no-response-failed").await;
    let actors = RequestActors::new(&db).await;
    let agent = actors.agent().clone();
    let request_id = "recovery-no-response-request";
    let session_id = "recovery-no-response-session";
    let doc_id = seed_scoped_request(&actors, request_id, session_id, "processing").await;
    seed_expired_execution_tuple(&actors, &doc_id).await;
    seed_recovery_session(
        &actors,
        session_id,
        Some("recovery no response"),
        &doc_id,
        request_id,
    )
    .await;

    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "an expired request without published output must still terminalize"
    );

    let row = fetch_request_recovery_row(&actors, request_id).await;
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

    let second = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(
        second.requests_recovered, 0,
        "a repaired terminal request must leave the active-recovery scope"
    );

    let db = test_db("recovery-no-response-interrupted").await;
    let actors = RequestActors::new(&db).await;
    let agent = actors.agent().clone();
    let request_id = "recovery-no-response-interrupted-request";
    let session_id = "recovery-no-response-interrupted-session";
    let doc_id = seed_scoped_request(&actors, request_id, session_id, "processing").await;
    seed_expired_execution_tuple(&actors, &doc_id).await;
    actors
        .set_interrupt_requested_at(&doc_id, "2026-03-23T00:00:30Z")
        .await;
    seed_recovery_session(
        &actors,
        session_id,
        Some("recovery no response interrupted"),
        &doc_id,
        request_id,
    )
    .await;

    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "a latched response-less interrupt must terminalize as interrupted"
    );

    let row = fetch_request_recovery_row(&actors, request_id).await;
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
