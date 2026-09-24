use super::*;

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
