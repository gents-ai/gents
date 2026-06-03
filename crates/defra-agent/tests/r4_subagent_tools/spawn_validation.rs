use super::*;

#[tokio::test]
async fn spawn_subagent_skip_payload_is_persisted_to_transcript() {
    let fixture = setup_spawn_fixture(
        "spawn_subagent_skip_transcript",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let parent_deadline = fixture.parent_deadline;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "child prompt for transcript",
        "await_mode": "background",
        "deadline": (parent_deadline - chrono::Duration::minutes(1)).to_rfc3339()
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-transcript".to_string()),
        "internal-spawn-transcript",
        &args,
    )
    .await;
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action");
    };
    let child_request_id = serde_json::from_str::<Value>(&reason).unwrap()["child_request_id"]
        .as_str()
        .unwrap()
        .to_string();

    hook.persist_message(&Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: "internal-spawn-transcript".to_string(),
            call_id: Some("model-call-transcript".to_string()),
            function: ToolFunction {
                name: "spawn_subagent".to_string(),
                arguments: serde_json::from_str(&args).unwrap(),
            },
            signature: None,
            additional_params: None,
        })),
    })
    .await
    .unwrap();
    hook.persist_stream_tool_result_message(
        &ToolResult {
            id: "internal-spawn-transcript".to_string(),
            call_id: Some("model-call-transcript".to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: reason.clone(),
            })),
        },
        "internal-spawn-transcript",
    )
    .await
    .unwrap();

    let history = load_history(db.node.as_ref(), &session_id).await.unwrap();
    assert!(history.iter().any(|message| {
        matches!(
            message,
            Message::User { content }
                if matches!(content.first_ref(), UserContent::ToolResult(tool_result)
                    if matches!(tool_result.content.first_ref(), ToolResultContent::Text(Text { text })
                        if text.contains(&child_request_id)
                            && text.contains("\"await_mode\": \"background\"")))
        )
    }));
}

#[tokio::test]
async fn spawn_subagent_rejects_unauthorized_target_without_child_request() {
    let fixture = setup_spawn_fixture(
        "spawn_subagent_unauthorized",
        vec!["different-child"],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "should not spawn",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-denied",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "tool_not_allowed");
    assert_eq!(error["requested_tool_name"], CHILD_BEHAVIOR_ID);

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-denied").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    assert!(tool
        .result
        .as_deref()
        .is_some_and(|result| result.contains("\"tool_not_allowed\"")));
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-denied")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_when_spawn_disabled_without_child_request() {
    let fixture = setup_spawn_fixture_with_flags(
        "spawn_subagent_spawn_disabled",
        vec![CHILD_BEHAVIOR_ID],
        0,
        false,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "should not spawn",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-disabled",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["failure_class"], "tool_not_allowed");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-disabled").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-disabled")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_background_when_background_disabled_without_child_request() {
    let fixture = setup_spawn_fixture(
        "spawn_subagent_background_disabled",
        vec![CHILD_BEHAVIOR_ID],
        0,
        false,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "should not spawn in background",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-bg-disabled",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["failure_class"], "tool_not_allowed");
    assert_eq!(error["requested_tool_name"], "background");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-bg-disabled").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(
        tool.tool_failure_class.as_deref(),
        Some("serviceUnavailable")
    );
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-bg-disabled")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_deadline_after_parent_without_child_request() {
    let fixture =
        setup_spawn_fixture("spawn_subagent_deadline", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let parent_deadline = fixture.parent_deadline;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "deadline too late",
        "await_mode": "background",
        "deadline": (parent_deadline + chrono::Duration::seconds(1)).to_rfc3339()
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-deadline",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["failure_class"], "invalid_tool_arguments");
    assert_eq!(error["path"], "/deadline");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-deadline").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(tool.tool_failure_class.as_deref(), Some("argumentInvalid"));
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-deadline")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn spawn_subagent_rejects_depth_ceiling_without_child_request() {
    let fixture = setup_spawn_fixture(
        "spawn_subagent_depth",
        vec![CHILD_BEHAVIOR_ID],
        MAX_SUBAGENT_DEPTH,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "too deep",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        None,
        "internal-spawn-depth",
        &args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "invalid_tool_arguments");
    assert_eq!(error["code"], "subagent_depth_exceeded");
    assert_eq!(error["parent_subagent_depth"], json!(MAX_SUBAGENT_DEPTH));
    assert_eq!(error["max_subagent_depth"], json!(MAX_SUBAGENT_DEPTH));

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-depth").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(tool.tool_failure_class.as_deref(), Some("argumentInvalid"));
    assert!(
        child_request_for_tool(db.node.as_ref(), "internal-spawn-depth")
            .await
            .is_none()
    );
}
