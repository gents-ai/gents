use super::*;

#[tokio::test]
async fn shared_cancel_subagent_controls_prior_turn_only_for_the_same_session_principal() {
    let fixture =
        setup_spawn_fixture("shared_cancel_prior_turn", vec![CHILD_BEHAVIOR_ID], 0, true).await;
    let spawn_args = json!({"name": CHILD_BEHAVIOR_ID,
        "prompt": "background child", "await_mode": "background"})
    .to_string();
    let runtime = run_canonical_spawn_turn(&fixture, "shared-cancel-bridge", &spawn_args).await;
    let bridge = fetch_tool_call(
        &fixture.db.node,
        &fixture.session_id,
        "shared-cancel-bridge",
    )
    .await;
    let receipt = persisted_tool_result_json(&bridge);
    let child = receipt["child_request_id"].as_str().unwrap();
    wait_for_child_session_id(fixture.db.node.as_ref(), child).await;
    let processing_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let child_row = fetch_child_request(fixture.db.node.as_ref(), child).await;
        if child_row.lifecycle_state == Some(RequestLifecycleState::Processing) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < processing_deadline,
            "real child did not enter processing before cancellation; state={:?}",
            child_row.lifecycle_state
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    create_parent_request_with_extra_fields(
        fixture.db.node.as_ref(),
        &fixture.agent_did,
        "foreign-session-principal",
        &fixture.session_id,
        0,
        fixture.parent_deadline,
        "requester_did: \"did:foreign:requester\"",
    )
    .await;
    let denied = gents::cancel_session_subagent(
        fixture.db.node.clone(),
        "foreign-session-principal",
        child,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        denied,
        gents::CancelSubagentOutcome::Unavailable { .. }
            | gents::CancelSubagentOutcome::NotAuthorized
    ));
    assert_eq!(
        fetch_tool_call(
            &fixture.db.node,
            &fixture.session_id,
            "shared-cancel-bridge"
        )
        .await
        .lifecycle_state
        .as_deref(),
        Some("running")
    );
    assert!(
        fetch_interrupt_requested_at(fixture.db.node.as_ref(), child)
            .await
            .unwrap()
            .is_none()
    );

    enqueue_local_accepted_request(
        &fixture.db,
        PARENT_BEHAVIOR_ID,
        "later-session-turn",
        &fixture.session_id,
        "authorize later-turn cancellation",
    )
    .await;
    let cancelled = gents::cancel_session_subagent(
        fixture.db.node.clone(),
        "later-session-turn",
        child,
        Some("later turn cancellation"),
    )
    .await
    .unwrap();
    let gents::CancelSubagentOutcome::Cancelled(receipt) = cancelled else {
        panic!("expected cancelled outcome, got {cancelled:?}");
    };
    assert_eq!(receipt.child_request_id, child);
    assert_eq!(receipt.parent_session_id, fixture.session_id);
    assert!(
        fetch_interrupt_requested_at(fixture.db.node.as_ref(), child)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        fetch_tool_call(
            &fixture.db.node,
            &fixture.session_id,
            "shared-cancel-bridge"
        )
        .await
        .lifecycle_state
        .as_deref(),
        Some("cancelled")
    );

    wait_for_request_terminal(fixture.db.node.as_ref(), child, "interrupted").await;
    assert!(matches!(
        gents::cancel_session_subagent(fixture.db.node.clone(), "later-session-turn", child, None)
            .await
            .unwrap(),
        gents::CancelSubagentOutcome::AlreadyTerminal(_)
    ));
    runtime.shutdown().await;
}

#[tokio::test]
async fn cancel_subagent_rejects_unlinked_child_without_lifecycle_row() {
    let fixture = setup_spawn_fixture(
        "cancel_subagent_unlinked_child",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    let db = &fixture.db;
    let session_id = fixture.session_id.clone();
    let cancel_args = json!({ "child_request_id": "not-this-parents-child" }).to_string();

    run_canonical_tool_turn(
        &fixture,
        "internal-cancel-denied",
        "cancel_subagent",
        &cancel_args,
    )
    .await;
    let error = canonical_tool_payload_json(&fixture, "internal-cancel-denied").await;
    assert_eq!(error["ok"], false);
    assert_eq!(error["failure_class"], "service_unavailable");
    assert_eq!(error["tool_name"], "cancel_subagent");
    assert_eq!(error["path"], "/child_request_id");
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), &session_id, "cancel_subagent").await,
        1
    );
}

#[tokio::test]
async fn cancel_subagent_explains_unmaterialized_child_bridge() {
    let fixture = setup_spawn_fixture(
        "cancel_subagent_unmaterialized",
        vec![CHILD_BEHAVIOR_ID],
        0,
        true,
    )
    .await;
    const REMOTE_DID: &str = "did:test:cancel-unmaterialized";
    configure_remote_spawn_target(&fixture, REMOTE_DID).await;
    let bridge_tool_call_id = "cancel-unmat-bridge";
    let spawn_args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "cross-deployment work",
        "await_mode": "background"
    })
    .to_string();
    run_canonical_spawn_turn(&fixture, bridge_tool_call_id, &spawn_args).await;
    let bridge = fetch_tool_call(&fixture.db.node, &fixture.session_id, bridge_tool_call_id).await;
    let child_request_id = bridge.child_request_id.expect("remote child request id");
    let cancel_args = json!({ "child_request_id": child_request_id }).to_string();
    run_canonical_followup_tool_turn(
        &fixture,
        "cancel-unmaterialized-followup",
        "internal-cancel-unmat",
        "cancel_subagent",
        &cancel_args,
    )
    .await;
    let error = canonical_tool_payload_json(&fixture, "internal-cancel-unmat").await;
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
