use super::*;

#[tokio::test]
async fn wait_subagent_waits_on_existing_bridge_without_lifecycle_row() {
    let fixture = setup_spawn_fixture(
        "wait_subagent_existing_bridge",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let agent_did = fixture.agent_did.clone();
    let spawn_args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "background child for wait_subagent",
        "await_mode": "background"
    })
    .to_string();

    let spawn_action = hook.on_tool_call("spawn_subagent",
        Some("model-call-wait-spawn".to_string()),
        "internal-wait-spawn",
        &spawn_args,
    )
    .await;
    let spawn_receipt = skip_reason_json(spawn_action);
    assert_eq!(spawn_receipt["ok"], true);
    assert_eq!(spawn_receipt["await_mode"], "background");
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    // Spawn convergence (#377): resolve the child session id from the DB once
    // SubagentSource has materialized the child.
    let child_session_id = wait_for_child_session_id(db.node.as_ref(), &child_request_id).await;

    let hook_for_wait = hook.clone();
    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    let wait_handle = tokio::spawn(async move {
        hook_for_wait.on_tool_call("wait_subagent",
            Some("model-call-wait".to_string()),
            "internal-wait-tool",
            &wait_args,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    let foregrounded_bridge =
        fetch_tool_call(db.node.as_ref(), &session_id, "internal-wait-spawn").await;
    assert_eq!(
        foregrounded_bridge.await_mode.as_deref(),
        Some("foreground")
    );
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );

    persist_child_completion(
        db.node.as_ref(),
        &agent_did,
        &child_request_id,
        &child_session_id,
        "wait_subagent final answer",
    )
    .await;

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("wait_subagent should complete after child completion")
        .expect("wait_subagent task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["final_response"], "wait_subagent final answer");

    let completed_bridge =
        fetch_tool_call(db.node.as_ref(), &session_id, "internal-wait-spawn").await;
    assert_eq!(
        completed_bridge.lifecycle_state.as_deref(),
        Some("completed")
    );
    assert_eq!(
        completed_bridge.result.as_deref(),
        Some("wait_subagent final answer")
    );
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );
}

#[tokio::test]
async fn wait_subagent_maps_child_terminal_failures_without_lifecycle_row() {
    let cases = [
        (
            "failed",
            "failed",
            "failed",
            Some("child failed reason"),
            "child failed reason",
        ),
        (
            "dead",
            "dead",
            "failed",
            None,
            "child request reached terminal state dead",
        ),
        (
            "interrupted",
            "interrupted",
            "cancelled",
            None,
            "child request was interrupted",
        ),
        (
            "superseded",
            "superseded",
            "failed",
            None,
            "child request was superseded",
        ),
    ];

    for (
        child_state,
        expected_status,
        expected_tool_state,
        failure_reason,
        expected_error_reason,
    ) in cases
    {
        let test_name = format!("wait_subagent_terminal_{child_state}");
        let internal_call_id = format!("internal-wait-terminal-spawn-{child_state}");
        let fixture = setup_spawn_fixture(&test_name, vec![CHILD_BEHAVIOR_ID], 0, true).await;
        let db = &fixture.db;
        let hook = fixture.hook.clone();
        let session_id = fixture.session_id.clone();
        let spawn_args = json!({
            "name": CHILD_BEHAVIOR_ID,
            "prompt": format!("background child terminal {child_state}"),
            "await_mode": "background"
        })
        .to_string();

        let spawn_action = hook.on_tool_call("spawn_subagent",
            Some(format!("model-call-wait-terminal-spawn-{child_state}")),
            &internal_call_id,
            &spawn_args,
        )
        .await;
        let spawn_receipt = skip_reason_json(spawn_action);
        assert_eq!(spawn_receipt["ok"], true);
        assert_eq!(spawn_receipt["await_mode"], "background");
        let child_request_id = spawn_receipt["child_request_id"]
            .as_str()
            .expect("child_request_id")
            .to_string();
        let background_bridge =
            fetch_tool_call(db.node.as_ref(), &session_id, &internal_call_id).await;
        assert_eq!(
            background_bridge.await_mode.as_deref(),
            Some("background"),
            "spawn_subagent should persist a background bridge before wait_subagent starts"
        );
        assert_eq!(
            background_bridge.lifecycle_state.as_deref(),
            Some("running")
        );

        // Wait for SubagentSource to materialize the child before invoking
        // wait_subagent (#377): the child is created asynchronously now.
        wait_for_child_session_id(db.node.as_ref(), &child_request_id).await;

        let hook_for_wait = hook.clone();
        let wait_args = json!({ "child_request_id": child_request_id }).to_string();
        let wait_handle = tokio::spawn(async move {
            hook_for_wait.on_tool_call("wait_subagent",
                Some(format!("model-call-wait-terminal-{child_state}")),
                "internal-wait-terminal",
                &wait_args,
            )
            .await
        });

        let foregrounded_bridge = wait_for_tool_call_await_mode(
            db.node.as_ref(),
            &session_id,
            &internal_call_id,
            "foreground",
        )
        .await;
        assert_eq!(
            foregrounded_bridge.lifecycle_state.as_deref(),
            Some("running")
        );

        persist_child_terminal(
            db.node.as_ref(),
            &child_request_id,
            child_state,
            failure_reason,
        )
        .await;

        let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
            .await
            .expect("wait_subagent should complete after child terminal")
            .expect("wait_subagent task should not panic");
        let result = skip_reason_json(action);
        assert_eq!(result["ok"], false);
        assert_eq!(result["await_mode"], "foreground");
        assert_eq!(result["status"], expected_status);
        assert_eq!(result["error"]["reason"], expected_error_reason);
        assert_eq!(result["error"]["failure_class"], "external");

        let bridge = fetch_tool_call(db.node.as_ref(), &session_id, &internal_call_id).await;
        assert_eq!(
            bridge.lifecycle_state.as_deref(),
            Some(expected_tool_state),
            "unexpected bridge state for child terminal {child_state}"
        );
        if let Some(reason) = failure_reason {
            assert_eq!(bridge.result.as_deref(), Some(reason));
            assert_eq!(bridge.tool_failure_class.as_deref(), Some("external"));
        }
        assert_eq!(
            count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
            0
        );
    }
}

#[tokio::test]
async fn wait_subagent_rejects_unlinked_child_without_lifecycle_row() {
    let fixture = setup_spawn_fixture(
        "wait_subagent_unlinked_child",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let wait_args = json!({ "child_request_id": "not-this-parents-child" }).to_string();

    let action = hook.on_tool_call("wait_subagent",
        Some("model-call-wait-denied".to_string()),
        "internal-wait-denied",
        &wait_args,
    )
    .await;
    let error = skip_reason_json(action);
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "service_unavailable");
    assert_eq!(error["tool_name"], "wait_subagent");
    assert_eq!(error["path"], "/child_request_id");
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );
}

