use super::*;
use gents::request::lifecycle::RequestTerminalOutcome;

use gents_protocol::request_lifecycle::RequestLifecycleState;

fn parsed_request_state(value: &str) -> RequestLifecycleState {
    RequestLifecycleState::parse(value).unwrap_or_else(|error| {
        panic!("invalid generated request lifecycle state {value:?}: {error}")
    })
}

fn rust_request_transition_action(from: &str, to: &str) -> Option<&'static str> {
    match (from, to) {
        ("workspaceBindingPending", "pending") => Some("bindWorkspace"),
        ("pending", "claimed") => Some("claim"),
        ("pending", "failed") => Some("admissionReject"),
        ("pending" | "workspaceBindingPending", "superseded") => Some("dedupLose"),
        ("claimed", "processing") => Some("beginInference"),
        ("processing", "processing") => Some("continueProcessing"),
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

fn rust_request_recovery_sweep_writer(from: &str, to: &str) -> Option<&'static str> {
    match (from, to) {
        ("claimed", "dead") | ("processing", "dead") => {
            Some("RequestLifecycle::repair_terminal_requests")
        }
        _ => None,
    }
}

#[tokio::test]
async fn ordinary_completion_rejects_claimed_without_execution() {
    let db = support::test_db("ordinary-complete-from-claimed").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let (access, agent_doc, session_doc) =
        super::session_control::seed_agent_session(&db, &fixture, &session_id, &created_at).await;
    let doc_id = super::session_control::seed_agent_request(
        &db,
        &fixture,
        &access,
        &agent_doc,
        &session_doc,
        &session_id,
        &request_id,
        &created_at,
    )
    .await;
    let mut request = build_request(doc_id.clone(), request_id, session_id, created_at);
    request.node_did = db.node_identity.did().to_owned();
    request.agent_did = fixture.agent.did().to_owned();
    request.requester_did = Some(fixture.agent.did().to_owned());
    request.content = "session control witness".into();
    request.execution_origin = Some("interactive".into());
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        fixture.agent.did(),
        db.node_identity.did(),
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    lifecycle
        .bind_managed_agent(fixture.agent.as_ref())
        .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    assert_eq!(
        lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap(),
        gents::request::lifecycle::TerminalizeResult::Lost,
    );
    let response = access
        .execute(&format!(
            "{{ AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{lifecycle_state}} }}",
            escape_graphql_string(&doc_id),
        ))
        .await
        .unwrap();
    assert_eq!(
        response["data"]["AgentRequest"][0]["lifecycle_state"],
        "claimed"
    );
}

#[tokio::test]
async fn claim_atomically_projects_the_request_owned_session() {
    let db = support::test_db("claim-atomically-projects-session").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let (access, agent_doc, session_doc) =
        super::session_control::seed_agent_session(&db, &fixture, &session_id, &created_at).await;
    let doc_id = super::session_control::seed_agent_request(
        &db,
        &fixture,
        &access,
        &agent_doc,
        &session_doc,
        &session_id,
        &request_id,
        &created_at,
    )
    .await;
    let mut request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    request.node_did = db.node_identity.did().to_owned();
    request.agent_did = fixture.agent.did().to_owned();
    request.requester_did = Some(fixture.agent.did().to_owned());
    request.content = "session control witness".into();
    request.execution_origin = Some("interactive".into());
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        fixture.agent.did(),
        db.node_identity.did(),
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    lifecycle
        .bind_managed_agent(fixture.agent.as_ref())
        .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let response = access
        .execute(&format!(
            "{{ AgentSession(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ {} }} }}",
            escape_graphql_string(&session_doc),
            gents::session::AGENT_SESSION_FIELDS,
        ))
        .await
        .unwrap();
    let session = gents::session::decode_session_row(&response["data"]["AgentSession"][0]).unwrap();
    assert_eq!(session.session.session_id, session_id);
    assert_eq!(session.session.node_did, db.node_identity.did());
    assert_eq!(session.session.agent_did, fixture.agent.did());
    assert_eq!(
        session.session.requester_did.as_deref(),
        Some(fixture.agent.did())
    );
    let latest = session
        .session
        .observation
        .expect("session observation")
        .latest_request
        .expect("latest request");
    assert_eq!(latest.request_doc_id, doc_id);
    assert_eq!(latest.request_id, request_id);
    assert_eq!(latest.lifecycle_state, RequestLifecycleState::Claimed);
}

#[tokio::test]
async fn admission_rejection_persists_despite_an_undecodable_session_row_under_its_label() {
    let db = test_db("admission-rejection-undecodable-session-row").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    actors
        .create_session_with(json!({
            "session_id": session_id,
            "node_did": actors.node_did(),
            "requester_did": actors.agent_did(),
            "agent_did": actors.agent_did(),
            "created_at": "not-an-rfc3339-timestamp",
            "relationships_pending": false,
        }))
        .await;
    let doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&request_id, &session_id, &created_at)
                .state("pending"),
        )
        .await;
    let mut lifecycle = request_lifecycle_for_case(
        &actors,
        doc_id.clone(),
        request_id,
        session_id.clone(),
        created_at,
    );

    lifecycle
        .reject_admission("session label holds an undecodable row")
        .await
        .unwrap();

    assert_eq!(
        fetch_request_snapshot(&actors, &doc_id)
            .await
            .lifecycle_state,
        RequestLifecycleState::Failed
    );
    let selected = actors
        .agent_access
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&session_id),
            gents::session::AGENT_SESSION_FIELDS,
        ))
        .await
        .unwrap();
    let rows = selected["data"]["AgentSession"]
        .as_array()
        .cloned()
        .unwrap();
    assert_eq!(rows.len(), 1, "the refusal must not mint beside the row");
    assert!(
        gents::session::decode_session_row(&rows[0]).is_err(),
        "the refusal must leave the undecodable row untouched"
    );
}

