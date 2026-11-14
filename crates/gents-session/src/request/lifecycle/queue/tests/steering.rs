use super::*;

/// Reads through the Agent's own access: the queued rows are Agent-owned, so
/// the node identity does not see them.
async fn agent_data(db: &TestDb, query: &str) -> serde_json::Value {
    let response = db
        .agent_access
        .execute(query)
        .await
        .expect("agent queue query");
    assert!(response.get("errors").is_none(), "{response}");
    response["data"].clone()
}

async fn enqueue_test_steering(
    db: &TestDb,
    parent: &gents_protocol::row::AgentRequest,
    content: &str,
    input: RequestInput,
) -> anyhow::Result<gents_protocol::request_lifecycle::EnqueuedAgentRequest> {
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

    let request = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &db.agent_access,
        &enqueued.doc_id,
    )
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
    assert_eq!(messages["AgentMessage"], serde_json::json!([]));
}

#[tokio::test]
async fn generated_pending_steering_terminals_retain_signed_admission_without_output() {
    for case in
        gents_lean_vocab::families::contracts_queued_steering_trace_pending_terminal::cases()
    {
        let name = case.input.name.clone();
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
        let content = format!("steering input token {}", case.input.content_token);
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
        let doc_id = gents_store::graphql::escape_graphql_string(&enqueued.doc_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ _docID lifecycle_state content input admission_signature }} }}"#
        );
        let before = agent_data(&db, &query).await["AgentRequest"][0].clone();
        assert_eq!(before["content"], content, "{name}");
        let request = crate::request::binding::load_agent_request_by_doc_id_with_access(
            &db.agent_access,
            &enqueued.doc_id,
        )
        .await
        .unwrap()
        .unwrap();
        let mut lifecycle = RequestLifecycle::new_with_node_did(db.node.clone(), request, 60);
        lifecycle
            .bind_managed_agent(db.agent_identity.as_ref())
            .unwrap();
        match case.input.actions.last().map(String::as_str) {
            Some("interruptBeforeClaim") => {
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
            Some("admissionReject") => {
                lifecycle
                    .reject_admission("generated admission rejection")
                    .await
                    .unwrap();
            }
            _ => panic!("{name} is not a pending steering terminal"),
        }
        let after = agent_data(&db, &query).await;
        let after = &after["AgentRequest"][0];
        let output = agent_data(&db, &format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#
        )).await;
        assert!(
            output["AgentOutputSegment"].as_array().unwrap().is_empty(),
            "{name}"
        );
        case.check(Some(
            gents_lean_vocab::families::contracts_queued_steering_trace_pending_terminal::PendingOutput {
                lifecycle_state: after["lifecycle_state"].as_str().unwrap().to_owned(),
                accepted_input: retains_signed_admission(&before, after),
                canonical_authored_count: output["AgentMessage"].as_array().unwrap().len() as u64,
            },
        ));
    }
}

fn retains_signed_admission(before: &serde_json::Value, after: &serde_json::Value) -> bool {
    before["admission_signature"]
        .as_str()
        .is_some_and(|signature| !signature.is_empty())
        && after["content"] == before["content"]
        && after["input"] == before["input"]
        && after["admission_signature"] == before["admission_signature"]
}

#[tokio::test]
async fn generated_owned_prepublication_terminals_retain_signed_admission_without_output() {
    for case in gents_lean_vocab::families::contracts_queued_steering_trace_owned_terminal::cases()
    {
        let name = case.input.name.clone();
        let outcome = if case
            .input
            .actions
            .iter()
            .any(|action| action == "latchInterrupt")
        {
            crate::request::lifecycle::RequestTerminalOutcome::Interrupted
        } else {
            crate::request::lifecycle::RequestTerminalOutcome::Failed
        };

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
        crate::request::admission::sign_agent_request_create(
            db.agent_identity.as_ref(),
            &mut parent_create,
        )
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
        let content = format!("steering input token {}", case.input.content_token);
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
        let physical = gents_store::graphql::escape_graphql_string(&enqueued.doc_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 1) {{ lifecycle_state content input admission_signature terminal_output }} AgentMessage(filter: {{ request_doc_id: {{ _eq: "{physical}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{physical}" }} }}) {{ _docID }} }}"#
        );
        let before = agent_data(&db, &query).await;
        let original = &before["AgentRequest"][0];
        assert_eq!(original["content"], content, "{name}");

        let parent_row = crate::request::binding::load_agent_request_by_doc_id_with_access(
            &db.agent_access,
            &parent.doc_id,
        )
        .await
        .unwrap()
        .expect("parent queue fixture");
        let mut parent_lifecycle =
            RequestLifecycle::new_with_node_did(db.node.clone(), parent_row, 60);
        parent_lifecycle
            .bind_managed_agent(db.agent_identity.as_ref())
            .unwrap();
        parent_lifecycle
            .reject_admission("finished parent fixture")
            .await
            .unwrap();

        let request = crate::request::binding::load_agent_request_by_doc_id_with_access(
            &db.agent_access,
            &enqueued.doc_id,
        )
        .await
        .unwrap()
        .expect("signed queued steering request");
        let mut lifecycle = RequestLifecycle::new_with_node_did(db.node.clone(), request, 60);
        lifecycle
            .bind_managed_agent(db.agent_identity.as_ref())
            .unwrap();
        assert_eq!(
            lifecycle.claim().await.unwrap(),
            ClaimOutcome::Claimed,
            "{name}"
        );
        let writer = DefraStreamWriter::new_managed(
            db.node.clone(),
            db.agent_identity.as_ref(),
            Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();

        if outcome == crate::request::lifecycle::RequestTerminalOutcome::Interrupted {
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
            "{name}"
        );
        let after = agent_data(&db, &query).await;
        let terminal = &after["AgentRequest"][0];
        assert_eq!(
            serde_json::from_value::<gents_protocol::output::TerminalOutput>(
                terminal["terminal_output"].clone(),
            )
            .unwrap(),
            gents_protocol::output::TerminalOutput::NoMessage,
            "{name}"
        );
        assert!(
            after["AgentOutputSegment"].as_array().unwrap().is_empty(),
            "{name}"
        );
        let queue_active = crate::request::interrupt::active_session_request_with_access(
            &db.agent_access,
            &parent.session_id,
            db.node_did(),
            Some(db.agent_did()),
        )
        .await
        .unwrap()
        .is_some();
        case.check(Some(
            gents_lean_vocab::families::contracts_queued_steering_trace_owned_terminal::OwnedOutput {
                lifecycle_state: terminal["lifecycle_state"].as_str().unwrap().to_owned(),
                accepted_input: retains_signed_admission(original, terminal),
                canonical_authored_count: after["AgentMessage"].as_array().unwrap().len() as u64,
                queue_active,
            },
        ));
    }
}
