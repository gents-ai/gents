use super::*;

#[tokio::test]
async fn atomic_background_completion_coalesces_keyed_subagent_wakeups() {
    let db = test_db("coalesce").await;
    let session_id = "session-coalesced-wakeup";
    let parent = parent_request(db.agent_did(), session_id);
    let hints = QueueHints {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{session_id}")),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
    };

    let first = persist_background_completion_with_message(
        &db.node,
        &parent,
        "terminal notification 1",
        "background-completion-notification:coalesce-1:tool",
        "Process pending subagent completion notifications in this session.",
        hints.clone(),
        None,
    )
    .await
    .unwrap()
    .request
    .expect("non-Goal wake");
    let second = persist_background_completion_with_message(
        &db.node,
        &parent,
        "terminal notification 2",
        "background-completion-notification:coalesce-2:tool",
        "This duplicate wake-up should coalesce.",
        hints,
        None,
    )
    .await
    .unwrap()
    .request
    .expect("non-Goal wake");

    assert_eq!(second.doc_id, first.doc_id);
    assert_eq!(second.request_id, first.request_id);
    assert_eq!(second.session_id, session_id);

    let rows = queue_rows(&db.node, session_id).await;
    assert_eq!(rows.len(), 1, "coalescing should leave one wake-up row");
    let row = &rows[0];
    assert_eq!(row.doc_id, first.doc_id);
    assert_eq!(row.session_id, session_id);
    assert_eq!(row.behavior_id, TEST_BEHAVIOR_ID);
    assert_eq!(
        row.content,
        "Process pending subagent completion notifications in this session."
    );
    assert_eq!(row.execution_origin, "scheduled");
    assert_eq!(row.subagent_depth, Some(2));
    assert_eq!(
        row.caused_by_parent_request_id.as_deref(),
        Some("parent-request")
    );
    assert_eq!(
        row.caused_by_parent_request_doc_id.as_deref(),
        Some("parent-doc")
    );
    assert_eq!(row.caused_by_parent_tool_call_id.as_deref(), None);
    assert_eq!(row.caused_by_parent_tool_call_doc_id.as_deref(), None);
    assert!(is_automated_wakeup(row.metadata.as_deref()));
    let notifications = db
        .node
        .execute("{ AgentMessage { request_id request_doc_id } }")
        .await;
    assert!(!notifications.has_errors(), "{:?}", notifications.errors);
    let data = notifications.data.unwrap();
    let messages = data["AgentMessage"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        2,
        "coalescing preserves both durable inputs"
    );
    for message in messages {
        assert_eq!(message["request_id"], first.request_id);
        assert_eq!(message["request_doc_id"], first.doc_id);
    }
}

#[tokio::test]
async fn atomic_background_completion_ignores_append_row_with_same_source_and_key() {
    let db = test_db("coalesce-ignores-append").await;
    let session_id = "session-coalesce-ignores-append";
    let parent = parent_request(db.agent_did(), session_id);
    let append_hints = QueueHints {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Append,
        key: Some(format!("background_completion:{session_id}")),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
    };
    insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        "req-existing-append-same-key",
        session_id,
        &queue_metadata_json(&append_hints),
    )
    .await;
    let coalesce_hints = QueueHints {
        policy: QueuePolicy::Coalesce,
        ..append_hints
    };

    let enqueued = persist_background_completion_with_message(
        &db.node,
        &parent,
        "terminal notification 3",
        "background-completion-notification:coalesce-3:tool",
        "coalesced wake-up",
        coalesce_hints,
        None,
    )
    .await
    .unwrap()
    .request
    .expect("non-Goal wake");

    let rows = queue_rows(&db.node, session_id).await;
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|row| row.request_id == "req-existing-append-same-key"
            && row.lifecycle_state == Some(RequestLifecycleState::Pending)));
    assert!(rows.iter().any(|row| row.request_id == enqueued.request_id
        && row.lifecycle_state == Some(RequestLifecycleState::Pending)));
}