#[tokio::test]
async fn admission_rejection_projects_the_failed_turn_into_its_session() {
    let db = test_db("admission-rejection-projects-session").await;
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
    let mut lifecycle = request_lifecycle_for_case(
        &actors,
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );

    lifecycle
        .reject_admission("session projection rejected")
        .await
        .unwrap();

    let request = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(request.lifecycle_state, RequestLifecycleState::Failed);
    assert_session_observes_request(
        &actors,
        &session_id,
        &request_id,
        RequestLifecycleState::Failed,
    )
    .await;
    let persisted = &actors
        .request_row(&doc_id, "terminal_output failure_reason")
        .await;
    assert_eq!(
        serde_json::from_value::<gents_protocol::output::TerminalOutput>(
            persisted["terminal_output"].clone()
        )
        .unwrap(),
        gents_protocol::output::TerminalOutput::NoMessage,
    );
    assert_eq!(persisted["failure_reason"], "session projection rejected");
}

#[tokio::test]
async fn terminalizing_an_older_request_preserves_the_latest_projection() {
    let db = test_db("older-request-terminal-preserves-latest").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let first_request_id = uuid::Uuid::new_v4().to_string();
    let first_doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&first_request_id, &session_id, &created_at)
                .state("pending"),
        )
        .await;
    let mut first = request_lifecycle_for_case(
        &actors,
        first_doc_id.clone(),
        first_request_id,
        session_id.clone(),
        created_at.clone(),
    );
    assert_eq!(first.claim().await.unwrap(), ClaimOutcome::Claimed);

    let second_request_id = uuid::Uuid::new_v4().to_string();
    let second_doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(&second_request_id, &session_id, &created_at)
                .state("pending"),
        )
        .await;
    actors
        .seed_session_observation(
            &session_id,
            &gents_protocol::session::SessionObservation {
                reply_preview: None,
                last_activity_at: created_at.clone(),
                preview: Some("newer request".into()),
                latest_request: Some(gents_protocol::session::SessionRequestObservation {
                    request_doc_id: second_doc_id,
                    request_id: second_request_id.clone(),
                    lifecycle_state: RequestLifecycleState::Pending,
                }),
            },
        )
        .await;

    crate::support::begin_owned_execution(&mut first, &db.node)
        .await
        .unwrap();
    first
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        fetch_request_snapshot(&actors, &first_doc_id)
            .await
            .lifecycle_state,
        RequestLifecycleState::Completed
    );
    assert_session_observes_request(
        &actors,
        &session_id,
        &(second_request_id),
        RequestLifecycleState::Pending,
    )
    .await;
}

fn request_lifecycle_for_case(
    actors: &support::agent_requests::RequestActors,
    doc_id: String,
    request_id: String,
    session_id: String,
    created_at: String,
) -> RequestLifecycle {
    let request = actors.build_request(doc_id, request_id, session_id, created_at);
    actors.lifecycle(
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    )
}

async fn drive_generated_request_legal_case(case: &LeanLifecycleTransitionCase) {
    let db = test_db("generated-request-transition").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let action = case
        .action
        .as_deref()
        .expect("legal Request transition case must carry an action");
    let valid_until = (action == "expire").then(|| {
        support::agent_requests::canonical_at(chrono::Utc::now() - chrono::Duration::seconds(1))
    });
    let request_input =
        (action == "dedupLose").then(|| coalesce_input(&format!("dedup-{request_id}")));
    let initial_state = if case.from == "workspaceBindingPending" {
        "workspaceBindingPending"
    } else {
        "pending"
    };
    let seed = support::agent_requests::RequestSeed {
        input: request_input.as_deref(),
        ..support::agent_requests::RequestSeed::new(&request_id, &session_id, &created_at)
            .state(initial_state)
            .valid_until(valid_until.as_deref())
    };
    let mut create = actors.request_create(&seed);
    if action == "dedupLose" && case.from == "workspaceBindingPending" {
        create.caused_by_trigger_id = Some("conformance-trigger".into());
    }
    let doc_id = actors.create_signed(create).await;
    let mut lifecycle = request_lifecycle_for_case(
        &actors,
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at.clone(),
    );

    match action {
        "bindWorkspace" => {
            activate_workspace_fixture(&actors, &doc_id)
                .await
                .expect("bindWorkspace CAS mutation failed");
        }
        "claim" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
        }
        "dedupLose" => {
            if case.from == "workspaceBindingPending" {
                let node_did = actors.node_did();
                actors
                    .agent_access
                    .transact_write("test.supersede_before_workspace_binding", |txn| {
                        Box::pin(async move {
                            gents::__test_internals::supersede_trigger_requests_in_txn(
                                txn,
                                node_did,
                                "conformance-trigger",
                                None,
                            )
                            .await?;
                            Ok::<_, anyhow::Error>(())
                        })
                    })
                    .await
                    .unwrap();
            } else {
                let survivor_id = uuid::Uuid::new_v4().to_string();
                let survivor_created_at = support::agent_requests::canonical_at(
                    chrono::Utc::now() - chrono::Duration::seconds(30),
                );
                let key = format!("dedup-{request_id}");
                let survivor_input = coalesce_input(&key);
                let survivor_doc_id = actors
                    .create_request(support::agent_requests::RequestSeed {
                        input: Some(&survivor_input),
                        ..support::agent_requests::RequestSeed::new(
                            &survivor_id,
                            &session_id,
                            &survivor_created_at,
                        )
                        .state("pending")
                    })
                    .await;
                gents::__test_internals::reconcile_coalesced_pending_request_for_with_access(
                    (&db.node, &actors.agent_access),
                    actors.agent_did(),
                    &session_id,
                    actors.node_did(),
                    gents::__test_internals::QueueSource::User,
                    &key,
                    None,
                )
                .await
                .expect("coalesce reconcile must succeed");
                let _ = survivor_doc_id;
            }
        }
        "admissionReject" => {
            lifecycle
                .reject_admission("session projection rejected")
                .await
                .unwrap();
        }
        "beginInference" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                .await
                .unwrap();
        }
        "continueProcessing" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                .await
                .unwrap();
        }
        "finish" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                .await
                .unwrap();
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Completed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
        }
        "fail" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            crate::support::begin_owned_execution(&mut lifecycle, &db.node)
                .await
                .unwrap();
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Failed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
        }
        "failBeforeStream" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Failed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
        }
        "expire" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);
        }
        "interruptBeforeClaim" => {
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            actors
                .set_interrupt_requested_at(&doc_id, &interrupt_at)
                .await;
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
        }
        "interruptClaimed" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            actors
                .set_interrupt_requested_at(&doc_id, &interrupt_at)
                .await;
            lifecycle
                .terminalize_owned(
                    RequestTerminalOutcome::Interrupted,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("interrupted"),
                )
                .await
                .unwrap();
        }
        "interruptProcessing" => {
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
                    RequestTerminalOutcome::Interrupted,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("interrupted"),
                )
                .await
                .unwrap();
        }
        other => panic!(
            "generated Request transition {} has unsupported action {other:?}",
            case.name
        ),
    }

    let evidence = actors.request_row(&doc_id, "stopped_before_running").await;
    let certified = evidence["stopped_before_running"].as_bool() == Some(true);
    assert_eq!(certified, case.stopped_before_running, "{}", case.name);
    let snap = fetch_request_snapshot(&actors, &doc_id).await;
    assert_eq!(
        snap.lifecycle_state,
        parsed_request_state(&case.to),
        "generated Request transition {} expected {} -> {} classified as {} via {:?}, got persisted lifecycle_state={}",
        case.name,
        case.from,
        case.to,
        case.classification,
        case.action,
        snap.lifecycle_state
    );
}

