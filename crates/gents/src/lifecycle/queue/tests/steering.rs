use super::*;

#[tokio::test]
async fn steering_claim_uses_durable_input_once_as_the_loop_prompt() {
    let TestDb { node, _tempdir } = test_db("steering-provider-input-once").await;
    let node = std::sync::Arc::new(node);
    let parent = {
        let mut parent = parent_request("steering-provider-input-once-session");
        parent.subagent_depth = 0;
        parent.caused_by_parent_request_id = None;
        parent.caused_by_parent_request_doc_id = None;
        parent.caused_by_parent_tool_call_id = None;
        parent.caused_by_parent_tool_call_doc_id = None;
        parent
    };
    let notification_key = "background-completion-notification:prior-child:subagent";
    let (notification_sequence, created_notification) =
        session::append_message_once_with_key_and_requester_did(
        node.as_ref(),
        &parent.session_id,
        &parent.agent_did,
        parent.requester_did.as_deref(),
        "user",
        r#"<subagent-notification child_request_id="prior-child" status="completed"><summary>done</summary></subagent-notification>"#,
        None,
        None,
        None,
        notification_key,
        None,
    )
    .await
        .unwrap();
    assert!(created_notification);
    let steering_text = "also inspect the staging configuration";
    let enqueued = enqueue_conversation_continuation(
        node.as_ref(),
        &parent,
        ConversationContinuation::Steering {
            message: steering_text,
            interrupted_request_id: None,
        },
    )
    .await
    .unwrap();
    let input_sequence = enqueued.input_sequence.unwrap();
    assert_eq!(input_sequence, notification_sequence + 1);
    let request =
        crate::request_binding::load_agent_request(node.as_ref(), &enqueued.request.request_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(request.content, steering_text);

    let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        TEST_BEHAVIOR_ID,
        TEST_AGENT_DID,
        request,
        60,
        ExecutionOrigin::Interactive,
        "backend-test",
    );
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    assert_eq!(
        lifecycle.provider_history_through_sequence(),
        Some(input_sequence.saturating_sub(1))
    );
    let history = session::load_history_through_sequence(
        node.as_ref(),
        &parent.session_id,
        lifecycle.provider_history_through_sequence(),
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 1);

    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ sequence message_key }} }}"#,
            escape_graphql_string(&parent.session_id)
        ))
        .await;
    assert!(
        !response.has_errors(),
        "message rows: {:?}",
        response.errors
    );
    let rows = response.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| {
        row["sequence"].as_u64() == Some(u64::from(notification_sequence))
            && row["message_key"].as_str() == Some(notification_key)
    }));
    assert!(rows.iter().any(|row| {
        row["sequence"].as_u64() == Some(u64::from(input_sequence))
            && row["message_key"].as_str()
                == Some(steering_input_message_key(&enqueued.request.request_id).as_str())
    }));
}

async fn pending_versioned_steering_request(
    node: &EmbeddedNode,
    session_id: &str,
    request_id: &str,
) -> (String, AgentRequest) {
    let metadata = queue_metadata_json(&QueueHints {
        source: QueueSource::Steering,
        policy: QueuePolicy::Append,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
    });
    let doc_id = insert_raw_queue_request(node, request_id, session_id, &metadata).await;
    let request = crate::request_binding::load_agent_request(node, request_id)
        .await
        .unwrap()
        .unwrap();
    (doc_id, request)
}

async fn assert_request_remains_pending(node: &EmbeddedNode, session_id: &str, request_id: &str) {
    let row = queue_rows(node, session_id)
        .await
        .into_iter()
        .find(|row| row.request_id == request_id)
        .unwrap();
    assert_eq!(row.status, "pending");
    assert_eq!(row.lifecycle_state.as_deref(), Some("pending"));
}

#[tokio::test]
async fn steering_claim_fails_closed_without_its_keyed_input() {
    let TestDb { node, _tempdir } = test_db("steering-missing-input").await;
    let node = std::sync::Arc::new(node);
    let session_id = "steering-missing-input-session";
    let request_id = "steering-missing-input-request";
    let (_, request) =
        pending_versioned_steering_request(node.as_ref(), session_id, request_id).await;
    let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        TEST_BEHAVIOR_ID,
        TEST_AGENT_DID,
        request,
        60,
        ExecutionOrigin::Interactive,
        "backend-test",
    );

    let error = lifecycle
        .claim_with_identity()
        .await
        .expect_err("a steering request without its durable input must not be claimed");
    assert!(error
        .to_string()
        .contains("must resolve exactly one durable input row"));
    assert_request_remains_pending(node.as_ref(), session_id, request_id).await;
}
