use super::*;
use crate::lean_vocab_test::LeanQueuedSteeringAction as Action;

/// Reads through the Agent's own access: the queued rows are Agent-owned, so
/// the node identity does not see them.
async fn agent_data(db: &TestDb, query: &str) -> serde_json::Value {
    let response = db.agent_access.execute(query).await.expect("agent queue query");
    assert!(response.get("errors").is_none(), "{response}");
    response["data"].clone()
}

async fn enqueue_test_steering(
    db: &TestDb,
    parent: &crate::request::watcher::AgentRequest,
    content: &str,
    input: RequestInput,
) -> anyhow::Result<crate::request::lifecycle::materialize::EnqueuedAgentRequest> {
    let signer = db
        .agent_identity
        .managed_agent_signer()
        .expect("fixture Agent signer");
    enqueue_steering_request(&db.node, parent, content, input, &signer).await
}

#[tokio::test]
async fn steering_admission_keeps_signed_content_queued_without_transcript_publication() {
    let db = test_db("steering-admission-only").await;
    let mut parent = parent_request(db.node_did(), db.agent_did(), "steering-session");
    parent.doc_id = insert_agent_queue_request(
        &db,
        &parent.request_id,
        &parent.session_id,
        &RequestInput::default(),
    )
    .await;
    let content = "preserve this exact queued steering input: <>&\nsecond line";
    let enqueued = enqueue_test_steering(
        &db,
        &parent,
        content,
        RequestInput {
            queue: Some(RequestQueue {
                delivery: Default::default(),
                position: None,
                source: QueueSource::Steering,
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: Some(parent.request_id.clone()),
                interrupted_request_id: Some("interrupted-request".to_string()),
                background_completion_wake_version: None,
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let request = crate::request::binding::load_agent_request_by_doc_id_with_access(&db.agent_access, &enqueued.doc_id)
        .await
        .unwrap()
        .expect("queued steering request");
    assert_eq!(request.content, content);
    assert_eq!(request.request_id, enqueued.request_id);
    let queue = request.input.queue.expect("steering queue input");
    assert_eq!(queue.source, QueueSource::Steering);
    assert_eq!(queue.policy, QueuePolicy::Append);
    assert_eq!(
        queue.queued_after_request_id.as_deref(),
        Some("parent-request")
    );
    assert_eq!(
        queue.interrupted_request_id.as_deref(),
        Some("interrupted-request")
    );

    let messages = agent_data(&db, "{ AgentMessage { _docID } }").await;
    assert_eq!(
        messages["AgentMessage"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn generated_pending_steering_terminals_retain_signed_admission_without_output() {
    let cases = crate::lean_vocab_test::lean_queued_steering_trace_cases();
    let mut covered = 0;
    for case in cases {
        let terminal = match case.actions.as_slice() {
            [Action::Enqueue, Action::InterruptBeforeClaim] => Action::InterruptBeforeClaim,
            [Action::Enqueue, Action::AdmissionReject] => Action::AdmissionReject,
            _ => continue,
        };
        covered += 1;
        assert_eq!(case.entry.request_id, case.request_id, "{}", case.name);
        assert_eq!(case.entry.source, "steering", "{}", case.name);
        assert_eq!(case.entry.policy, "append", "{}", case.name);
        assert!(case.entry.queue_key.is_none(), "{}", case.name);
        assert!(case.entry.queued_after.is_some(), "{}", case.name);

        let db = test_db("generated-steering-terminal").await;
        let mut parent =
            parent_request(db.node_did(), db.agent_did(), "generated-steering-session");
        parent.doc_id = insert_agent_queue_request(
            &db,
            &parent.request_id,
            &parent.session_id,
            &RequestInput::default(),
        )
        .await;
        let content = format!("steering input token {}", case.content_token);
        let enqueued = enqueue_test_steering(
            &db,
            &parent,
            &content,
            RequestInput {
                queue: Some(RequestQueue {
                    delivery: Default::default(),
                    position: None,
                    source: QueueSource::Steering,
                    policy: QueuePolicy::Append,
                    key: None,
                    queued_after_request_id: Some(parent.request_id.clone()),
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let doc_id = crate::store::graphql::escape_graphql_string(&enqueued.doc_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ _docID lifecycle_state content input admission_signature }} }}"#
        );
        let before = agent_data(&db, &query).await["AgentRequest"][0].clone();
        assert_eq!(before["content"], content, "{}", case.name);
        assert!(
            before["admission_signature"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "{}",
            case.name
        );
        let request =
            crate::request::binding::load_agent_request_by_doc_id_with_access(&db.agent_access, &enqueued.doc_id)
                .await
                .unwrap()
                .unwrap();
        let mut lifecycle = RequestLifecycle::new_with_node_did(
            db.node.clone(),
            db.agent_did(),
            db.node_did(),
            request,
            60,
        );
        lifecycle.bind_managed_agent(db.agent_identity.as_ref()).unwrap();
        match terminal {
            Action::InterruptBeforeClaim => {
                assert!(case.interrupt_at.is_some(), "{}", case.name);
                crate::request::interrupt::interrupt_request_by_doc_id_with_access(
                    &db.agent_access,
                    &enqueued.doc_id,
                    db.node_did(),
                    Some(db.agent_did()),
                )
                .await
                .unwrap();
                assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
            }
            Action::AdmissionReject => {
                lifecycle
                    .reject_admission("generated admission rejection")
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let after = agent_data(&db, &query).await;
        let after = &after["AgentRequest"][0];
        assert_eq!(
            after["lifecycle_state"], case.lifecycle_state,
            "{}",
            case.name
        );
        assert_eq!(after["content"], before["content"], "{}", case.name);
        assert_eq!(after["input"], before["input"], "{}", case.name);
        assert_eq!(
            after["admission_signature"], before["admission_signature"],
            "{}",
            case.name
        );
        let output = agent_data(&db, &format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#
        )).await;
        assert_eq!(
            output["AgentMessage"].as_array().unwrap().len() as u64,
            case.canonical_authored_count,
            "{}",
            case.name
        );
        assert!(
            output["AgentOutputSegment"].as_array().unwrap().is_empty(),
            "{}",
            case.name
        );
    }
    assert_eq!(
        covered, 2,
        "only the two modeled pending terminal paths are bound here"
    );
}

#[tokio::test]
async fn generated_owned_prepublication_terminals_retain_signed_admission_without_output() {
    let cases = crate::lean_vocab_test::lean_queued_steering_trace_cases();
    let mut covered = 0;
    for case in cases {
        let outcome = match case.actions.as_slice() {
            [Action::Enqueue, Action::ClaimAndBegin, Action::PrepareFails, Action::Fail, Action::Capture, Action::Send] => {
                crate::request::lifecycle::RequestTerminalOutcome::Failed
            }
            [Action::Enqueue, Action::ClaimAndBegin, Action::PrepareFails, Action::LatchInterrupt, Action::InterruptProcessing, Action::Capture, Action::Send] => {
                crate::request::lifecycle::RequestTerminalOutcome::Interrupted
            }
            _ => continue,
        };
        covered += 1;
        assert!(case.accepted_input, "{}", case.name);
        assert!(!case.provider_send_permitted, "{}", case.name);
        assert_eq!(case.canonical_authored_count, 0, "{}", case.name);
        assert_eq!(case.queue_active, None, "{}", case.name);

        let db = test_db("generated-owned-steering-terminal").await;
        // The session is Agent-owned: created through the Agent's access so the
        // parent request's own session check finds the same physical row.
        let node_did = db.node_did().to_owned();
        let agent_did = db.agent_did().to_owned();
        let now = chrono::Utc::now().to_rfc3339();
        db.agent_access
            .transact_write("test.owned_steering_session", |txn| {
                let (node_did, agent_did, now) =
                    (node_did.as_str(), agent_did.as_str(), now.as_str());
                Box::pin(async move {
                    crate::session::ensure_session_in_txn(
                        txn,
                        "generated-owned-steering-session",
                        node_did,
                        agent_did,
                        Some(agent_did),
                        None,
                        None,
                        now,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        let mut parent_create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "parent-request",
            db.node_did(),
            db.agent_did(),
            db.agent_did(),
            "generated-owned-steering-session",
            "parent input",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                db.agent_did(),
            ),
        );
        crate::sign_agent_request_create(db.agent_identity.as_ref(), &mut parent_create)
            .await
            .unwrap();
        let parent_mutation = parent_create.graphql_mutation().unwrap();
        db.agent_access
            .transact_write("test.owned_steering_parent", |txn| {
                let parent_mutation = &parent_mutation;
                Box::pin(async move { Ok::<_, anyhow::Error>(txn.execute(parent_mutation).await?) })
            })
            .await
            .unwrap();
        let parent = crate::request::binding::load_agent_request_with_access(
            &db.agent_access,
            "parent-request",
        )
        .await
        .unwrap()
        .expect("signed parent fixture");
        let content = format!("steering input token {}", case.content_token);
        let enqueued = enqueue_test_steering(
            &db,
            &parent,
            &content,
            RequestInput {
                queue: Some(RequestQueue {
                    delivery: Default::default(),
                    position: None,
                    source: QueueSource::Steering,
                    policy: QueuePolicy::Append,
                    key: None,
                    queued_after_request_id: Some(parent.request_id.clone()),
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let physical = crate::store::graphql::escape_graphql_string(&enqueued.doc_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 1) {{ lifecycle_state content input admission_signature terminal_output }} AgentMessage(filter: {{ request_doc_id: {{ _eq: "{physical}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{physical}" }} }}) {{ _docID }} }}"#
        );
        let before = agent_data(&db, &query).await;
        let original = &before["AgentRequest"][0];
        assert_eq!(original["content"], content, "{}", case.name);
        assert!(
            original["admission_signature"]
                .as_str()
                .is_some_and(|signature| !signature.is_empty()),
            "{}",
            case.name
        );

        let parent_row =
            crate::request::binding::load_agent_request_by_doc_id_with_access(&db.agent_access, &parent.doc_id)
                .await
                .unwrap()
                .expect("parent queue fixture");
        let mut parent_lifecycle = RequestLifecycle::new_with_node_did(
            db.node.clone(),
            db.agent_did(),
            db.node_did(),
            parent_row,
            60,
        );
        parent_lifecycle.bind_managed_agent(db.agent_identity.as_ref()).unwrap();
        parent_lifecycle
            .reject_admission("finished parent fixture")
            .await
            .unwrap();

        let request =
            crate::request::binding::load_agent_request_by_doc_id_with_access(&db.agent_access, &enqueued.doc_id)
                .await
                .unwrap()
                .expect("signed queued steering request");
        let mut lifecycle = RequestLifecycle::new_with_node_did(
            db.node.clone(),
            db.agent_did(),
            db.node_did(),
            request,
            60,
        );
        lifecycle.bind_managed_agent(db.agent_identity.as_ref()).unwrap();
        assert_eq!(
            lifecycle.claim().await.unwrap(),
            ClaimOutcome::Claimed,
            "{}",
            case.name
        );
        let writer = DefraStreamWriter::new_managed(db.node.clone(), db.agent_identity.as_ref(), Duration::ZERO)
            .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();

        if outcome == crate::request::lifecycle::RequestTerminalOutcome::Interrupted {
            assert!(case.interrupt_at.is_none(), "{}", case.name);
            crate::request::interrupt::interrupt_request_by_doc_id_with_access(
                &db.agent_access,
                &enqueued.doc_id,
                db.node_did(),
                Some(db.agent_did()),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            lifecycle
                .terminalize_owned(
                    outcome,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("injected before authored publication")
                )
                .await
                .unwrap(),
            crate::request::lifecycle::TerminalizeResult::Won,
            "{}",
            case.name
        );
        let after = agent_data(&db, &query).await;
        let terminal = &after["AgentRequest"][0];
        assert_eq!(
            terminal["lifecycle_state"], case.lifecycle_state,
            "{}",
            case.name
        );
        assert_eq!(terminal["content"], original["content"], "{}", case.name);
        assert_eq!(terminal["input"], original["input"], "{}", case.name);
        assert_eq!(
            terminal["admission_signature"], original["admission_signature"],
            "{}",
            case.name
        );
        assert_eq!(
            serde_json::from_value::<gents_protocol::output::TerminalOutput>(
                terminal["terminal_output"].clone(),
            )
            .unwrap(),
            gents_protocol::output::TerminalOutput::NoMessage,
            "{}",
            case.name
        );
        assert_eq!(
            after["AgentMessage"].as_array().unwrap().len() as u64,
            case.canonical_authored_count,
            "{}",
            case.name
        );
        assert!(
            after["AgentOutputSegment"].as_array().unwrap().is_empty(),
            "{}",
            case.name
        );
        assert!(
            crate::request::interrupt::active_session_request_with_access(
                &db.agent_access,
                &parent.session_id,
                db.node_did(),
                Some(db.agent_did()),
            )
            .await
            .unwrap()
            .is_none(),
            "{} left an active request",
            case.name
        );
    }
    assert_eq!(
        covered, 2,
        "the two generated owned pre-publication terminals must be bound"
    );
}