pub(super) async fn generated_request_transition_cases_cover_lifecycle_policy() {
    for case in lean_request_transition_cases() {
        let source = parsed_request_state(&case.from);
        let target = parsed_request_state(&case.to);
        assert_eq!(
            source.certifies_stopped_before_running(target, true),
            case.stopped_before_running
        );
        assert!(!source.certifies_stopped_before_running(target, false));
        match case.classification.as_str() {
            "legal" => {
                assert_eq!(
                    case.action.as_deref(),
                    rust_request_transition_action(&case.from, &case.to),
                    "Request transition {} legal writer action drifted for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                assert!(
                    rust_request_recovery_sweep_writer(&case.from, &case.to).is_none(),
                    "Request transition {} is legal but a Rust recovery sweep also claims {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                drive_generated_request_legal_case(case).await;
            }
            "illegal" => {
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
            }
            other => panic!(
                "generated Request transition {} has unknown classification {other:?}",
                case.name
            ),
        }
    }
}

fn assert_terminal_lifecycle_state(lifecycle_state: &str) {
    assert!(
        matches!(
            lifecycle_state,
            "completed" | "failed" | "superseded" | "dead" | "interrupted"
        ),
        "not a terminal lifecycle_state: {lifecycle_state}"
    );
}

async fn force_terminal_persisted_state(
    actors: &support::agent_requests::RequestActors,
    doc_id: &str,
    lifecycle_state: &str,
) {
    assert_terminal_lifecycle_state(lifecycle_state);
    force_persisted_lifecycle_state(actors, doc_id, lifecycle_state).await;
}

fn coalesce_input(key: &str) -> String {
    serde_json::json!({
        "queue": {
            "source": "user",
            "policy": "coalesce",
            "key": key,
            "queued_after_request_id": null,
        }
    })
    .to_string()
}

async fn force_persisted_lifecycle_state(
    actors: &support::agent_requests::RequestActors,
    doc_id: &str,
    lifecycle_state: &str,
) {
    actors
        .set_lifecycle_state(doc_id, parsed_request_state(lifecycle_state))
        .await;
}

async fn scoped_matrix_request(
    actors: &support::agent_requests::RequestActors,
    request_id: &str,
    session_id: &str,
    created_at: &str,
    valid_until: Option<&str>,
    input: Option<&str>,
) -> String {
    actors
        .create_request(support::agent_requests::RequestSeed {
            input,
            ..support::agent_requests::RequestSeed::new(request_id, session_id, created_at)
                .valid_until(valid_until)
        })
        .await
}

fn scoped_matrix_lifecycle(
    db: &support::TestDb,
    agent: &dyn gents::NodeIdentity,
    doc_id: String,
    request_id: String,
    session_id: String,
    created_at: String,
) -> RequestLifecycle {
    let mut request = support::build_request(doc_id, request_id, session_id, created_at);
    request.node_did = db.node_identity.did().to_owned();
    request.agent_did = agent.did().to_owned();
    request.requester_did = Some(agent.did().to_owned());
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        agent.did(),
        db.node_identity.did(),
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    lifecycle.bind_managed_agent(agent).unwrap();
    lifecycle
}

#[tokio::test]
async fn production_request_writers_only_reach_contracted_edges() {
    const START_STATES: [&str; 9] = [
        "pending",
        "claimed",
        "processing",
        "completed",
        "failed",
        "superseded",
        "dead",
        "interrupted",
        "workspaceBindingPending",
    ];
    const WRITERS: [&str; 11] = [
        "claim",
        "admission_reject",
        "claim_after_ttl_lapse",
        "claim_after_interrupt",
        "begin_execution",
        "complete",
        "fail",
        "interrupt",
        "repair_terminal_requests",
        "coalesce_pending",
        "bind_workspace",
    ];

    let mut observed: std::collections::BTreeSet<(String, String)> = Default::default();

    for start in START_STATES {
        let db = test_db(&format!("production-edges-{}", start.to_lowercase())).await;
        let actors = support::agent_requests::RequestActors::new(&db).await;
        let agent = actors.agent().clone();
        let access = actors.agent_access.clone();

        for writer in WRITERS {
            let request_id = uuid::Uuid::new_v4().to_string();
            let session_id = uuid::Uuid::new_v4().to_string();
            let created_at = support::agent_requests::canonical_now();
            let valid_until = (writer == "claim_after_ttl_lapse").then(|| {
                support::agent_requests::canonical_at(
                    chrono::Utc::now() - chrono::Duration::seconds(1),
                )
            });
            let coalesce =
                (writer == "coalesce_pending").then(|| coalesce_input("conformance-key"));
            let doc_id = scoped_matrix_request(
                &actors,
                &request_id,
                &session_id,
                &created_at,
                valid_until.as_deref(),
                coalesce.as_deref(),
            )
            .await;
            let mut lifecycle = scoped_matrix_lifecycle(
                &db,
                agent.as_ref(),
                doc_id.clone(),
                request_id.clone(),
                session_id.clone(),
                created_at.clone(),
            );

            match writer {
                "claim"
                | "admission_reject"
                | "claim_after_ttl_lapse"
                | "claim_after_interrupt"
                | "coalesce_pending"
                | "bind_workspace" => {}
                _ => {
                    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
                }
            }

            if start == "processing" && matches!(writer, "complete" | "fail" | "interrupt") {
                let writer = gents::DefraStreamWriter::new_managed(
                    db.node.clone(),
                    agent.as_ref(),
                    Duration::ZERO,
                )
                .unwrap();
                lifecycle.begin_owned_execution(&writer).await.unwrap();
            }
            if writer == "claim_after_interrupt" {
                actors
                    .set_interrupt_requested_at(&doc_id, &chrono::Utc::now().to_rfc3339())
                    .await;
            }
            if writer == "repair_terminal_requests" {
                let expiry = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
                actors
                    .update_request(&doc_id, json!({ "execution_lease_expires_at": expiry }))
                    .await;
            }

            if writer == "coalesce_pending" {
                let survivor_id = uuid::Uuid::new_v4().to_string();
                let survivor_created_at = support::agent_requests::canonical_at(
                    chrono::Utc::now() - chrono::Duration::seconds(30),
                );
                let survivor_input = coalesce_input("conformance-key");
                let survivor_doc_id = scoped_matrix_request(
                    &actors,
                    &survivor_id,
                    &session_id,
                    &survivor_created_at,
                    None,
                    Some(&survivor_input),
                )
                .await;
                let _ = survivor_doc_id;
            }

            force_persisted_lifecycle_state(&actors, &doc_id, start).await;

            let before = fetch_request_snapshot(&actors, &doc_id)
                .await
                .lifecycle_state;
            assert_eq!(
                before,
                parsed_request_state(start),
                "fixture for {start}/{writer} did not reach the intended start state"
            );

            match writer {
                "claim" | "claim_after_interrupt" => {
                    let _ = lifecycle.claim().await;
                }
                "admission_reject" => {
                    let _ = lifecycle.reject_admission("projection rejected").await;
                }
                "claim_after_ttl_lapse" => {
                    let _ = lifecycle.claim().await;
                }
                "begin_execution" => {
                    let writer = gents::DefraStreamWriter::new_managed(
                        db.node.clone(),
                        agent.as_ref(),
                        Duration::ZERO,
                    )
                    .unwrap();
                    let _ = lifecycle.begin_owned_execution(&writer).await;
                }
                "complete" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Completed,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            None,
                        )
                        .await;
                }
                "fail" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Failed,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            None,
                        )
                        .await;
                }
                "interrupt" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Interrupted,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            Some("interrupted"),
                        )
                        .await;
                }
                "repair_terminal_requests" => {
                    let _ = RequestLifecycle::repair_terminal_requests_with_identities(
                        &db.node,
                        db.node_identity.did(),
                        std::slice::from_ref(&agent),
                    )
                    .await;
                }
                "coalesce_pending" => {
                    let _ = gents::__test_internals::reconcile_coalesced_pending_request_for_with_access(
                        (&db.node, &access), agent.did(), &session_id, db.node_identity.did(),
                        gents::__test_internals::QueueSource::User, "conformance-key", None,
                    )
                    .await;
                }
                "bind_workspace" => {
                    let _ =
                        gents::__test_internals::activate_workspace_bound_request_with_identity(
                            &db.node,
                            &doc_id,
                            ::identity::Did::new(agent.did()).unwrap(),
                            Some(agent.as_ref()),
                        )
                        .await;
                }
                other => panic!("unhandled writer {other}"),
            }

            let after = fetch_request_snapshot(&actors, &doc_id)
                .await
                .lifecycle_state;
            if after != before {
                observed.insert((before.as_str().to_string(), after.as_str().to_string()));
            }
        }
    }

    let expected: std::collections::BTreeSet<(String, String)> = [
        ("pending", "claimed"),
        ("pending", "failed"),
        ("pending", "dead"),
        ("pending", "interrupted"),
        ("pending", "superseded"),
        ("claimed", "processing"),
        ("claimed", "failed"),
        ("claimed", "interrupted"),
        ("processing", "completed"),
        ("processing", "failed"),
        ("processing", "interrupted"),
    ]
    .into_iter()
    .map(|(from, to)| (from.to_string(), to.to_string()))
    .collect();
    let missing: Vec<_> = expected.difference(&observed).collect();
    assert!(
        missing.is_empty(),
        "production writers stopped reaching edges this test is supposed to cover: {missing:?}"
    );

    let contract = lean_request_transition_cases();
    for (from, to) in &observed {
        let case = contract
            .iter()
            .find(|case| &case.from == from && &case.to == to)
            .unwrap_or_else(|| {
                panic!("production reached {from} -> {to}, absent from the Lean contract entirely")
            });
        assert!(
            matches!(case.classification.as_str(), "legal" | "recoveryReachable"),
            "production writers reached {from} -> {to}, which the Lean contract classifies {}",
            case.classification
        );
    }
}

