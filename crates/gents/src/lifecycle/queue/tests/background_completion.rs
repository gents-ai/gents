use super::*;
use crate::lifecycle::RequestTerminalOutcome;
use crate::session;

/// A desktop-owned session: an enrolled desktop request, finished through the
/// real lifecycle, whose session is owned by the desktop's requester DID.
async fn desktop_owned_session(
    db: &TestDb,
    session_id: &str,
) -> (
    crate::identity::KeyIdentity,
    gents_protocol::request_admission::AgentRequestCreate,
    AgentRequest,
    crate::streaming::DefraStreamWriter,
) {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
    let desktop =
        crate::identity::KeyIdentity::load_or_create(db._tempdir.path().join("desktop.key"), None)
            .unwrap();
    let mut create = AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "desktop-parent",
        db.agent_did(),
        desktop.did(),
        TEST_BEHAVIOR_ID,
        session_id,
        "run work",
        "interactive",
        "2030-01-01T00:00:00Z",
        AgentRequestAdmissionRecord::enrollment(
            desktop.did(),
            "enrollment",
            "digest",
            db.agent_did(),
            1,
            "2099-01-01T00:00:00Z",
        ),
    );
    crate::sign_agent_request_create(&desktop, &mut create)
        .await
        .unwrap();
    let response = db.node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let parent = crate::request_binding::load_agent_request(&db.node, "desktop-parent")
        .await
        .unwrap()
        .unwrap();
    // Materialize and finish the enrolled desktop turn through the real
    // lifecycle, before its longer-lived background work completes.
    let writer = crate::streaming::DefraStreamWriter::new(
        db.node.clone(),
        db.agent_did(),
        std::time::Duration::ZERO,
    );
    let mut parent_lifecycle = crate::RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        parent.clone(),
        60,
        ExecutionOrigin::Interactive,
        "backend-test",
    );
    assert_eq!(
        parent_lifecycle.claim_with_identity().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    parent_lifecycle
        .begin_owned_execution(&writer)
        .await
        .unwrap();
    parent_lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    session::ensure_session_with_behavior_id_and_requester_did(
        &db.node,
        session_id,
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        TEST_BEHAVIOR_ID,
        Some(desktop.did()),
    )
    .await
    .unwrap();
    (desktop, create, parent, writer)
}

