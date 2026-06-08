use super::*;

#[tokio::test]
async fn foreground_spawn_subagent_waits_for_child_completion() {
    let fixture = setup_spawn_fixture(
        "spawn_subagent_foreground",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let parent_deadline = fixture.parent_deadline;
    let agent_did = fixture.agent_did.clone();
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child prompt",
        "deadline": (parent_deadline - chrono::Duration::minutes(1)).to_rfc3339()
    })
    .to_string();

    let hook_for_wait = hook.clone();
    let args_for_wait = args.clone();
    let wait_handle = tokio::spawn(async move {
        hook_for_wait.on_tool_call("spawn_subagent",
            Some("model-call-fg".to_string()),
            "internal-spawn-fg",
            &args_for_wait,
        )
        .await
    });

    let child = wait_for_child_request_for_tool(db.node.as_ref(), "internal-spawn-fg").await;
    persist_child_completion(
        db.node.as_ref(),
        &agent_did,
        &child.request_id,
        &child.session_id,
        "foreground final answer",
    )
    .await;

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("foreground wait should complete")
        .expect("foreground task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["final_response"], "foreground final answer");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-fg").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("completed"));
    assert_eq!(tool.result.as_deref(), Some("foreground final answer"));
}

#[tokio::test]
async fn foreground_spawn_subagent_parent_deadline_marks_bridge_dead() {
    let parent_deadline = chrono::Utc::now() + chrono::Duration::milliseconds(250);
    let fixture = setup_spawn_fixture_with_flags_and_deadline(
        "foreground_spawn_deadline",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
        true,
        parent_deadline,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child that will exceed parent deadline"
    })
    .to_string();

    let action = hook.on_tool_call("spawn_subagent",
        Some("model-call-fg-deadline".to_string()),
        "internal-spawn-fg-deadline",
        &args,
    )
    .await;
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], false);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "dead");
    assert!(result["error"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("parent request deadline exceeded")));

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-fg-deadline").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
}

#[tokio::test]
async fn foreground_spawn_subagent_cancellation_cascades_to_child_and_unblocks_wait() {
    let fixture =
        setup_spawn_fixture("foreground_spawn_cancel", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child that will be cancelled"
    })
    .to_string();

    let hook_for_wait = hook.clone();
    let args_for_wait = args.clone();
    let wait_handle = tokio::spawn(async move {
        hook_for_wait.on_tool_call("spawn_subagent",
            Some("model-call-fg-cancel".to_string()),
            "internal-spawn-fg-cancel",
            &args_for_wait,
        )
        .await
    });

    let child = wait_for_child_request_for_tool(db.node.as_ref(), "internal-spawn-fg-cancel").await;
    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-spawn-fg-cancel")
            .await
            .unwrap()
            .expect("foreground bridge should be persisted");
    lifecycle
        .cancel_during_run(CancelCause::Interrupted)
        .await
        .unwrap();
    let intent = lifecycle
        .bridge_cancel_cascade()
        .await
        .unwrap()
        .expect("foreground bridge should return cascade intent");
    assert_eq!(intent.child_request_id, child.request_id);
    interrupt_request(db.node.as_ref(), &intent.child_request_id)
        .await
        .unwrap();

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("foreground wait should unblock after cancellation")
        .expect("foreground task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], false);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "interrupted");

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-fg-cancel").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("cancelled"));
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child.request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_some(),
        "cascade cancellation should latch interrupt_requested_at on the child"
    );
}

#[tokio::test]
async fn foreground_spawn_subagent_user_backgrounding_returns_background_receipt() {
    let fixture = setup_spawn_fixture(
        "foreground_spawn_backgrounded",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child that will be backgrounded"
    })
    .to_string();

    let hook_for_wait = hook.clone();
    let args_for_wait = args.clone();
    let wait_handle = tokio::spawn(async move {
        hook_for_wait.on_tool_call("spawn_subagent",
            Some("model-call-fg-backgrounded".to_string()),
            "internal-spawn-fg-backgrounded",
            &args_for_wait,
        )
        .await
    });

    let child =
        wait_for_child_request_for_tool(db.node.as_ref(), "internal-spawn-fg-backgrounded").await;
    let mut lifecycle = ToolCallLifecycle::load(
        db.node.clone(),
        &session_id,
        "internal-spawn-fg-backgrounded",
    )
    .await
    .unwrap()
    .expect("foreground bridge should be persisted");
    lifecycle.background().await.unwrap();

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("foreground wait should unblock after backgrounding")
        .expect("foreground task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "background");
    assert_eq!(result["status"], "running");
    assert_eq!(result["backgrounded"], true);
    assert_eq!(result["child_request_id"], child.request_id);

    let tool = fetch_tool_call(
        db.node.as_ref(),
        &session_id,
        "internal-spawn-fg-backgrounded",
    )
    .await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
}

#[tokio::test]
async fn foreground_spawn_subagent_maps_child_terminal_failures() {
    let cases = [
        ("failed", "failed", "failed", Some("child failed reason")),
        ("dead", "dead", "failed", None),
        ("interrupted", "interrupted", "cancelled", None),
        ("superseded", "superseded", "failed", None),
    ];

    for (child_state, expected_status, expected_tool_state, failure_reason) in cases {
        let test_name = format!("foreground_spawn_terminal_{child_state}");
        let internal_call_id = format!("internal-spawn-terminal-{child_state}");
        let fixture = setup_spawn_fixture(&test_name, vec![CHILD_BEHAVIOR_ID], 0, true).await;
        let db = &fixture.db;
        let hook = fixture.hook.clone();
        let session_id = fixture.session_id.clone();
        let args = json!({
            "name": CHILD_BEHAVIOR_ID,
            "prompt": format!("foreground child terminal {child_state}")
        })
        .to_string();

        let hook_for_wait = hook.clone();
        let args_for_wait = args.clone();
        let internal_call_id_for_wait = internal_call_id.clone();
        let wait_handle = tokio::spawn(async move {
            hook_for_wait.on_tool_call("spawn_subagent",
                Some(format!("model-call-{child_state}")),
                &internal_call_id_for_wait,
                &args_for_wait,
            )
            .await
        });

        let child = wait_for_child_request_for_tool(db.node.as_ref(), &internal_call_id).await;
        persist_child_terminal(
            db.node.as_ref(),
            &child.request_id,
            child_state,
            failure_reason,
        )
        .await;

        let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
            .await
            .expect("foreground wait should complete after child terminal")
            .expect("foreground task should not panic");
        let result = skip_reason_json(action);
        assert_eq!(result["ok"], false);
        assert_eq!(result["await_mode"], "foreground");
        assert_eq!(result["status"], expected_status);
        if let Some(reason) = failure_reason {
            assert_eq!(result["error"]["reason"], reason);
            assert_eq!(result["error"]["failure_class"], "external");
        }

        let tool = fetch_tool_call(db.node.as_ref(), &session_id, &internal_call_id).await;
        assert_eq!(
            tool.lifecycle_state.as_deref(),
            Some(expected_tool_state),
            "unexpected tool state for child terminal {child_state}"
        );
        if let Some(reason) = failure_reason {
            assert_eq!(tool.result.as_deref(), Some(reason));
            assert_eq!(tool.tool_failure_class.as_deref(), Some("external"));
        }
    }
}