#[tokio::test]
async fn terminal_persisted_requests_reject_request_mutating_lifecycle_writers() {
    const TERMINAL_STATES: [&str; 5] = ["completed", "failed", "superseded", "dead", "interrupted"];
    const WRITERS: [&str; 7] = [
        "claim",
        "admission_reject",
        "begin_execution",
        "complete",
        "fail",
        "interrupt",
        "repair_terminal_requests",
    ];

    for terminal in TERMINAL_STATES {
        let db = test_db(&format!("terminal-irreversibility-{terminal}")).await;
        let actors = support::agent_requests::RequestActors::new(&db).await;
        let agent = actors.agent().clone();

        for writer in WRITERS {
            let request_id = uuid::Uuid::new_v4().to_string();
            let session_id = uuid::Uuid::new_v4().to_string();
            let created_at = support::agent_requests::canonical_now();
            let doc_id =
                scoped_matrix_request(&actors, &request_id, &session_id, &created_at, None, None)
                    .await;
            let mut lifecycle = scoped_matrix_lifecycle(
                &db,
                agent.as_ref(),
                doc_id.clone(),
                request_id.clone(),
                session_id.clone(),
                created_at.clone(),
            );

            if writer != "claim" {
                assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            }

            force_terminal_persisted_state(&actors, &doc_id, terminal).await;

            match writer {
                "claim" => {
                    let outcome = lifecycle.claim().await;
                    if let Ok(outcome) = outcome {
                        assert_ne!(
                            outcome,
                            ClaimOutcome::Claimed,
                            "claim() reported success against a persisted {terminal} request"
                        );
                    }
                }
                "admission_reject" => {
                    let _ = lifecycle.reject_admission("projection rejected").await;
                }
                "begin_execution" => {
                    let writer = gents::DefraStreamWriter::new_managed(
                        db.node.clone(),
                        agent.as_ref(),
                        Duration::ZERO,
                    )
                    .unwrap();
                    let _ = lifecycle.begin_owned_execution(&writer).await;
                }
                "complete" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Completed,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            None,
                        )
                        .await;
                }
                "fail" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Failed,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            None,
                        )
                        .await;
                }
                "interrupt" => {
                    let _ = lifecycle
                        .terminalize_owned(
                            RequestTerminalOutcome::Interrupted,
                            gents_protocol::output::TerminalOutput::NoMessage,
                            Some("interrupted"),
                        )
                        .await;
                }
                "repair_terminal_requests" => {
                    let report = RequestLifecycle::repair_terminal_requests_with_identities(
                        &db.node,
                        db.node_identity.did(),
                        std::slice::from_ref(&agent),
                    )
                    .await
                    .expect("terminal repair sweep must succeed");
                    assert_eq!(
                        report.repaired, 0,
                        "terminal repair moved a persisted {terminal} request: {report:?}"
                    );
                }
                other => panic!("unhandled writer {other}"),
            };

            let snap = fetch_request_snapshot(&actors, &doc_id).await;
            assert_eq!(
                snap.lifecycle_state,
                parsed_request_state(terminal),
                "writer {writer} moved a persisted {terminal} request to {} — terminal states must be irreversible (S1)",
                snap.lifecycle_state
            );
        }
    }
}