/// Lean `Enrollment.runtime_internal_adopts_session_scope`: runtime-signed
/// controls of an enrolled desktop parent are written under the desktop's
/// requester and share its existing session, while a parent from another
/// session cannot lend its scope.
#[tokio::test]
async fn desktop_session_runtime_controls_adopt_owner_and_reject_foreign_ancestry() {
    for source in [
        QueueSource::BackgroundCompletion,
        QueueSource::Steering,
        QueueSource::Goal,
    ] {
        let db = test_db("desktop-control-scope").await;
        let session_id = "desktop-owned-session";
        let (desktop, create, parent, writer) = desktop_owned_session(&db, session_id).await;
        let query = format!("{{ AgentSession {{ {} }} }}", session::AGENT_SESSION_FIELDS);
        let before = db.node.execute(&query).await.data.unwrap();
        let mutation = if source == QueueSource::Goal {
            let mut continuation = prepare_goal_continuation(
                &parent,
                TEST_BEHAVIOR_ID.into(),
                "goal",
                "continue",
                1,
                false,
                "2030-01-01T00:00:01Z",
                parent.subagent_depth,
            )
            .unwrap();
            crate::sign_agent_request_create(db.identity.as_ref(), &mut continuation)
                .await
                .unwrap();
            continuation.graphql_mutation().unwrap()
        } else {
            session_request_create_mutation(
                &parent,
                TEST_BEHAVIOR_ID,
                "continue",
                ExecutionOrigin::Scheduled,
                RequestInput {
                    queue: Some(hints(source, QueuePolicy::Append)),
                    ..Default::default()
                },
                "runtime-control",
                "2030-01-01T00:00:01Z",
                None,
            )
            .await
            .unwrap()
        };
        let response = db.node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let request_id = if source == QueueSource::Goal {
            goal_continuation_identity("goal", &parent.request_id, 1)
                .unwrap()
                .request_id
        } else {
            "runtime-control".into()
        };
        let request = crate::request_binding::load_agent_request(&db.node, &request_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.requester_did.as_deref(), Some(desktop.did()));
        let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
            db.node.clone(),
            TEST_BEHAVIOR_ID,
            db.agent_did(),
            request.clone(),
            60,
            ExecutionOrigin::Scheduled,
            "backend-test",
        );
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        let sessions = db.node.execute(&query).await.data.unwrap();
        assert_eq!(sessions["AgentSession"].as_array().unwrap().len(), 1);
        assert_eq!(
            sessions["AgentSession"][0]["_docID"], before["AgentSession"][0]["_docID"],
            "control must reuse the desktop-owned session"
        );
        assert_eq!(sessions["AgentSession"][0]["requester_did"], desktop.did());
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();

        let next = session_request_create_mutation(
            &request,
            TEST_BEHAVIOR_ID,
            "second-hop",
            ExecutionOrigin::Scheduled,
            wake_queue_input(hints(QueueSource::Steering, QueuePolicy::Append)),
            "second-control",
            "2030-01-01T00:00:02Z",
            None,
        )
        .await
        .unwrap();
        let response = db.node.execute(&next).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let second = crate::request_binding::load_agent_request(&db.node, "second-control")
            .await
            .unwrap()
            .unwrap();
        let mut second_lifecycle = crate::RequestLifecycle::new_with_execution_binding(
            db.node.clone(),
            TEST_BEHAVIOR_ID,
            db.agent_did(),
            second,
            60,
            ExecutionOrigin::Scheduled,
            "backend-test",
        );
        assert_eq!(
            second_lifecycle.claim_with_identity().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        assert_eq!(
            db.node.execute(&query).await.data.unwrap()["AgentSession"][0]["_docID"],
            before["AgentSession"][0]["_docID"]
        );
        second_lifecycle
            .begin_owned_execution(&writer)
            .await
            .unwrap();
        second_lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();

        let forged = session_request_create_mutation(
            &request,
            TEST_BEHAVIOR_ID,
            "signed-original",
            ExecutionOrigin::Scheduled,
            wake_queue_input(hints(QueueSource::Steering, QueuePolicy::Append)),
            "forged-control",
            "2030-01-01T00:00:03Z",
            None,
        )
        .await
        .unwrap()
        .replace("signed-original", "unsigned-tampering");
        let response = db.node.execute(&forged).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let forged = crate::request_binding::load_agent_request(&db.node, "forged-control")
            .await
            .unwrap()
            .unwrap();
        let error = admission_verifier(&db)
            .verify_fresh(&forged, TEST_BEHAVIOR_ID)
            .await
            .unwrap_err();
        assert!(error.is_denied(), "{error:#}");
        let mut forged_lifecycle = crate::RequestLifecycle::new_with_execution_binding(
            db.node.clone(),
            TEST_BEHAVIOR_ID,
            db.agent_did(),
            forged,
            60,
            ExecutionOrigin::Scheduled,
            "backend-test",
        );
        forged_lifecycle
            .reject_admission(&error.to_string())
            .await
            .unwrap();
        let failed = db.node.execute("{ AgentRequest(filter: { request_id: { _eq: \"forged-control\" } }) { lifecycle_state failure_reason } }").await;
        assert!(!failed.has_errors(), "{:?}", failed.errors);
        let failed = failed.data.unwrap();
        assert_eq!(failed["AgentRequest"][0]["lifecycle_state"], "failed");
        assert_eq!(
            failed["AgentRequest"][0]["failure_reason"],
            error.to_string()
        );

        // The next enrolled user request can use the same canonical session.
        let mut followup = create.clone();
        followup.initial_lifecycle_state = RequestLifecycleState::Pending;
        followup.request_id = "desktop-followup".into();
        followup.retry_root_request = Some(followup.request_id.clone());
        followup.created_at = "2030-01-01T00:00:04Z".into();
        followup.content = "is it finished?".into();
        crate::sign_agent_request_create(&desktop, &mut followup)
            .await
            .unwrap();
        let response = db.node.execute(&followup.graphql_mutation().unwrap()).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let followup = crate::request_binding::load_agent_request(&db.node, "desktop-followup")
            .await
            .unwrap()
            .unwrap();
        let mut followup_lifecycle = crate::RequestLifecycle::new_with_execution_binding(
            db.node.clone(),
            TEST_BEHAVIOR_ID,
            db.agent_did(),
            followup,
            60,
            ExecutionOrigin::Interactive,
            "backend-test",
        );
        assert_eq!(
            followup_lifecycle.claim_with_identity().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        let after = db.node.execute(&query).await.data.unwrap();
        assert_eq!(after["AgentSession"].as_array().unwrap().len(), 1);
        assert_eq!(
            after["AgentSession"][0]["_docID"],
            before["AgentSession"][0]["_docID"]
        );
        assert_eq!(after["AgentSession"][0]["requester_did"], desktop.did());

        // Valid runtime signature but a physical parent from another session.
        let mut foreign_parent = parent.clone();
        foreign_parent.session_id = "other-session".into();
        let bad = session_request_create_mutation(
            &foreign_parent,
            TEST_BEHAVIOR_ID,
            "bad",
            ExecutionOrigin::Scheduled,
            wake_queue_input(hints(QueueSource::Steering, QueuePolicy::Append)),
            "foreign-control",
            "2030-01-01T00:00:02Z",
            None,
        )
        .await
        .unwrap();
        let response = db.node.execute(&bad).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        // An existing other-session owner must not authorize the mismatched parent.
        session::ensure_session_with_behavior_id_and_requester_did(
            &db.node,
            "other-session",
            TEST_BEHAVIOR_ID,
            db.agent_did(),
            TEST_BEHAVIOR_ID,
            Some(desktop.did()),
        )
        .await
        .unwrap();
        let bad = crate::request_binding::load_agent_request(&db.node, "foreign-control")
            .await
            .unwrap()
            .unwrap();
        let error = admission_verifier(&db)
            .verify_fresh(&bad, TEST_BEHAVIOR_ID)
            .await
            .unwrap_err();
        assert!(error.is_denied(), "{error:#}");
        assert!(
            error.to_string().contains("outside its session"),
            "{error:#}"
        );
    }
}

fn admission_verifier(db: &TestDb) -> crate::request_admission::AgentRequestAdmissionVerifier {
    let (_owner, authority) = crate::agent::p2p_reconcile::enrollment_authority_channel();
    crate::request_admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.identity.clone(),
        authority,
    )
}

