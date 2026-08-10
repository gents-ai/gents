use super::*;

fn root_parent(session_id: &str) -> AgentRequest {
    let mut parent = parent_request(session_id);
    parent.subagent_depth = 0;
    parent.caused_by_parent_request_id = None;
    parent.caused_by_parent_request_doc_id = None;
    parent.caused_by_parent_tool_call_id = None;
    parent.caused_by_parent_tool_call_doc_id = None;
    parent
}

fn background_hints(parent: &AgentRequest) -> QueueHints {
    QueueHints {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{}", parent.session_id)),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
    }
}

#[tokio::test]
async fn notification_is_atomically_bound_to_coalesced_wake() {
    let db = test_db("atomic-background-notification").await;
    let parent = root_parent("atomic-background-session");
    let first = enqueue_background_completion_with_message(
        &db.node,
        &parent,
        "first notification",
        "background-completion-notification:first:tool",
        "review notifications",
        background_hints(&parent),
    )
    .await
    .unwrap();
    let second = enqueue_background_completion_with_message(
        &db.node,
        &parent,
        "second notification",
        "background-completion-notification:second:tool",
        "review notifications",
        background_hints(&parent),
    )
    .await
    .unwrap();
    assert!(first.created_request);
    assert!(!second.created_request);
    assert_eq!(first.request.doc_id, second.request.doc_id);

    let wake_query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
            }}
        }}"#,
        escape_graphql_string(&first.request.doc_id)
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
            ) {{ request_id request_doc_id }}
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
            row["request_id"].as_str(),
            Some(first.request.request_id.as_str())
        );
        assert_eq!(
            row["request_doc_id"].as_str(),
            Some(first.request.doc_id.as_str())
        );
    }
}

#[tokio::test]
async fn concurrent_notifications_converge_to_one_pending_wake() {
    let db = test_db("atomic-background-race").await;
    let parent = root_parent("atomic-background-race-session");
    let first = enqueue_background_completion_with_message(
        &db.node,
        &parent,
        "first concurrent notification",
        "background-completion-notification:race-first:tool",
        "review notifications",
        background_hints(&parent),
    );
    let second = enqueue_background_completion_with_message(
        &db.node,
        &parent,
        "second concurrent notification",
        "background-completion-notification:race-second:tool",
        "review notifications",
        background_hints(&parent),
    );
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.request.doc_id, second.request.doc_id);
    assert_eq!(
        [first.created_request, second.created_request]
            .into_iter()
            .filter(|created| *created)
            .count(),
        1
    );

    let request_query = format!(
        r#"{{
            AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{
                _docID request_id status lifecycle_state
            }}
        }}"#,
        escape_graphql_string(&parent.session_id)
    );
    let request_response = db.node.execute(&request_query).await;
    assert!(
        !request_response.has_errors(),
        "pending wake query: {:?}",
        request_response.errors
    );
    let requests = request_response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|row| row["status"] == "pending" && row["lifecycle_state"] == "pending")
            .count(),
        1
    );

    let message_query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ sequence: ASC }}
            ) {{ request_id request_doc_id }}
        }}"#,
        escape_graphql_string(&parent.session_id)
    );
    let message_response = db.node.execute(&message_query).await;
    assert!(
        !message_response.has_errors(),
        "message query: {:?}",
        message_response.errors
    );
    let rows = message_response.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 2);
    let actual = rows
        .iter()
        .map(|row| {
            (
                row["request_id"].as_str().unwrap().to_string(),
                row["request_doc_id"].as_str().unwrap().to_string(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    let persisted_bindings = requests
        .into_iter()
        .map(|row| {
            (
                row["request_id"].as_str().unwrap().to_string(),
                row["_docID"].as_str().unwrap().to_string(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(actual.is_subset(&persisted_bindings));
}