#[tokio::test]
async fn interactive_claim_snapshot_matches_claimed_waiting() {
    let db = test_db("interactive-claim").await;
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
    assert_lean_transition_is_legal("Request", "pending", "claimed");

    assert_eq!(
        fetch_request_snapshot(&actors, &doc_id).await,
        RequestSnapshot {
            lifecycle_state: RequestLifecycleState::Claimed,
            agent_did: actors.agent_did().into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: gents::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
}

#[tokio::test]
async fn interactive_claim_atomically_pins_session_agent() {
    let db = test_db("interactive-claim-session-projection").await;
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

    assert_session_observes_request(
        &actors,
        &session_id,
        &(request_id),
        RequestLifecycleState::Claimed,
    )
    .await;
}

#[tokio::test]
async fn interactive_admission_and_progress_snapshots_match_execution_flow() {
    let db = test_db("interactive-executing").await;
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
    assert_lean_transition_is_legal("Request", "claimed", "processing");
    assert_lean_transition_is_legal("Request", "processing", "processing");

    assert_eq!(
        fetch_request_snapshot(&actors, &doc_id).await,
        RequestSnapshot {
            lifecycle_state: RequestLifecycleState::Processing,
            agent_did: actors.agent_did().into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: gents::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_session_observes_request_with_authoritative_state(
        &actors,
        &session_id,
        &(request_id),
        RequestLifecycleState::Claimed,
        RequestLifecycleState::Processing,
    )
    .await;
}

#[tokio::test]
async fn interactive_fail_before_stream_snapshot_matches_failed_released() {
    let db = test_db("interactive-fail").await;
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
            RequestTerminalOutcome::Failed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    assert_lean_transition_is_legal("Request", "claimed", "failed");

    assert_eq!(
        fetch_request_snapshot(&actors, &doc_id).await,
        RequestSnapshot {
            lifecycle_state: RequestLifecycleState::Failed,
            agent_did: actors.agent_did().into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: gents::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_session_observes_request(
        &actors,
        &session_id,
        &(request_id),
        RequestLifecycleState::Failed,
    )
    .await;
}

#[tokio::test]
async fn scheduled_materialization_snapshot_matches_claimed_waiting() {
    let db = support::test_db("scheduled-materialize").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let agent = &fixture.agent;
    let access = gents::ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        agent.did(),
        db.node_identity.clone(),
        agent.clone(),
        "scheduled prompt body",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        TriggerLineage::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        fetch_request_snapshot_with_access(&access, &lifecycle.request().doc_id).await,
        RequestSnapshot {
            lifecycle_state: RequestLifecycleState::Claimed,
            agent_did: agent.did().into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "scheduled".into(),
            retry_parent_request: "".into(),
            retry_root_request: lifecycle.request().request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: gents::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );

    assert_session_observes_request_with_access(
        &access,
        &lifecycle.request().session_id,
        &(lifecycle.request().request_id.clone()),
        RequestLifecycleState::Claimed,
        RequestLifecycleState::Claimed,
        db.node_identity.did(),
        agent.did(),
    )
    .await;
}

#[tokio::test]
async fn scheduled_materialization_persists_trigger_lineage() {
    let db = support::test_db("scheduled-materialize-lineage").await;
    let fixture = support::managed_agent_fixture(&db).await;
    let agent = &fixture.agent;
    let access = gents::ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let lineage = TriggerLineage {
        trigger_id: Some("sched-1".into()),
        trigger_kind: Some("schedule".into()),
        source_doc_id: None,
        correlation: None,
        trigger_context: None,
    };

    let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        agent.did(),
        db.node_identity.clone(),
        agent.clone(),
        "scheduled prompt body with lineage",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    assert_eq!(
        fetch_request_lineage_with_access(
            &access,
            &format!(
                "_docID: {{_eq: \"{}\"}}",
                escape_graphql_string(&lifecycle.request().doc_id)
            )
        )
        .await,
        RequestLineageSnapshot {
            caused_by_trigger_id: Some("sched-1".into()),
            caused_by_trigger_kind: Some("schedule".into()),
        }
    );

    assert_eq!(
        Some(fetch_request_lineage_with_access(&access,
            "caused_by_trigger_id: {_eq: \"sched-1\"}, caused_by_trigger_kind: {_eq: \"schedule\"}"
        ).await),
        Some(RequestLineageSnapshot {
            caused_by_trigger_id: Some("sched-1".into()),
            caused_by_trigger_kind: Some("schedule".into()),
        })
    );

    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ admission_kind admission_signer_did admission_signature }} }}"#,
            escape_graphql_string(&lifecycle.request().doc_id),
        ))
        .await;
    let response = response.unwrap();
    let row = &response["data"]["AgentRequest"][0];
    assert_eq!(row["admission_kind"], "local-self");
    assert_eq!(row["admission_signer_did"], agent.did());
    assert!(row["admission_signature"]
        .as_str()
        .is_some_and(|signature| !signature.is_empty()));
}