/// #2064: a node-owned callee's result, returned (Lean
/// `CausalHop.return_keeps_caller_hop`) into a session a paired desktop
/// started, is written under the desktop's requester, admitted, claimed and
/// resumed in that same session.
#[tokio::test]
async fn a_returned_result_is_admitted_into_a_paired_client_session() {
    let db = test_db("paired-client-completion").await;
    let session_id = "desktop-owned-session";
    let (desktop, _, parent, _) = desktop_owned_session(&db, session_id).await;
    let wake = persist_background_completion_with_message_waking(
        &db.node,
        &parent,
        "gatekeeper answer",
        "background-completion-notification:agent-new:tool",
        "review notifications",
        background_hints(&parent),
        None,
        crate::lifecycle::RequestHopCause::Return,
    )
    .await
    .unwrap()
    .request
    .expect("completion wake");
    let wake = crate::request_admission::load_request_for_admission_test(&db.node, &wake.doc_id)
        .await
        .unwrap();
    assert_eq!(wake.requester_did.as_deref(), Some(desktop.did()));
    assert_eq!(wake.subagent_depth, parent.subagent_depth);
    let verified = admission_verifier(&db)
        .verify_fresh(&wake, TEST_BEHAVIOR_ID)
        .await
        .unwrap();
    let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        verified.clone(),
        60,
        ExecutionOrigin::Scheduled,
        "backend-test",
    );
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    crate::hook::DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        session_id,
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        verified.requester_did.as_deref(),
        crate::hook::FailurePolicy::FailClosed,
    )
    .await
    .expect("the completion resumes the desktop-owned session");
    let sessions = db
        .node
        .execute(&format!(
            "{{ AgentSession {{ {} }} }}",
            session::AGENT_SESSION_FIELDS
        ))
        .await
        .data
        .unwrap();
    assert_eq!(sessions["AgentSession"].as_array().unwrap().len(), 1);
    assert_eq!(sessions["AgentSession"][0]["requester_did"], desktop.did());
}

fn root_parent(agent_did: &str, session_id: &str) -> AgentRequest {
    let mut parent = parent_request(agent_did, session_id);
    parent.subagent_depth = 0;
    parent.caused_by_parent_request_id = None;
    parent.caused_by_parent_request_doc_id = None;
    parent.caused_by_parent_tool_call_id = None;
    parent.caused_by_parent_tool_call_doc_id = None;
    parent
}

