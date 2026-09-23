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
    let session_id = fixture.session_id.clone();
    let parent_deadline = fixture.parent_deadline;
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child prompt",
        "deadline": (parent_deadline - chrono::Duration::minutes(1)).to_rfc3339()
    })
    .to_string();

    run_canonical_foreground_spawn_turn(
        &fixture,
        "internal-spawn-fg",
        &args,
        "foreground child prompt",
        StreamResponse::completes("foreground child prompt", ["foreground final answer"]),
    )
    .await;
    let tool = fetch_tool_call(&db.node, &session_id, "internal-spawn-fg").await;
    let result = canonical_tool_payload_json(&fixture, "internal-spawn-fg").await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["final_response"], "foreground final answer");

    assert_eq!(tool.lifecycle_state.as_deref(), Some("completed"));
    let persisted: serde_json::Value = serde_json::from_str(
        tool.result
            .as_deref()
            .expect("persisted canonical bridge result"),
    )
    .expect("bridge result envelope");
    assert_eq!(persisted, result);
}

/// The owned request deadline fails the parent while its hook sweep takes the
/// licensed `timedOut` bridge transition. It does not invent a terminal child
/// observation or wait beyond the request deadline to synthesize a hook JSON
/// envelope (#1002 defect 2).
#[tokio::test]
async fn foreground_spawn_subagent_parent_deadline_times_out_bridge() {
    let parent_deadline = chrono::Utc::now() + chrono::Duration::seconds(3);
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
    let session_id = fixture.session_id.clone();
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child that will exceed parent deadline"
    })
    .to_string();

    let runtime = boot_canonical_foreground_spawn_turn_with_execution_deadline(
        &fixture,
        "internal-spawn-fg-deadline",
        &args,
        "foreground child that will exceed parent deadline",
        StreamResponse::Stream(StreamScript::paused(
            "foreground child that will exceed parent deadline",
            ["child started"],
        )),
        3,
    )
    .await;
    wait_for_parent_terminal(&fixture, "failed").await;
    let parent = fetch_child_request(db.node.as_ref(), &fixture.request_id).await;
    assert_eq!(parent.lifecycle_state, Some(RequestLifecycleState::Failed));
    assert!(parent
        .failure_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("request deadline exceeded")));

    let tool = fetch_tool_call(&db.node, &session_id, "internal-spawn-fg-deadline").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("timedOut"));
    assert_eq!(tool.cancel_cause.as_deref(), Some("deadline"));
    assert_eq!(tool.tool_failure_class.as_deref(), Some("external"));
    assert!(tool
        .result
        .as_deref()
        .is_some_and(|result| result.contains("tool call deadline exceeded")));
    runtime.shutdown().await;
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
    let session_id = fixture.session_id.clone();
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "foreground child that will be backgrounded"
    })
    .to_string();

    let runtime = boot_canonical_foreground_spawn_turn_with_backend_capacity(
        &fixture,
        "internal-spawn-fg-backgrounded",
        &args,
        "foreground child that will be backgrounded",
        StreamResponse::Stream(StreamScript::paused(
            "foreground child that will be backgrounded",
            ["child started"],
        )),
        2,
    )
    .await;

    let child = wait_for_child_request_for_tool(
        db.node.as_ref(),
        &session_id,
        "internal-spawn-fg-backgrounded",
    )
    .await;
    let mut lifecycle = ToolCallLifecycle::load(
        db.node.clone(),
        &session_id,
        "internal-spawn-fg-backgrounded",
    )
    .await
    .unwrap()
    .expect("foreground bridge should be persisted");
    lifecycle.background().await.unwrap();

    wait_for_parent_terminal(&fixture, "completed").await;
    let result = canonical_tool_payload_json(&fixture, "internal-spawn-fg-backgrounded").await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "background");
    assert_eq!(result["status"], "running");
    assert_eq!(result["backgrounded"], true);
    assert_eq!(result["child_request_id"], child.request_id);

    let tool = fetch_tool_call(&db.node, &session_id, "internal-spawn-fg-backgrounded").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
    runtime.shutdown().await;
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
        let session_id = fixture.session_id.clone();
        let args = json!({
            "name": CHILD_BEHAVIOR_ID,
            "prompt": format!("foreground child terminal {child_state}")
        })
        .to_string();

        let child_prompt = format!("foreground child terminal {child_state}");
        let runtime = boot_canonical_foreground_spawn_turn(
            &fixture,
            &internal_call_id,
            &args,
            &child_prompt,
            StreamResponse::Stream(StreamScript::paused(&child_prompt, ["child started"])),
        )
        .await;

        let child =
            wait_for_child_request_for_tool(db.node.as_ref(), &session_id, &internal_call_id).await;
        persist_child_terminal(
            db.node.as_ref(),
            &child.request_id,
            child_state,
            failure_reason,
        )
        .await;

        wait_for_parent_terminal(&fixture, "completed").await;
        let result = canonical_tool_payload_json(&fixture, &internal_call_id).await;
        assert_eq!(result["ok"], false);
        assert_eq!(result["await_mode"], "foreground");
        assert_eq!(result["status"], expected_status);
        if let Some(reason) = failure_reason {
            assert_eq!(result["error"]["reason"], reason);
            assert_eq!(result["error"]["failure_class"], "external");
        }

        let tool = fetch_tool_call(&db.node, &session_id, &internal_call_id).await;
        assert_eq!(
            tool.lifecycle_state.as_deref(),
            Some(expected_tool_state),
            "unexpected tool state for child terminal {child_state}"
        );
        if let Some(reason) = failure_reason {
            let persisted: serde_json::Value = serde_json::from_str(
                tool.result
                    .as_deref()
                    .expect("persisted canonical failure result"),
            )
            .expect("bridge failure envelope");
            assert_eq!(persisted["error"]["reason"], reason);
            assert_eq!(persisted["error"]["failure_class"], "external");
            assert_eq!(tool.tool_failure_class.as_deref(), Some("external"));
        }
        runtime.shutdown().await;
    }
}