pub(super) async fn generated_queue_deadline_cases_pin_r4a_contract_rows() {
    let cases = lean_queue_deadline_cases();
    assert!(!cases.is_empty());

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
        "background_completion_notification_creates_no_agent_request" => {}
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
    lifecycle_state: Option<RequestLifecycleState>,
    input: Option<serde_json::Value>,
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
    lifecycle_state: Option<RequestLifecycleState>,
    deadline: String,
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
    row.lifecycle_state == Some(RequestLifecycleState::Pending)
}

fn row_is_active(row: &QueueRuntimeRow) -> bool {
    matches!(
        row.lifecycle_state,
        Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
    )
}

fn row_is_terminal(row: &QueueRuntimeRow) -> bool {
    row.lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
}

fn row_matches_coalesced_key(row: &QueueRuntimeRow, queue_key: Option<&str>) -> bool {
    let Some(queue_key) = queue_key else {
        return false;
    };
    if !row_is_pending(row) {
        return false;
    }
    let Some(input) = row.input.as_ref() else {
        return false;
    };
    let Some(queue) = input.get("queue") else {
        return false;
    };
    queue.get("source").and_then(serde_json::Value::as_str) == Some("background_completion")
        && queue.get("policy").and_then(serde_json::Value::as_str) == Some("coalesce")
        && queue.get("key").and_then(serde_json::Value::as_str) == Some(queue_key)
}

async fn fetch_queue_runtime_snapshot(
    actors: &support::agent_requests::RequestActors,
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
                lifecycle_state
                input
            }}
        }}"#
    );
    let response = actors
        .agent_access
        .execute(&query)
        .await
        .expect("queue snapshot query");
    let rows: Vec<QueueRuntimeRow> = response
        .get("data")
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
    actors: &support::agent_requests::RequestActors,
    doc_id: String,
    request_id: usize,
    session_id: &str,
    created_at: &str,
    deadline: Option<String>,
) -> gents::AgentRequest {
    let mut request = actors.build_request(
        doc_id,
        request_id.to_string(),
        session_id.to_string(),
        created_at.to_string(),
    );
    request.deadline = deadline;
    request
}

fn lifecycle_for(
    actors: &support::agent_requests::RequestActors,
    request: gents::AgentRequest,
    deadline_duration_secs: u64,
) -> RequestLifecycle {
    actors.lifecycle(
        request,
        deadline_duration_secs,
        ExecutionOrigin::Interactive,
        "",
    )
}