fn background_hints(parent: &AgentRequest) -> RequestQueue {
    RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{}", parent.session_id)),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

fn wake_agent_request(
    parent: &AgentRequest,
    doc_id: &str,
    request_id: &str,
    hints: &RequestQueue,
) -> AgentRequest {
    AgentRequest {
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: doc_id.to_string(),
        request_id: request_id.to_string(),
        agent_did: parent.agent_did.clone(),
        // Background wakes are signed local-control requests, so their exact
        // requester scope is the signing principal even when the parent was
        // an unscoped interactive request.
        requester_did: Some(parent.agent_did.clone()),
        behavior_id: parent.behavior_id.clone(),
        session_id: parent.session_id.clone(),
        content: "review notifications".to_string(),
        max_total_tokens: None,
        input: RequestInput {
            queue: Some(background_wake_queue(
                hints,
                hints.queued_after_request_id.clone(),
            )),
            ..Default::default()
        },
        execution_origin: Some("scheduled".to_string()),
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        subagent_depth: 0,
        caused_by_parent_request_id: Some(parent.request_id.clone()),
        caused_by_parent_request_doc_id: Some(parent.doc_id.clone()),
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        workspace_id: None,
        workspace_owner_agent_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    }
}

#[tokio::test]
async fn notification_is_atomically_bound_to_coalesced_wake() {
    let db = test_db("atomic-background-notification").await;
    let parent = root_parent(db.agent_did(), "atomic-background-session");
    let first = persist_background_completion_with_message(
        &db.node,
        &parent,
        "first notification",
        "background-completion-notification:first:tool",
        "review notifications",
        background_hints(&parent),
        None,
    )
    .await
    .unwrap();
    let second = persist_background_completion_with_message(
        &db.node,
        &parent,
        "second notification",
        "background-completion-notification:second:tool",
        "review notifications",
        background_hints(&parent),
        None,
    )
    .await
    .unwrap();
    assert!(first.created_request);
    assert!(!second.created_request);
    assert_eq!(
        first.request.as_ref().expect("non-Goal wake").doc_id,
        second.request.as_ref().expect("non-Goal wake").doc_id
    );

    let wake_query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
            }}
        }}"#,
        escape_graphql_string(&first.request.as_ref().expect("non-Goal wake").doc_id)
    );
    let wake_response = db.node.execute(&wake_query).await;
    assert!(
        !wake_response.has_errors(),
        "wake query: {:?}",
        wake_response.errors
    );
    let wake = &wake_response.data.as_ref().unwrap()["AgentRequest"][0];
    assert_eq!(
        wake["caused_by_parent_request_id"].as_str(),
        Some(parent.request_id.as_str())
    );
    assert_eq!(
        wake["caused_by_parent_request_doc_id"].as_str(),
        Some(parent.doc_id.as_str())
    );

    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ sequence: ASC }}
            ) {{ request_doc_id }}
        }}"#,
        escape_graphql_string(&parent.session_id)
    );
    let response = db.node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "message query: {:?}",
        response.errors
    );
    let rows = response.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(
            row["request_doc_id"].as_str(),
            Some(
                first
                    .request
                    .as_ref()
                    .expect("non-Goal wake")
                    .doc_id
                    .as_str()
            )
        );
    }
}
#[tokio::test]
async fn duplicate_notification_key_recovers_its_original_wake_binding() {
    let db = test_db("atomic-background-idempotent-notification").await;
    let parent = root_parent(
        db.agent_did(),
        "atomic-background-idempotent-notification-session",
    );
    let message_key = "background-completion-notification:idempotent:tool";
    let first = persist_background_completion_with_message(
        &db.node,
        &parent,
        "idempotent notification",
        message_key,
        "review notifications",
        background_hints(&parent),
        None,
    )
    .await
    .unwrap();
    let retry = persist_background_completion_with_message(
        &db.node,
        &parent,
        "idempotent notification",
        message_key,
        "review notifications",
        background_hints(&parent),
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        retry.request.as_ref().expect("non-Goal wake").doc_id,
        first.request.as_ref().expect("non-Goal wake").doc_id
    );
    assert_eq!(
        retry.request.as_ref().expect("non-Goal wake").request_id,
        first.request.as_ref().expect("non-Goal wake").request_id
    );
    assert_eq!(
        retry.request.as_ref().expect("non-Goal wake").session_id,
        first.request.as_ref().expect("non-Goal wake").session_id
    );
    assert_eq!(retry.message_sequence, first.message_sequence);
    assert!(!retry.created_request);

    let conflict = persist_background_completion_with_message(
        &db.node,
        &parent,
        "changed notification",
        message_key,
        "review notifications",
        background_hints(&parent),
        None,
    )
    .await
    .unwrap_err();
    assert!(
        conflict
            .to_string()
            .contains("replay conflicts with authority, scope, or content"),
        "unexpected conflict: {conflict:#}"
    );
}

#[tokio::test]
async fn many_concurrent_notifications_publish_one_wake_identity() {
    let db = test_db("atomic-background-many-race").await;
    let parent = root_parent(db.agent_did(), "atomic-background-many-race-session");
    let enqueued = futures::future::join_all((0..16).map(|index| {
        let node = db.node.clone();
        let parent = parent.clone();
        async move {
            persist_background_completion_with_message(
                node.as_ref(),
                &parent,
                &format!("concurrent notification {index}"),
                &format!("background-completion-notification:many-race-{index}:tool"),
                "review notifications",
                background_hints(&parent),
                None,
            )
            .await
            .unwrap()
        }
    }))
    .await;

    let wake_doc_id = &enqueued[0].request.as_ref().expect("non-Goal wake").doc_id;
    assert!(enqueued
        .iter()
        .all(|result| result.request.as_ref().expect("non-Goal wake").doc_id == *wake_doc_id));
    assert_eq!(
        enqueued
            .iter()
            .filter(|result| result.created_request)
            .count(),
        1
    );
}

#[tokio::test]
async fn restart_before_claim_preserves_pending_input_until_the_wake_completes() {
    let db = test_db("background-restart-before-claim").await;
    let node = db.node.clone();
    let parent = root_parent(db.agent_did(), "background-restart-before-claim-session");
    let hints = background_hints(&parent);
    let enqueued = persist_background_completion_with_message(
        node.as_ref(),
        &parent,
        "restart-safe notification",
        "background-completion-notification:restart-before-claim:tool",
        "review notifications",
        hints.clone(),
        None,
    )
    .await
    .unwrap();

    let recovery = crate::RequestLifecycle::recover_all(node.as_ref(), db.agent_did())
        .await
        .unwrap();
    assert_eq!(recovery.background_wakes_redriven, 0);
    let before = crate::load_background_completion_diagnostics(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        db.agent_did(),
    )
    .await
    .unwrap();
    assert_eq!(before.pending_notifications, 1);
    assert_eq!(before.acknowledged_notifications, 0);
    assert_eq!(before.epochs[0].state, "pending");

    let request = wake_agent_request(
        &parent,
        &enqueued.request.as_ref().expect("non-Goal wake").doc_id,
        &enqueued.request.as_ref().expect("non-Goal wake").request_id,
        &hints,
    );
    let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        request,
        60,
        ExecutionOrigin::Scheduled,
        "backend-test",
    );
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        db.agent_did(),
        std::time::Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let after = crate::load_background_completion_diagnostics(
        &crate::config_client::ConfigAccess::Local(node),
        db.agent_did(),
    )
    .await
    .unwrap();
    assert_eq!(after.pending_notifications, 0);
    assert_eq!(after.acknowledged_notifications, 1);
    assert_eq!(after.epochs[0].state, "acknowledged");
}