#[tokio::test]
async fn reconcile_coalesced_pending_request_supersedes_duplicate_race_rows() {
    let db = test_db("coalesce-race-reconcile").await;
    let session_id = "session-coalesce-race-reconcile";
    let parent = parent_request(db.agent_did(), session_id);
    let hints = QueueHints {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{session_id}")),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
    };
    let key = hints.key.clone().unwrap();
    let survivor = persist_background_completion_with_message(
        &db.node,
        &parent,
        "terminal notification 4",
        "background-completion-notification:coalesce-4:tool",
        "first wake-up",
        hints.clone(),
        None,
    )
    .await
    .unwrap()
    .request
    .expect("non-Goal wake");
    let duplicate_doc_id = insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        "req-coalesce-race-duplicate",
        session_id,
        &queue_metadata_json(&hints),
    )
    .await;

    let reconciled = reconcile_coalesced_pending_request(
        &db.node,
        session_id,
        db.agent_did(),
        QueueSource::BackgroundCompletion,
        &key,
    )
    .await
    .unwrap()
    .expect("survivor");
    assert_eq!(reconciled.request_id, survivor.request_id);

    let rows = queue_rows(&db.node, session_id).await;
    let survivor_row = rows
        .iter()
        .find(|row| row.request_id == survivor.request_id)
        .expect("survivor row");
    assert_eq!(
        survivor_row.lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );

    let duplicate = rows
        .iter()
        .find(|row| row.doc_id == duplicate_doc_id)
        .expect("duplicate row");
    assert_eq!(
        duplicate.lifecycle_state,
        Some(RequestLifecycleState::Superseded)
    );
    assert_eq!(
        duplicate.superseded_by_request.as_deref(),
        Some(survivor.request_id.as_str())
    );
    assert_eq!(
        duplicate.superseded_by_request_doc_id.as_deref(),
        Some(survivor.doc_id.as_str())
    );
}

#[tokio::test]
async fn atomic_background_completion_reuses_reconciled_duplicate_rows() {
    let db = test_db("coalesce-preexisting-duplicates").await;
    let session_id = "session-coalesce-preexisting-duplicates";
    let parent = parent_request(db.agent_did(), session_id);
    let hints = QueueHints {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{session_id}")),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
    };
    let survivor_doc_id = insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        "req-preexisting-coalesce-a-survivor",
        session_id,
        &queue_metadata_json(&hints),
    )
    .await;
    let duplicate_doc_id = insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        "req-preexisting-coalesce-b-duplicate",
        session_id,
        &queue_metadata_json(&hints),
    )
    .await;

    reconcile_coalesced_pending_request(
        &db.node,
        session_id,
        db.agent_did(),
        QueueSource::BackgroundCompletion,
        hints.key.as_deref().unwrap(),
    )
    .await
    .unwrap()
    .expect("preexisting survivor");

    let enqueued = persist_background_completion_with_message(
        &db.node,
        &parent,
        "terminal notification 5",
        "background-completion-notification:coalesce-5:tool",
        "should reuse survivor",
        hints,
        None,
    )
    .await
    .unwrap()
    .request
    .expect("non-Goal wake");
    assert_eq!(enqueued.doc_id, survivor_doc_id);

    let rows = queue_rows(&db.node, session_id).await;
    let survivor = rows
        .iter()
        .find(|row| row.doc_id == survivor_doc_id)
        .expect("survivor");
    assert_eq!(
        survivor.lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    let duplicate = rows
        .iter()
        .find(|row| row.doc_id == duplicate_doc_id)
        .expect("duplicate");
    assert_eq!(
        duplicate.lifecycle_state,
        Some(RequestLifecycleState::Superseded)
    );
    assert_eq!(
        duplicate.superseded_by_request.as_deref(),
        Some("req-preexisting-coalesce-a-survivor")
    );
    assert_eq!(
        duplicate.superseded_by_request_doc_id.as_deref(),
        Some(survivor.doc_id.as_str())
    );
}

#[tokio::test]
async fn atomic_background_completion_without_key_rejects_without_persisting_input() {
    let db = test_db("coalesce-without-key").await;
    let session_id = "session-unkeyed-wakeup";
    let parent = parent_request(db.agent_did(), session_id);
    let hints = QueueHints {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: None,
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
    };
    let result = persist_background_completion_with_message(
        &db.node,
        &parent,
        "terminal notification",
        "background-completion-notification:unkeyed:tool",
        "review notifications",
        hints,
        None,
    )
    .await;
    assert!(result.is_err());
    assert!(queue_rows(&db.node, session_id).await.is_empty());
    let response = db.node.execute("{ AgentMessage {_docID} }").await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert!(response.data.unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .is_empty());
}