async fn drive_active_request_blocks_later_same_session_claim(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-active-blocks").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let session_id = case.session_id.to_string();
    let active_id = case.pre_active_request_id.expect("active request id");
    let pending_id = case
        .pre_pending_request_ids
        .first()
        .copied()
        .expect("pending request id");
    let active_created_at = "2026-03-23T00:00:10Z";
    let pending_created_at = "2026-03-23T00:00:20Z";

    let active_doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(
                &active_id.to_string(),
                &session_id,
                active_created_at,
            )
            .state("pending"),
        )
        .await;
    let pending_doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(
                &pending_id.to_string(),
                &session_id,
                pending_created_at,
            )
            .state("pending"),
        )
        .await;

    let active_request = request_from_parts(
        &actors,
        active_doc_id,
        active_id,
        &session_id,
        active_created_at,
        None,
    );
    let mut active_lifecycle = lifecycle_for(&actors, active_request, DEADLINE_SECS);
    assert_eq!(
        active_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(&actors, &session_id, None, &generated_ids).await;
    assert_pre_queue_snapshot(case, &pre);

    let pending_request = request_from_parts(
        &actors,
        pending_doc_id,
        pending_id,
        &session_id,
        pending_created_at,
        None,
    );
    let mut pending_lifecycle = lifecycle_for(&actors, pending_request, DEADLINE_SECS);
    assert_eq!(
        pending_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Queued
    );
    assert_eq!(case.claimed_request_id, None);

    let post = fetch_queue_runtime_snapshot(&actors, &session_id, None, &generated_ids).await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_terminal_active_allows_next_pending_same_session_claim(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-terminal-allows").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let session_id = case.session_id.to_string();
    let active_id = case.pre_active_request_id.expect("active request id");
    let pending_id = case.claimed_request_id.expect("claimed request id");
    let active_created_at = "2026-03-23T00:00:10Z";
    let pending_created_at = "2026-03-23T00:00:20Z";

    let active_doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(
                &active_id.to_string(),
                &session_id,
                active_created_at,
            )
            .state("pending"),
        )
        .await;
    let pending_doc_id = actors
        .create_request(
            support::agent_requests::RequestSeed::new(
                &pending_id.to_string(),
                &session_id,
                pending_created_at,
            )
            .state("pending"),
        )
        .await;

    let active_request = request_from_parts(
        &actors,
        active_doc_id,
        active_id,
        &session_id,
        active_created_at,
        None,
    );
    let mut active_lifecycle = lifecycle_for(&actors, active_request, DEADLINE_SECS);
    assert_eq!(
        active_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(&actors, &session_id, None, &generated_ids).await;
    assert_pre_queue_snapshot(case, &pre);

    crate::support::begin_owned_execution(&mut active_lifecycle, &db.node)
        .await
        .unwrap();
    active_lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let pending_request = request_from_parts(
        &actors,
        pending_doc_id,
        pending_id,
        &session_id,
        pending_created_at,
        None,
    );
    let mut pending_lifecycle = lifecycle_for(&actors, pending_request, DEADLINE_SECS);
    assert_eq!(
        pending_lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed
    );

    let post = fetch_queue_runtime_snapshot(&actors, &session_id, None, &generated_ids).await;
    assert_post_queue_snapshot(case, &post);
}

async fn drive_cancel_drains_automated_wakeups_preserves_user_pending(
    case: &lean_vocab_test::LeanQueueDeadlineConformanceCase,
) {
    let db = test_db("queue-deadline-cancel-drain").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let session_id = case.session_id.to_string();
    let parent_request_id = "queue-deadline-cancel-parent";
    create_queue_request(
        &actors,
        parent_request_id,
        &session_id,
        "completed",
        "2026-03-23T00:00:00Z",
        "interactive",
        None,
        None,
        None,
    )
    .await;

    let automated_id = case.automated_drained_request_ids[0];
    let user_id = case.preserved_user_pending_request_ids[0];
    create_queue_request(
        &actors,
        &automated_id.to_string(),
        &session_id,
        "pending",
        "2026-03-23T00:00:10Z",
        "scheduled",
        Some(&automated_queue_input(
            case.queue_key.as_deref().expect("queue key"),
            parent_request_id,
        )),
        None,
        None,
    )
    .await;
    create_queue_request(
        &actors,
        &user_id.to_string(),
        &session_id,
        "pending",
        "2026-03-23T00:00:20Z",
        "scheduled",
        Some(&user_queue_input()),
        None,
        None,
    )
    .await;

    let generated_ids = std::collections::BTreeMap::new();
    let pre = fetch_queue_runtime_snapshot(
        &actors,
        &session_id,
        case.queue_key.as_deref(),
        &generated_ids,
    )
    .await;
    assert_pre_queue_snapshot(case, &pre);

    gents::interrupt_request(&db.node, actors.agent().as_ref(), parent_request_id)
        .await
        .unwrap();

    let post = fetch_queue_runtime_snapshot(
        &actors,
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
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let session_id = case.session_id.to_string();
    let request_id = case.claimed_request_id.expect("claimed request id");
    let created_at = support::agent_requests::canonical_now();
    let explicit_deadline_at =
        chrono::Utc::now() + chrono::Duration::seconds(case.pre_request_deadline.unwrap() as i64);
    let explicit_deadline = explicit_deadline_at.to_rfc3339();
    let doc_id = create_queue_request(
        &actors,
        &request_id.to_string(),
        &session_id,
        "pending",
        &created_at,
        "interactive",
        None,
        Some(&explicit_deadline),
        None,
    )
    .await;
    let request = request_from_parts(
        &actors,
        doc_id,
        request_id,
        &session_id,
        &created_at,
        Some(explicit_deadline.clone()),
    );

    let before_claim = chrono::Utc::now();
    let mut lifecycle = lifecycle_for(
        &actors,
        request,
        case.synthesized_claim_deadline.unwrap() as u64,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let row = fetch_deadline_runtime_row(&actors, request_id).await;
    assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Claimed));

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
    actors: &support::agent_requests::RequestActors,
    request_id: &str,
    session_id: &str,
    status: &str,
    created_at: &str,
    execution_origin: &str,
    input: Option<&str>,
    deadline: Option<&str>,
    node_did: Option<&str>,
) -> String {
    assert!(
        node_did.is_none(),
        "queue fixtures are owned by the fixture runtime"
    );
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
    let doc_id = actors
        .create_request(support::agent_requests::RequestSeed {
            input,
            content: Some("queue deadline conformance"),
            execution_origin: Some(execution_origin),
            ..support::agent_requests::RequestSeed::new(request_id, session_id, created_at)
                .state(lifecycle_state)
        })
        .await;
    if let Some(deadline) = deadline {
        actors
            .update_request(&doc_id, json!({ "deadline": deadline }))
            .await;
    }
    doc_id
}

fn automated_queue_input(queue_key: &str, queued_after_request_id: &str) -> String {
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

fn user_queue_input() -> String {
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

async fn fetch_deadline_runtime_row(
    actors: &support::agent_requests::RequestActors,
    request_id: usize,
) -> DeadlineRuntimeRow {
    let escaped_request_id = escape_graphql_string(&request_id.to_string());
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ lifecycle_state deadline }}
        }}"#
    );
    let response = actors
        .agent_access
        .execute(&query)
        .await
        .expect("deadline row");
    serde_json::from_value(response["data"]["AgentRequest"][0].clone()).expect("deadline row")
}