#[tokio::test]
async fn canonical_terminal_commit_makes_acknowledgement_restart_atomic() {
    let db = test_db("background-response-repair-ack").await;
    let node = db.node.clone();
    let parent = root_parent(db.agent_did(), "background-response-repair-ack-session");
    let hints = background_hints(&parent);
    let enqueued = persist_background_completion_with_message(
        node.as_ref(),
        &parent,
        "response-persisted notification",
        "background-completion-notification:response-persisted:tool",
        "review notifications",
        hints.clone(),
        None,
    )
    .await
    .unwrap();
    let request = wake_agent_request(
        &parent,
        &enqueued.request.as_ref().expect("non-Goal wake").doc_id,
        &enqueued.request.as_ref().expect("non-Goal wake").request_id,
        &hints,
    );
    let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        request,
        60,
        ExecutionOrigin::Scheduled,
        "backend-test",
    );
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        db.agent_did(),
        std::time::Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            gents_protocol::rendered_request::CaptureScope {
                kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                seq: 0,
            },
        )
        .await;
    let published = writer
        .publish_native_turn(
            &lifecycle,
            0,
            0,
            &Message::assistant("integrated notification"),
        )
        .await
        .unwrap();
    // Publication alone does not acknowledge the wake. The terminal commit
    // selects the exact header and acknowledges the claimed notification set
    // atomically; startup must not infer completion from published bytes.
    let unpublished_terminal = crate::load_background_completion_diagnostics(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        db.agent_did(),
    )
    .await
    .unwrap();
    assert_eq!(unpublished_terminal.pending_notifications, 1);
    assert_eq!(unpublished_terminal.acknowledged_notifications, 0);
    lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: published.message_doc_id.clone(),
            },
            None,
        )
        .await
        .unwrap();

    let terminal = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state terminal_output }} }}"#,
            escape_graphql_string(&lifecycle.request().doc_id),
        ))
        .await;
    assert!(!terminal.has_errors(), "{:?}", terminal.errors);
    let terminal_row = &terminal.data.as_ref().unwrap()["AgentRequest"][0];
    assert_eq!(terminal_row["lifecycle_state"], "completed");
    assert_eq!(
        terminal_row["terminal_output"],
        serde_json::to_value(gents_protocol::output::TerminalOutput::Message {
            message_doc_id: published.message_doc_id,
        })
        .unwrap()
    );

    let first_repair =
        crate::RequestLifecycle::repair_terminal_requests(node.as_ref(), db.agent_did())
            .await
            .unwrap();
    assert_eq!(first_repair.repaired, 0);
    let first = crate::load_background_completion_diagnostics(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        db.agent_did(),
    )
    .await
    .unwrap();
    assert_eq!(first.pending_notifications, 0);
    assert_eq!(first.acknowledged_notifications, 1);
    assert_eq!(first.epochs[0].state, "acknowledged");

    let second_repair =
        crate::RequestLifecycle::repair_terminal_requests(node.as_ref(), db.agent_did())
            .await
            .unwrap();
    assert_eq!(second_repair.repaired, 0);
    let second = crate::load_background_completion_diagnostics(
        &crate::config_client::ConfigAccess::Local(node),
        db.agent_did(),
    )
    .await
    .unwrap();
    assert_eq!(
        second, first,
        "acknowledgement projection must be restart-idempotent"
    );
}

