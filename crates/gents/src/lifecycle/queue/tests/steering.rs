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