#[tokio::test]
async fn terminal_repair_sweep_ignores_foreign_did_claims() {
    let db = test_db("terminal-repair-scope-foreign").await;
    let actors = support::agent_requests::RequestActors::new(&db).await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = support::agent_requests::canonical_now();
    let own_doc = actors
        .create_request(support::agent_requests::RequestSeed::new(
            &request_id,
            &session_id,
            &created_at,
        ))
        .await;
    let request = actors.build_request(own_doc.clone(), request_id, session_id, created_at.clone());
    let mut lifecycle = actors.lifecycle(request, DEADLINE_SECS, ExecutionOrigin::Interactive, "");
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    // A replica owned by another node's runtime, visible to the fixture Agent:
    // the runtime root writes it as it would any replicated row, and the sweep
    // must still ignore it because the node scope is foreign.
    let foreign = tokio::task::spawn_blocking(|| support::fixtures::test_identity("foreign-owner"))
        .await
        .unwrap();
    let foreign_request_id = uuid::Uuid::new_v4().to_string();
    let value = json!({
        "request_id": foreign_request_id,
        "purpose": "normal",
        "node_did": gents::NodeIdentity::did(&foreign),
        "agent_did": actors.agent_did(),
        "session_id": uuid::Uuid::new_v4().to_string(),
        "retry_root_request": foreign_request_id,
        "content": "foreign replica",
        "lifecycle_state": "pending",
        "execution_origin": "interactive",
        "created_at": created_at,
        "retry_count": 0,
        "max_retries": 3,
    });
    let agent = ::identity::Did::new(actors.agent_did()).unwrap();
    let foreign_doc = gents::ConfigAccess::local_as(
        db.node.clone(),
        ::identity::Did::new(db.node_identity.did()).unwrap(),
    )
    .transact_write("test.foreign_claim_replica", |txn| {
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
    .unwrap();
    actors
        .set_lifecycle_state(&foreign_doc, RequestLifecycleState::Claimed)
        .await;
    let expired = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    for doc_id in [&own_doc, &foreign_doc] {
        actors
            .update_request(doc_id, json!({ "execution_lease_expires_at": expired }))
            .await;
    }
    let foreign_before = fetch_request_snapshot(&actors, &foreign_doc).await;
    let report = RequestLifecycle::repair_terminal_requests_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(actors.agent()),
    )
    .await
    .unwrap();
    assert_eq!(report.repaired, 1);
    assert_eq!(
        fetch_request_snapshot(&actors, &own_doc)
            .await
            .lifecycle_state,
        RequestLifecycleState::Failed,
    );
    assert_eq!(
        fetch_request_snapshot(&actors, &foreign_doc).await,
        foreign_before
    );
}

async fn fetch_request_snapshot_with_access(
    access: &gents::ConfigAccess,
    doc_id: &str,
) -> RequestSnapshot {
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, limit: 1) {{
            request_id lifecycle_state agent_did backend_id execution_origin
            retry_parent_request retry_root_request superseded_by_request retry_count max_retries
            claimed_at deadline failure_reason
        }} }}"#,
            escape_graphql_string(doc_id),
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    row.into()
}

async fn fetch_request_lineage_with_access(
    access: &gents::ConfigAccess,
    filter: &str,
) -> RequestLineageSnapshot {
    let response = access.execute(&format!(
        "{{ AgentRequest(filter: {{{filter}}}, limit: 1) {{ caused_by_trigger_id caused_by_trigger_kind }} }}"
    )).await.unwrap();
    serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap()
}

async fn assert_session_observes_request_with_access(
    access: &gents::ConfigAccess,
    session_id: &str,
    request_id: &str,
    expected_observation: RequestLifecycleState,
    expected_authoritative: RequestLifecycleState,
    node_did: &str,
    agent_did: &str,
) {
    let response = access
        .execute(&format!(
            r#"{{ AgentSession(filter: {{session_id: {{_eq: "{}"}}}}, limit: 2) {{ {} }} }}"#,
            escape_graphql_string(session_id),
            gents::session::AGENT_SESSION_FIELDS,
        ))
        .await
        .unwrap();
    let rows = response["data"]["AgentSession"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "exact session must be unambiguous");
    let session = gents::session::decode_session_row(&rows[0])
        .unwrap()
        .session;
    assert_eq!(session.session_id, session_id);
    assert_eq!(session.node_did, node_did);
    assert_eq!(session.agent_did, agent_did);
    assert!(
        session.closed_at.is_none(),
        "request terminality does not close the session"
    );
    let latest = session
        .observation
        .expect("index observation")
        .latest_request
        .expect("latest request");
    assert_eq!(latest.request_id, request_id);
    assert_eq!(latest.lifecycle_state, expected_observation);
    let response = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{ _docID node_did requester_did agent_did lifecycle_state }} }}"#,
        escape_graphql_string(request_id), escape_graphql_string(session_id),
    )).await.unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "exact request must be unambiguous");
    assert_eq!(
        rows[0]["_docID"].as_str(),
        Some(latest.request_doc_id.as_str())
    );
    assert_eq!(
        rows[0]["lifecycle_state"].as_str(),
        Some(expected_authoritative.as_str())
    );
    assert_eq!(
        rows[0]["node_did"].as_str(),
        Some(session.node_did.as_str())
    );
    assert_eq!(
        rows[0]["requester_did"].as_str(),
        session.requester_did.as_deref()
    );
    assert_eq!(
        rows[0]["agent_did"].as_str(),
        Some(session.agent_did.as_str())
    );
}

async fn assert_session_observes_request(
    actors: &support::agent_requests::RequestActors,
    session_id: &str,
    request_id: &str,
    expected: RequestLifecycleState,
) {
    assert_session_observes_request_with_authoritative_state(
        actors, session_id, request_id, expected, expected,
    )
    .await;
}

async fn assert_session_observes_request_with_authoritative_state(
    actors: &support::agent_requests::RequestActors,
    session_id: &str,
    request_id: &str,
    expected_observation: RequestLifecycleState,
    expected_authoritative: RequestLifecycleState,
) {
    assert_session_observes_request_with_access(
        &actors.agent_access,
        session_id,
        request_id,
        expected_observation,
        expected_authoritative,
        actors.node_did(),
        actors.agent_did(),
    )
    .await;
}

async fn activate_workspace_fixture(
    actors: &support::agent_requests::RequestActors,
    doc_id: &str,
) -> anyhow::Result<()> {
    gents::__test_internals::activate_workspace_bound_request_with_identity(
        &actors.node,
        doc_id,
        actors.agent_did().parse()?,
        Some(actors.agent().as_ref()),
    )
    .await
}