#[tokio::test]
async fn successor_acknowledges_input_left_by_a_failed_active_wake() {
    let db = test_db("background-successor-ack").await;
    let node = db.node.clone();
    let parent = root_parent(db.agent_did(), "background-successor-ack-session");
    let hints = background_hints(&parent);
    let first = persist_background_completion_with_message(
        node.as_ref(),
        &parent,
        "first notification",
        "background-completion-notification:successor-first:tool",
        "review notifications",
        hints.clone(),
        None,
    )
    .await
    .unwrap();
    let first_request = wake_agent_request(
        &parent,
        &first.request.as_ref().expect("non-Goal wake").doc_id,
        &first.request.as_ref().expect("non-Goal wake").request_id,
        &hints,
    );
    let mut first_lifecycle = crate::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        first_request,
        60,
        ExecutionOrigin::Scheduled,
        "backend-test",
    );
    assert_eq!(
        first_lifecycle.claim_with_identity().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    crate::session::import_history_observation(
        &node,
        "unrelated-foreground-request-doc",
        &parent.session_id,
        &parent.agent_did,
        parent.requester_did.as_deref(),
        "unrelated foreground input",
        "unrelated-foreground-input",
        first.message_sequence + 1,
        None,
    )
    .await;

    let second = persist_background_completion_with_message(
        node.as_ref(),
        &parent,
        "second notification",
        "background-completion-notification:successor-second:tool",
        "review notifications",
        hints.clone(),
        None,
    )
    .await
    .unwrap();
    assert!(second.created_request);
    assert_ne!(
        first.request.as_ref().expect("non-Goal wake").doc_id,
        second.request.as_ref().expect("non-Goal wake").doc_id
    );
    let generation_query = format!(
        r#"{{
            AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ retry_key }}
        }}"#,
        escape_graphql_string(&parent.session_id)
    );
    let generation_response = node.execute(&generation_query).await;
    assert!(
        !generation_response.has_errors(),
        "generation query: {:?}",
        generation_response.errors
    );
    let mut retry_keys = generation_response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["retry_key"].as_str())
        .filter(|key| key.starts_with("background-completion:"))
        .collect::<Vec<_>>();
    retry_keys.sort_unstable();
    assert_eq!(retry_keys.len(), 2);
    assert!(retry_keys[0].ends_with(":00000000000000000000"));
    assert!(retry_keys[1].ends_with(":00000000000000000001"));
    assert_eq!(
        retry_keys[0].rsplit_once(':').unwrap().0,
        retry_keys[1].rsplit_once(':').unwrap().0,
        "unrelated transcript writes must not advance the queue-local generation"
    );
    first_lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Failed,
            gents_protocol::output::TerminalOutput::NoMessage,
            Some("injected provider failure"),
        )
        .await
        .unwrap();

    let second_request = wake_agent_request(
        &parent,
        &second.request.as_ref().expect("non-Goal wake").doc_id,
        &second.request.as_ref().expect("non-Goal wake").request_id,
        &hints,
    );
    let mut second_lifecycle = crate::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        second_request,
        60,
        ExecutionOrigin::Scheduled,
        "backend-test",
    );
    assert_eq!(
        second_lifecycle.claim_with_identity().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        db.agent_did(),
        std::time::Duration::ZERO,
    );
    second_lifecycle
        .begin_owned_execution(&writer)
        .await
        .unwrap();
    second_lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();

    let access = crate::config_client::ConfigAccess::Local(node);
    let diagnostics = crate::load_background_completion_diagnostics(&access, db.agent_did())
        .await
        .unwrap();
    assert_eq!(diagnostics.pending_notifications, 0);
    assert_eq!(diagnostics.acknowledged_notifications, 2);
    assert_eq!(diagnostics.stranded_notifications, 0);
    let first_epoch = diagnostics
        .epochs
        .iter()
        .find(|epoch| {
            epoch.root_request_id == first.request.as_ref().expect("non-Goal wake").request_id
        })
        .unwrap();
    assert_eq!(first_epoch.state, "acknowledged_by_successor");
    let timeline = crate::run_timeline_fetch::load_run_timeline(
        &access,
        &first.request.as_ref().expect("non-Goal wake").request_id,
    )
    .await
    .unwrap();
    assert_eq!(timeline.background_completions.len(), 2);
    assert!(timeline.background_completion_diagnostics_error.is_none());
    assert!(timeline.child_request_ids.is_empty());
}

