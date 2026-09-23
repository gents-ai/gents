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
    let session_id = fixture.session_id.clone();
    let spawn_args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "background child for wait_subagent",
        "await_mode": "background"
    })
    .to_string();

    // Keep one canonical runtime alive across the background spawn and the
    // later accepted wait request. The held child provider stream needs its
    // own inference permit while the parent makes its follow-up provider turn.
    let runtime = run_canonical_spawn_turn_with_child_response(
        &fixture,
        "internal-wait-spawn",
        &spawn_args,
        "wait_subagent final answer",
        2,
    )
    .await;
    let spawned = fetch_tool_call(&db.node, &session_id, "internal-wait-spawn").await;
    let spawn_receipt = persisted_tool_result_json(&spawned);
    assert_eq!(spawn_receipt["ok"], true);
    assert_eq!(spawn_receipt["await_mode"], "background");
    let child_request_id = spawn_receipt["child_request_id"]
        .as_str()
        .expect("child_request_id")
        .to_string();
    wait_for_child_session_id(db.node.as_ref(), &child_request_id).await;
    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    let followup_request_id = "wait-subagent-existing-followup";
    runtime
        .backend
        .enable_dynamic_followups("followup wait prompt");
    runtime.backend.enqueue_response(
        "followup wait prompt",
        StreamResponse::streams(
            "followup wait prompt",
            vec![StreamChunk::tool_call(
                "internal-wait-tool",
                "wait_subagent",
                wait_args.clone(),
            )],
        ),
    );
    runtime.backend.enqueue_response(
        "followup wait prompt",
        StreamResponse::completes("followup wait prompt", ["parent complete"]),
    );
    let valid_until = fixture
        .parent_deadline
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    enqueue_local_accepted_request_until(
        db,
        PARENT_BEHAVIOR_ID,
        followup_request_id,
        &session_id,
        "followup wait prompt",
        Some(&valid_until),
    )
    .await;

    let foregrounded_bridge =
        wait_for_tool_call_await_mode(&db.node, &session_id, "internal-wait-spawn", "foreground")
            .await;
    assert_eq!(
        foregrounded_bridge.await_mode.as_deref(),
        Some("foreground")
    );
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        1,
        "accepted wait control must retain its physical tool row"
    );

    runtime
        .backend
        .release("background child for wait_subagent");
    wait_for_request_terminal(db.node.as_ref(), followup_request_id, "completed").await;
    let result = canonical_tool_payload_json(&fixture, "internal-wait-tool").await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["await_mode"], "foreground");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["final_response"], "wait_subagent final answer");

    let completed_bridge = fetch_tool_call(&db.node, &session_id, "internal-wait-spawn").await;
    assert_eq!(
        completed_bridge.lifecycle_state.as_deref(),
        Some("completed")
    );
    assert_eq!(
        completed_bridge.result.as_deref(),
        spawned.result.as_deref(),
        "the immutable background receipt remains the bridge's native result"
    );
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        1,
        "completed wait control must retain its physical tool row"
    );
    runtime.shutdown().await;
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
    let session_id = fixture.session_id.clone();
    let wait_args = json!({ "child_request_id": "not-this-parents-child" }).to_string();

    run_canonical_tool_turn(
        &fixture,
        "internal-wait-denied",
        "wait_subagent",
        &wait_args,
    )
    .await;
    let error = canonical_tool_payload_json(&fixture, "internal-wait-denied").await;
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "service_unavailable");
    assert_eq!(error["tool_name"], "wait_subagent");
    assert_eq!(error["path"], "/child_request_id");
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "wait_subagent").await,
        1
    );
}

#[tokio::test]
async fn wait_subagent_explains_unmaterialized_child_bridge() {
    let fixture = setup_spawn_fixture(
        "wait_subagent_unmaterialized",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    const REMOTE_DID: &str = "did:test:wait-unmaterialized";
    configure_remote_spawn_target(&fixture, REMOTE_DID).await;
    let bridge_tool_call_id = "wait-unmat-bridge";
    let spawn_args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "cross-deployment work",
        "await_mode": "background"
    })
    .to_string();
    run_canonical_spawn_turn(&fixture, bridge_tool_call_id, &spawn_args).await;
    let bridge = fetch_tool_call(&fixture.db.node, &fixture.session_id, bridge_tool_call_id).await;
    let child_request_id = bridge.child_request_id.expect("remote child request id");
    let wait_args = json!({ "child_request_id": child_request_id }).to_string();
    run_canonical_followup_tool_turn(
        &fixture,
        "wait-unmaterialized-followup",
        "internal-wait-unmat",
        "wait_subagent",
        &wait_args,
    )
    .await;
    let error = canonical_tool_payload_json(&fixture, "internal-wait-unmat").await;
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "service_unavailable");
    assert_eq!(error["retryable"], true);
    let message = error["message"].as_str().expect("message");
    assert!(
        message.contains("has no materialized row yet"),
        "explains materialization: {message}"
    );
    assert!(
        message.contains(bridge_tool_call_id),
        "names the bridge: {message}"
    );
}