#[tokio::test]
async fn wait_subagent_from_resumed_hook_cascades_parent_interrupt() {
    let fixture = setup_spawn_fixture(
        "wait_subagent_resumed_interrupt",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let request_id = fixture.request_id.clone();
    let parent_deadline = fixture.parent_deadline;
    let agent_did = fixture.agent_did.clone();
    let spawn_args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "background child for resumed wait cancellation",
        "await_mode": "background"
    })
    .to_string();

    let spawn_action = hook.on_tool_call("spawn_subagent",
        Some("model-call-wait-resume-spawn".to_string()),
        "internal-wait-resume-spawn",
        &spawn_args,
    )
    .await;
    let spawn_receipt = skip_reason_json(spawn_action);
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    // Wait for SubagentSource to materialize the child (#377).
    wait_for_child_session_id(db.node.as_ref(), &child_request_id).await;

    let resumed_hook = DefraSessionHook::resume_or_create_with_identity_policy(
        db.node.clone(),
        &session_id,
        PARENT_BEHAVIOR_ID,
        &agent_did,
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    resumed_hook
        .set_active_request_id(Some(request_id.clone()))
        .await;
    resumed_hook
        .set_request_deadline_at(Some(parent_deadline))
        .await;

    let hook_for_wait = resumed_hook.clone();
    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    let wait_handle = tokio::spawn(async move {
        hook_for_wait.on_tool_call("wait_subagent",
            Some("model-call-wait-resume".to_string()),
            "internal-wait-resume",
            &wait_args,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    let foregrounded_bridge =
        fetch_tool_call(db.node.as_ref(), &session_id, "internal-wait-resume-spawn").await;
    assert_eq!(
        foregrounded_bridge.await_mode.as_deref(),
        Some("foreground")
    );

    interrupt_request(db.node.as_ref(), &request_id)
        .await
        .unwrap();

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("resumed wait_subagent should unblock after parent interrupt")
        .expect("resumed wait_subagent task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], false);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "interrupted");

    let cancelled_bridge =
        fetch_tool_call(db.node.as_ref(), &session_id, "internal-wait-resume-spawn").await;
    assert_eq!(
        cancelled_bridge.lifecycle_state.as_deref(),
        Some("cancelled")
    );
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_some(),
        "wait_subagent cancellation should cascade to the child request"
    );
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );
}

#[tokio::test]
async fn wait_subagent_returns_background_receipt_when_bridge_is_backgrounded() {
    let fixture = setup_spawn_fixture(
        "wait_subagent_backgrounded",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let hook = fixture.hook.clone();
    let session_id = fixture.session_id.clone();
    let spawn_args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "background child for wait backgrounding",
        "await_mode": "background"
    })
    .to_string();

    let spawn_action = hook.on_tool_call("spawn_subagent",
        Some("model-call-wait-bg-spawn".to_string()),
        "internal-wait-bg-spawn",
        &spawn_args,
    )
    .await;
    let spawn_receipt = skip_reason_json(spawn_action);
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    // Wait for SubagentSource to materialize the child (#377).
    wait_for_child_session_id(db.node.as_ref(), &child_request_id).await;

    let hook_for_wait = hook.clone();
    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    let wait_handle = tokio::spawn(async move {
        hook_for_wait.on_tool_call("wait_subagent",
            Some("model-call-wait-bg".to_string()),
            "internal-wait-bg",
            &wait_args,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-wait-bg-spawn")
            .await
            .unwrap()
            .expect("wait_subagent should foreground the original bridge");
    lifecycle.background().await.unwrap();

    let action = tokio::time::timeout(Duration::from_secs(5), wait_handle)
        .await
        .expect("wait_subagent should unblock after bridge backgrounding")
        .expect("wait_subagent task should not panic");
    let result = skip_reason_json(action);
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "background");
    assert_eq!(result["status"], "running");
    assert_eq!(result["backgrounded"], true);
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        0
    );
}