#[tokio::test]
async fn append_sequence_index_fetches_only_latest_header() {
    let db = test_db("sequence-index").await;
    let session = "sequence-index-session";
    let query = super::super::atomic_inputs::append_sequence_query(db.agent_did(), session);
    let access = crate::config_client::ConfigAccess::Local(db.node.clone());
    let empty = access.execute(&query).await.unwrap();
    assert!(empty["data"]["AgentMessage"].as_array().unwrap().is_empty());
    for sequence in 1..=49 {
        let response = crate::config_client::ConfigAccess::write_local(
            &db.node,
            "test.sequence_index",
            &format!(
                r#"mutation {{ create_AgentMessage(input: {{agent_did: "{}", session_id: "{}", requester_did: "reader-{sequence}", sequence: {sequence}}}) {{_docID}} }}"#,
                escape_graphql_string(db.agent_did()),
                escape_graphql_string(session),
            ),
        ).await.unwrap();
        crate::graphql::created_doc_id(&response, "AgentMessage").unwrap();
    }
    for (owner, other_session) in [
        ("did:key:foreign", session),
        (db.agent_did(), "another-session"),
    ] {
        crate::config_client::ConfigAccess::write_local(
            &db.node,
            "test.sequence_index",
            &format!(
                r#"mutation {{ create_AgentMessage(input: {{agent_did: "{}", session_id: "{}", sequence: 700}}) {{_docID}} }}"#,
                escape_graphql_string(owner),
                escape_graphql_string(other_session),
            ),
        ).await.unwrap();
    }
    let latest = access.execute(&query).await.unwrap();
    assert_eq!(latest["data"]["AgentMessage"][0]["sequence"], 49);
    fn metric(value: &serde_json::Value) -> Option<u64> {
        match value {
            serde_json::Value::Object(object) => object
                .get("docFetches")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| object.values().find_map(metric)),
            serde_json::Value::Array(array) => array.iter().find_map(metric),
            _ => None,
        }
    }
    let before = query.replace(
        "order: [{ agent_did: DESC }, { session_id: DESC }, { sequence: DESC }]",
        "order: { sequence: DESC }",
    );
    assert_ne!(before, query);
    let mut fetched = Vec::new();
    for query in [&before, &query] {
        let response = access
            .execute(&format!("query @explain(type: execute) {query}"))
            .await
            .unwrap();
        let plan = &response["data"]["explain"];
        assert_eq!(plan["executionSuccess"], true, "{response}");
        let roots = plan["operationNode"].as_array().unwrap();
        assert_eq!(roots.len(), 2);
        // DefraDB emits operation roots in query selection order: message, then tool call.
        fetched.push(metric(&roots[0]).unwrap_or_else(|| panic!("{response}")));
    }
    assert_eq!(fetched, vec![49, 1]);
    for (row_session, sequence, key) in [
        (session, "null", "null-sequence"),
        (session, "49", "duplicate-maximum"),
        ("only-null-sequence", "null", "only-null"),
    ] {
        crate::config_client::ConfigAccess::write_local(
            &db.node,
            "test.sequence_index",
            &format!(
                r#"mutation {{ create_AgentMessage(input: {{agent_did: "{}", session_id: "{}", message_key: "{}", sequence: {sequence}}}) {{_docID}} }}"#,
                escape_graphql_string(db.agent_did()),
                escape_graphql_string(row_session),
                escape_graphql_string(key),
            ),
        ).await.unwrap();
    }
    for row_session in [session, "only-null-sequence"] {
        let indexed =
            super::super::atomic_inputs::append_sequence_query(db.agent_did(), row_session);
        let original = indexed.replace(
            "order: [{ agent_did: DESC }, { session_id: DESC }, { sequence: DESC }]",
            "order: { sequence: DESC }",
        );
        assert_eq!(
            access.execute(&indexed).await.unwrap()["data"]["AgentMessage"],
            access.execute(&original).await.unwrap()["data"]["AgentMessage"],
            "index ordering must preserve nullable sequence and duplicate-maximum results",
        );
    }
    let txn = ConfigApplyTxn::begin_local(&db.node, None).await.unwrap();
    let next = super::super::atomic_inputs::next_append_sequence_in_transaction(
        &txn,
        db.agent_did(),
        session,
    )
    .await
    .unwrap();
    txn.discard().await.unwrap();
    assert_eq!(next, 50);
}

#[tokio::test]
async fn append_sequence_excludes_foreign_session_messages_and_reservations() {
    let db = test_db("sequence-owner-scope").await;
    let session = "same-session-label";
    for (owner, sequence) in [(db.agent_did(), 4), ("did:key:foreign", 700)] {
        crate::session::import_history_observation(
            &db.node,
            &format!("observed-request-{sequence}"),
            session,
            owner,
            None,
            "input",
            &format!("observed-input-{sequence}"),
            sequence,
            None,
        )
        .await;
    }
    for (id, owner, sequence) in [
        ("own-reservation", db.agent_did(), 5),
        ("foreign-reservation", "did:key:foreign", 900),
    ] {
        let response = db.node.execute(&format!(r#"mutation {{ create_AgentToolCall(input:{{tool_call_id:"{}",session_id:"{}",agent_did:"{}",message_sequence:{sequence},await_mode:"background"}}){{_docID}} }}"#, escape_graphql_string(id), escape_graphql_string(session), escape_graphql_string(owner))).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    let txn = ConfigApplyTxn::begin_local(&db.node, None).await.unwrap();
    let next = super::super::atomic_inputs::next_append_sequence_in_transaction(
        &txn,
        db.agent_did(),
        session,
    )
    .await
    .unwrap();
    txn.discard().await.unwrap();
    assert_eq!(
        next, 7,
        "own background reservation remains ahead of own message; foreign rows cannot move this session's cursor"
    );
}

/// Lean `CausalHop.completionWake`: the next wake claim consumes every pending
/// notification, so a notification that needs a higher hop raises the one
/// pending wake instead of riding a lower-hop wake already queued.
#[tokio::test]
async fn a_lower_hop_wake_never_consumes_a_higher_hop_notification() {
    let db = test_db("wake-hop-raise").await;
    let parent = root_parent(db.agent_did(), "wake-hop-raise-session");
    let native = persist_background_completion_with_message(
        &db.node,
        &parent,
        "native process done",
        "background-completion-notification:native:tool",
        "review notifications",
        background_hints(&parent),
        None,
    )
    .await
    .unwrap();
    let native_wake = native.request.expect("native wake").doc_id;
    let raised = persist_background_completion_with_message_waking(
        &db.node,
        &parent,
        "agent result",
        "background-completion-notification:agent:tool",
        "review notifications",
        background_hints(&parent),
        None,
        crate::lifecycle::RequestHopCause::CrossSession { cause_hop: 1 },
    )
    .await
    .unwrap();
    assert!(raised.created_request);
    let raised_wake = raised.request.expect("raised wake").doc_id;
    assert_ne!(raised_wake, native_wake);
    let later = persist_background_completion_with_message(
        &db.node,
        &parent,
        "another native process done",
        "background-completion-notification:native-2:tool",
        "review notifications",
        background_hints(&parent),
        None,
    )
    .await
    .unwrap();
    assert!(!later.created_request);
    assert_eq!(later.request.expect("joined wake").doc_id, raised_wake);

    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ _docID lifecycle_state subagent_depth superseded_by_request_doc_id }} }}"#,
            escape_graphql_string(&parent.session_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .clone();
    let pending = rows
        .iter()
        .filter(|row| row["lifecycle_state"] == "pending")
        .collect::<Vec<_>>();
    assert_eq!(pending.len(), 1, "{rows:?}");
    assert_eq!(pending[0]["_docID"], raised_wake.as_str());
    assert_eq!(pending[0]["subagent_depth"], 2);
    let lower = rows
        .iter()
        .find(|row| row["_docID"] == native_wake.as_str())
        .unwrap();
    assert_eq!(lower["lifecycle_state"], "superseded");
    assert_eq!(lower["superseded_by_request_doc_id"], raised_wake.as_str());
}

