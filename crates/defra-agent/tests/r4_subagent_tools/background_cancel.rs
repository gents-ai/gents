use super::*;

#[tokio::test]
async fn spawn_subagent_background_materializes_child_and_bridge() {
    let (db, hook, session_id, request_id, parent_deadline) = setup_spawn_fixture(
        "spawn_subagent_background",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let child_deadline = parent_deadline - chrono::Duration::minutes(1);
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "child prompt from spawn tool",
        "await_mode": "background",
        "deadline": child_deadline.to_rfc3339()
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-1".to_string()),
        "internal-spawn-1",
        &args,
    )
    .await;
    let receipt = skip_reason_json(action);
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["behavior_id"], CHILD_BEHAVIOR_ID);
    assert_eq!(receipt["await_mode"], "background");
    assert_eq!(receipt["status"], "running");
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    let child_session_id = receipt["child_session_id"]
        .as_str()
        .expect("child_session_id")
        .to_string();

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-spawn-1").await;
    assert_eq!(tool.request_id.as_deref(), Some(request_id.as_str()));
    assert_eq!(tool.tool_name.as_deref(), Some("spawn_subagent"));
    assert_eq!(tool.args.as_deref(), Some(args.as_str()));
    assert_eq!(tool.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
    assert_eq!(tool.cancel_policy.as_deref(), Some("cascade"));
    assert_eq!(
        tool.child_request_id.as_deref(),
        Some(child_request_id.as_str())
    );
    let unclaimed_deadline_at = tool
        .unclaimed_deadline_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc))
        .expect("background spawn should set unclaimed_deadline_at");
    let delta = (unclaimed_deadline_at - chrono::Utc::now()).num_seconds();
    assert!(
        (45..=75).contains(&delta),
        "unclaimed_deadline_at should be about 60s out, got {delta}s"
    );

    let child = fetch_child_request(db.node.as_ref(), &child_request_id).await;
    assert_eq!(child.request_id, child_request_id);
    assert_eq!(child.session_id, child_session_id);
    assert_eq!(child.behavior_id, CHILD_BEHAVIOR_ID);
    assert_eq!(child.content, "child prompt from spawn tool");
    assert_eq!(child.lifecycle_state.as_deref(), Some("pending"));
    assert_eq!(child.subagent_depth, Some(1));
    assert_eq!(
        child.deadline.as_deref(),
        Some(child_deadline.to_rfc3339().as_str())
    );
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(request_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some("internal-spawn-1")
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some("internal-spawn-1")
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("subagent"));
}

#[tokio::test]
async fn background_cross_deployment_spawn_writes_bridge_without_local_child() {
    let (db, hook, session_id, request_id, _parent_deadline) = setup_spawn_fixture(
        "spawn_subagent_cross_deployment_background",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    upsert_agent_behavior(
        db.node.as_ref(),
        &AgentBehaviorDocument {
            behavior_id: CHILD_BEHAVIOR_ID.to_string(),
            agent_did: "did:defra-agent:r5-remote-child".to_string(),
            display_name: Some("R5 remote child".to_string()),
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            created_at: Some("2026-05-14T00:00:00Z".to_string()),
        },
    )
    .await
    .unwrap();

    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "remote child prompt from spawn tool",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-r5-remote-spawn".to_string()),
        "internal-r5-remote-spawn",
        &args,
    )
    .await;
    let receipt = skip_reason_json(action);
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["behavior_id"], CHILD_BEHAVIOR_ID);
    assert_eq!(receipt["await_mode"], "background");
    assert_eq!(receipt["status"], "running");
    assert!(
        receipt["child_session_id"].is_null(),
        "A does not know the child session until B claims and replicates the AgentRequest"
    );
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-r5-remote-spawn").await;
    assert_eq!(tool.request_id.as_deref(), Some(request_id.as_str()));
    assert_eq!(tool.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
    assert_eq!(tool.cancel_policy.as_deref(), Some("cascade"));
    assert_eq!(
        tool.child_request_id.as_deref(),
        Some(child_request_id.as_str())
    );
    assert!(
        tool.unclaimed_deadline_at.is_some(),
        "cross-deployment bridge keeps the unclaimed-spawn deadline"
    );
    assert!(
        fetch_child_request_optional(db.node.as_ref(), &child_request_id)
            .await
            .is_none(),
        "A must not materialize the B-owned child request"
    );
}

#[tokio::test]
async fn cross_deployment_cancel_writes_cascade_intent_on_bridge() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "cross_deployment_cancel_intent",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "remote child prompt",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-xdep-cancel".to_string()),
        "internal-xdep-cancel",
        &args,
    )
    .await;
    let receipt = skip_reason_json(action);
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    override_child_agent_did(
        db.node.as_ref(),
        &child_request_id,
        "did:defra-agent:remote",
    )
    .await;

    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-xdep-cancel")
            .await
            .unwrap()
            .expect("bridge should be persisted");
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(AGENT_DID)
        .await
        .unwrap()
        .expect("cascade dispatch");
    assert!(
        matches!(dispatch, CascadeDispatch::RemoteIntentWritten),
        "remote child should write bridge intent"
    );

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-xdep-cancel").await;
    assert!(
        tool.cancel_cascade_intent_at.is_some(),
        "remote branch must set cancel_cascade_intent_at"
    );
    assert_eq!(tool.cancel_pending_remote_ack, Some(true));
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_none(),
        "remote branch must not write child interrupt_requested_at"
    );
}

#[tokio::test]
async fn single_deployment_cancel_dispatch_still_interrupts_child() {
    let (db, hook, session_id, _request_id, _parent_deadline) = setup_spawn_fixture(
        "single_deployment_cancel_interrupt",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let args = json!({
        "behavior_id": CHILD_BEHAVIOR_ID,
        "prompt": "local child prompt",
        "await_mode": "background"
    })
    .to_string();

    let action = PromptHook::<TestModel>::on_tool_call(
        &hook,
        "spawn_subagent",
        Some("model-call-local-cancel".to_string()),
        "internal-local-cancel",
        &args,
    )
    .await;
    let receipt = skip_reason_json(action);
    let child_request_id = receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();

    let mut lifecycle =
        ToolCallLifecycle::load(db.node.clone(), &session_id, "internal-local-cancel")
            .await
            .unwrap()
            .expect("bridge should be persisted");
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(AGENT_DID)
        .await
        .unwrap()
        .expect("cascade dispatch");
    let CascadeDispatch::Local(intent) = dispatch else {
        panic!("local child should use local cascade dispatch");
    };
    assert_eq!(intent.child_request_id, child_request_id);
    interrupt_request(db.node.as_ref(), &intent.child_request_id)
        .await
        .unwrap();

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "internal-local-cancel").await;
    assert!(
        tool.cancel_cascade_intent_at.is_none(),
        "local branch must not set bridge cancel intent"
    );
    let child_interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &child_request_id)
        .await
        .unwrap();
    assert!(
        child_interrupt.is_some(),
        "local branch should still interrupt the child"
    );
}
