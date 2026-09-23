use super::*;

#[tokio::test]
async fn atomic_background_completion_coalesces_keyed_subagent_wakeups() {
    let db = test_db("coalesce").await;
    let session_id = "session-coalesced-wakeup";
    let mut fixture = canonical_background_fixture(&db, session_id).await;
    let parent = fixture.parent.clone();
    let hints = RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{session_id}")),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    };

    let first = fixture
        .persist_notification(
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
    let second = fixture
        .persist_notification(
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

    let rows = queue_rows(&db.node, session_id)
        .await
        .into_iter()
        .filter(|row| {
            row.input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .is_some_and(queue_is_automated_wakeup)
        })
        .collect::<Vec<_>>();
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
    assert_eq!(row.subagent_depth, Some(parent.subagent_depth));
    assert_eq!(
        row.caused_by_parent_request_id.as_deref(),
        Some(parent.request_id.as_str())
    );
    assert_eq!(
        row.caused_by_parent_request_doc_id.as_deref(),
        Some(parent.doc_id.as_str())
    );
    assert_eq!(row.caused_by_parent_tool_call_id.as_deref(), None);
    assert_eq!(row.caused_by_parent_tool_call_doc_id.as_deref(), None);
    assert!(row
        .input
        .as_ref()
        .and_then(|input| input.queue.as_ref())
        .is_some_and(|queue| queue_is_automated_wakeup(queue)));
    let notifications = db
        .node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ request_doc_id }} }}"#,
            escape_graphql_string(&first.doc_id)
        ))
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
        assert_eq!(message["request_doc_id"], first.doc_id);
    }
}

#[tokio::test]
async fn atomic_background_completion_ignores_append_row_with_same_source_and_key() {
    let db = test_db("coalesce-ignores-append").await;
    let session_id = "session-coalesce-ignores-append";
    let mut fixture = canonical_background_fixture(&db, session_id).await;
    let parent = fixture.parent.clone();
    let append_hints = RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Append,
        key: Some(format!("background_completion:{session_id}")),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    };
    insert_raw_queue_request(
        &db.node,
        db.agent_did(),
        "req-existing-append-same-key",
        session_id,
        &wake_queue_input(append_hints.clone()),
    )
    .await;
    let coalesce_hints = RequestQueue {
        policy: QueuePolicy::Coalesce,
        ..append_hints
    };

    let enqueued = fixture
        .persist_notification(
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

    // Include both policies: queue_is_automated_wakeup deliberately selects
    // only coalesced wakes and would hide the append row under test.
    let rows = queue_rows(&db.node, session_id)
        .await
        .into_iter()
        .filter(|row| {
            row.input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .is_some_and(|queue| {
                    queue.source == QueueSource::BackgroundCompletion
                        && queue.key.as_deref()
                            == Some(format!("background_completion:{session_id}").as_str())
                })
        })
        .collect::<Vec<_>>();
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
    let mut fixture = canonical_background_fixture(&db, session_id).await;
    let parent = fixture.parent.clone();
    let hints = RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{session_id}")),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    };
    let key = hints.key.clone().unwrap();
    let survivor = fixture
        .persist_notification(
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
        &wake_queue_input(hints.clone()),
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

    let reused = fixture
        .persist_notification(
            "notification after duplicate reconciliation",
            "background-completion-notification:coalesce-reuse:tool",
            "reuse the surviving wake",
            hints,
            None,
        )
        .await
        .unwrap()
        .request
        .expect("non-Goal wake");
    assert_eq!(
        reused.doc_id, survivor.doc_id,
        "the enqueue owner must reuse the reconciled pending wake"
    );
}
#[tokio::test]
async fn atomic_background_completion_without_key_rejects_without_persisting_input() {
    let db = test_db("coalesce-without-key").await;
    let session_id = "session-unkeyed-wakeup";
    let mut fixture = canonical_background_fixture(&db, session_id).await;
    let parent = fixture.parent.clone();
    let hints = RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: None,
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    };
    let result = fixture
        .persist_notification(
            "terminal notification",
            "background-completion-notification:unkeyed:tool",
            "review notifications",
            hints,
            None,
        )
        .await;
    assert!(result.is_err());
    assert!(queue_rows(&db.node, session_id)
        .await
        .into_iter()
        .all(|row| {
            !row.input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .is_some_and(queue_is_automated_wakeup)
        }));
    let response = db.node.execute("{ AgentMessage {_docID} }").await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert!(response.data.unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .is_empty());
}