/// Lean `CausalHop.return_keeps_caller_hop` and
/// `CausalHop.return_into_refused_session_is_refused`: B's result returned to
/// A wakes A at A's own hop and joins A's pending process wake. Once a request
/// over the bound is A's latest, a later returned result copies that hop and
/// is refused too.
#[tokio::test]
async fn a_returned_result_keeps_the_callers_hop_until_the_session_is_refused() {
    let db = test_db("wake-hop-refusal").await;
    let parent = root_parent(db.agent_did(), "wake-hop-refusal-session");
    let bound = crate::document_config::DEFAULT_MAX_REQUEST_HOP;
    let hop_of = |doc_id: String| {
        let node = db.node.clone();
        async move {
            let response = node
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ subagent_depth lifecycle_state }} }}"#,
                    escape_graphql_string(&doc_id)
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            response.data.unwrap()["AgentRequest"][0].clone()
        }
    };
    // A's background process completed first: a wake at A's hop is pending.
    let native = persist_background_completion_with_message_waking(
        &db.node,
        &parent,
        "sleep done",
        "background-completion-notification:sleep:tool",
        "review notifications",
        background_hints(&parent),
        None,
        crate::lifecycle::RequestHopCause::Continuation,
    )
    .await
    .unwrap();
    let native_wake = native.request.expect("native wake").doc_id;
    // B's result returns at A's hop, so it rides the pending wake.
    let returned = persist_background_completion_with_message_waking(
        &db.node,
        &parent,
        "B result",
        "background-completion-notification:b:tool",
        "review notifications",
        background_hints(&parent),
        None,
        crate::lifecycle::RequestHopCause::Return,
    )
    .await
    .unwrap();
    assert!(!returned.created_request);
    assert_eq!(returned.request.expect("joined wake").doc_id, native_wake);
    assert_eq!(hop_of(native_wake.clone()).await["subagent_depth"], 0);
    // A request past the bound becomes A's latest.
    let refused = persist_background_completion_with_message_waking(
        &db.node,
        &parent,
        "C message",
        "background-completion-notification:c:tool",
        "review notifications",
        background_hints(&parent),
        None,
        crate::lifecycle::RequestHopCause::CrossSession { cause_hop: bound },
    )
    .await
    .unwrap();
    let refused_wake = refused.request.expect("over-bound wake").doc_id;
    let refused_row = hop_of(refused_wake.clone()).await;
    assert_eq!(refused_row["subagent_depth"], bound + 1);
    assert!(!crate::lifecycle::request_hop_within_bound(
        bound,
        bound + 1
    ));
    assert_eq!(hop_of(native_wake).await["lifecycle_state"], "superseded");
    // Admission refuses the over-bound wake; it stays the session's latest.
    let response = db
        .node
        .execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "dead" }}) {{ _docID }} }}"#,
            escape_graphql_string(&refused_wake)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    // A later returned result copies the refused hop and is refused as well.
    let later = persist_background_completion_with_message_waking(
        &db.node,
        &parent,
        "another B result",
        "background-completion-notification:b-2:tool",
        "review notifications",
        background_hints(&parent),
        None,
        crate::lifecycle::RequestHopCause::Return,
    )
    .await
    .unwrap();
    assert!(later.created_request);
    let later_row = hop_of(later.request.expect("later wake").doc_id).await;
    assert_eq!(later_row["subagent_depth"], bound + 1);
}
