use super::*;
use crate::lean_vocab_test::LeanQueuedSteeringAction as Action;

#[tokio::test]
async fn steering_admission_keeps_signed_content_queued_without_transcript_publication() {
    let crate::lean_vocab_test::LeanR4cBackgroundWorkCase::SteerAppendPreservesLineage {
        queue_source,
        queue_policy,
        ..
    } = crate::lean_vocab_test::lean_r4c_background_work_case(
        "r4c.steer_subagent.append_preserves_lineage",
    )
    else {
        panic!("steering admission witness variant drifted");
    };
    assert_eq!(queue_source, "steering");
    assert_eq!(queue_policy, "append");

    let db = test_db("steering-admission-only").await;
    let mut parent = parent_request(db.agent_did(), "steering-session");
    parent.doc_id = insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        &parent.request_id,
        &parent.session_id,
        &RequestInput::default(),
    )
    .await;
    let content = "preserve this exact queued steering input: <>&\nsecond line";
    let enqueued = enqueue_steering_request(
        &db.node,
        &parent,
        content,
        RequestInput {
            queue: Some(RequestQueue {
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

    let request = crate::request_binding::load_agent_request_by_doc_id(&db.node, &enqueued.doc_id)
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

    let messages = db.node.execute("{ AgentMessage { _docID } }").await;
    assert!(!messages.has_errors(), "{:?}", messages.errors);
    assert_eq!(
        messages.data.unwrap()["AgentMessage"],
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
            _ => continue, // Other generated actions need their own native owners.
        };
        covered += 1;
        assert_eq!(case.entry.request_id, case.request_id, "{}", case.name);
        assert_eq!(case.entry.source, "steering", "{}", case.name);
        assert_eq!(case.entry.policy, "append", "{}", case.name);
        assert!(case.entry.queue_key.is_none(), "{}", case.name);
        assert!(case.entry.queued_after.is_some(), "{}", case.name);

        let db = test_db("generated-steering-terminal").await;
        let mut parent = parent_request(db.agent_did(), "generated-steering-session");
        parent.doc_id = insert_raw_queue_request(
            &db.node,
            db.agent_did(),
            &parent.request_id,
            &parent.session_id,
            &RequestInput::default(),
        )
        .await;
        let content = format!("steering input token {}", case.content_token);
        let enqueued = enqueue_steering_request(
            &db.node,
            &parent,
            &content,
            RequestInput {
                queue: Some(RequestQueue {
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
        let doc_id = crate::graphql::escape_graphql_string(&enqueued.doc_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ _docID lifecycle_state content input admission_signature }} }}"#
        );
        let before = db.node.execute(&query).await;
        assert!(!before.has_errors(), "{}: {:?}", case.name, before.errors);
        let before = before.data.unwrap()["AgentRequest"][0].clone();
        assert_eq!(before["content"], content, "{}", case.name);
        assert!(
            before["admission_signature"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "{}",
            case.name
        );
        let request =
            crate::request_binding::load_agent_request_by_doc_id(&db.node, &enqueued.doc_id)
                .await
                .unwrap()
                .unwrap();
        let mut lifecycle = RequestLifecycle::new_with_agent_did(
            db.node.clone(),
            TEST_BEHAVIOR_ID,
            db.agent_did(),
            request,
            60,
        );
        match terminal {
            Action::InterruptBeforeClaim => {
                assert!(case.interrupt_at.is_some(), "{}", case.name);
                crate::interrupt::interrupt_request_by_doc_id(
                    &db.node,
                    &enqueued.doc_id,
                    db.agent_did(),
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
        let after = db.node.execute(&query).await;
        assert!(!after.has_errors(), "{}: {:?}", case.name, after.errors);
        let after = &after.data.unwrap()["AgentRequest"][0];
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
        let output = db.node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#
        )).await;
        assert!(!output.has_errors(), "{}: {:?}", case.name, output.errors);
        let output = output.data.unwrap();
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
                crate::lifecycle::RequestTerminalOutcome::Failed
            }
            [Action::Enqueue, Action::ClaimAndBegin, Action::PrepareFails, Action::LatchInterrupt, Action::InterruptProcessing, Action::Capture, Action::Send] => {
                crate::lifecycle::RequestTerminalOutcome::Interrupted
            }
            _ => continue,
        };
        covered += 1;
        assert!(case.accepted_input, "{}", case.name);
        assert!(!case.provider_send_permitted, "{}", case.name);
        assert_eq!(case.canonical_authored_count, 0, "{}", case.name);
        assert_eq!(case.queue_active, None, "{}", case.name);

        let db = test_db("generated-owned-steering-terminal").await;
        crate::session::ensure_session_with_behavior_id_and_requester_did(
            &db.node,
            "generated-owned-steering-session",
            TEST_BEHAVIOR_ID,
            db.agent_did(),
            TEST_BEHAVIOR_ID,
            Some(db.agent_did()),
        )
        .await
        .unwrap();
        let mut parent_create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "parent-request",
            db.agent_did(),
            db.agent_did(),
            TEST_BEHAVIOR_ID,
            "generated-owned-steering-session",
            "parent input",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                db.agent_did(),
            ),
        );
        crate::sign_agent_request_create(db.identity.as_ref(), &mut parent_create)
            .await
            .unwrap();
        let created_parent = db
            .node
            .execute(&parent_create.graphql_mutation().unwrap())
            .await;
        assert!(
            !created_parent.has_errors(),
            "{}: {:?}",
            case.name,
            created_parent.errors
        );
        let parent = crate::request_binding::load_agent_request(&db.node, "parent-request")
            .await
            .unwrap()
            .expect("signed parent fixture");
        let content = format!("steering input token {}", case.content_token);
        let enqueued = enqueue_steering_request(
            &db.node,
            &parent,
            &content,
            RequestInput {
                queue: Some(RequestQueue {
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
        let physical = crate::graphql::escape_graphql_string(&enqueued.doc_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 1) {{ lifecycle_state content input admission_signature terminal_output }} AgentMessage(filter: {{ request_doc_id: {{ _eq: "{physical}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{physical}" }} }}) {{ _docID }} }}"#
        );
        let before = db.node.execute(&query).await;
        assert!(!before.has_errors(), "{}: {:?}", case.name, before.errors);
        let before = before.data.unwrap();
        let original = &before["AgentRequest"][0];
        assert_eq!(original["content"], content, "{}", case.name);
        assert!(
            original["admission_signature"]
                .as_str()
                .is_some_and(|signature| !signature.is_empty()),
            "{}",
            case.name
        );

        // The signed parent supplies the queued-after edge; the enqueue owner
        // signs the child. Vacate the parent before child claim.
        let parent_row =
            crate::request_binding::load_agent_request_by_doc_id(&db.node, &parent.doc_id)
                .await
                .unwrap()
                .expect("parent queue fixture");
        let mut parent_lifecycle = RequestLifecycle::new_with_agent_did(
            db.node.clone(),
            TEST_BEHAVIOR_ID,
            db.agent_did(),
            parent_row,
            60,
        );
        parent_lifecycle
            .reject_admission("finished parent fixture")
            .await
            .unwrap();

        let request =
            crate::request_binding::load_agent_request_by_doc_id(&db.node, &enqueued.doc_id)
                .await
                .unwrap()
                .expect("signed queued steering request");
        let mut lifecycle = RequestLifecycle::new_with_agent_did(
            db.node.clone(),
            TEST_BEHAVIOR_ID,
            db.agent_did(),
            request,
            60,
        );
        assert_eq!(
            lifecycle.claim().await.unwrap(),
            ClaimOutcome::Claimed,
            "{}",
            case.name
        );
        let writer = DefraStreamWriter::new(db.node.clone(), db.agent_did(), Duration::ZERO);
        lifecycle.begin_owned_execution(&writer).await.unwrap();

        // This is the real owner handoff after claim/begin but before an
        // AuthoredInputReady stream item has been accepted. The upstream
        // preparation error/interrupt source is not simulated by this test.
        if outcome == crate::lifecycle::RequestTerminalOutcome::Interrupted {
            assert!(case.interrupt_at.is_none(), "{}", case.name);
            crate::interrupt::interrupt_request_by_doc_id(
                &db.node,
                &enqueued.doc_id,
                db.agent_did(),
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
            crate::lifecycle::TerminalizeResult::Won,
            "{}",
            case.name
        );
        let after = db.node.execute(&query).await;
        assert!(!after.has_errors(), "{}: {:?}", case.name, after.errors);
        let after = after.data.unwrap();
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
            crate::interrupt::active_session_request(
                &db.node,
                &parent.session_id,
                db.agent_did(),
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
