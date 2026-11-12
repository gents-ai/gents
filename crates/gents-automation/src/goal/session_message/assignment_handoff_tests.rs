use super::SessionGoals;
use gents_protocol::tool_call::AwaitMode;
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::background::completion::session_message::{
    settle_running_session_message_rows_with_access, turn_assignment_handoff,
};
use gents_toolcalls::call::lifecycle::admission_fixture::{
    caused_session_id, recovery_identities, session_message_notifications, tool_call_row,
    PublishedSessionMessage,
};
use gents_toolcalls::call::lifecycle::ToolCallLifecycle;

#[tokio::test]
async fn completed_turn_with_live_goal_hands_off_a_non_complete_reason() {
    let PublishedSessionMessage {
        admission,
        caused_request_id,
        caused_request_doc_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "handoff-live-goal".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .expect("publish accepted session message");
    let node = &admission.node;
    crate::lifecycle_hooks::attach(node).unwrap();
    let did = &admission.node_did;
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    gents_inference::test_support::install_test_agent(node, did, "general").await;
    // The caused session carries an active Goal: a finished turn with a
    // live assignment is not assignment success.
    crate::goal::set_goal_from_access(
        &admission.tool.access().unwrap(),
        did,
        &caused_session_id(&admission.tool.access().unwrap(), &caused_request_doc_id).await,
        Some("hold"),
        Some(gents_protocol::goal_status::GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("persist an active Goal in the caused session");
    bind_caused_assignment(
        &admission.tool.access().unwrap(),
        did,
        &caused_request_doc_id,
    )
    .await;
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "child done",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;

    let settled = settle_running_session_message_rows_with_access(
        node,
        &admission.tool.access().unwrap(),
        &SessionGoals,
        did,
    )
    .await
    .unwrap();
    assert_eq!(settled, 1, "the caused terminal settles the row");
    // One settle call persists the suffix, publishes the notice and
    // advances the row: the converged form is `completed` with the
    // reason rendered. The durable pre-delivery suffix is pinned in the
    // crash-before-notice regression, where the side effect is skipped.
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["lifecycle_state"], "completed");
    assert_eq!(row["status"], "completed");

    let notifications = session_message_notifications(
        &admission.tool.access().unwrap(),
        did,
        admission.tool.session_id(),
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].contains("<reason>assignment_active</reason>"),
        "first notice names the live assignment: {}",
        notifications[0]
    );

    // Replay: the side effects already converged; the redrive republishes
    // nothing and the notice stays byte-identical.
    let redrive =
        gents_toolcalls::call::lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
            node, did, &recovery_identities(&admission),
        )
        .await
        .unwrap();
    assert_eq!(
        redrive.side_effects_converged, 0,
        "side effects already converged; the replay republishes nothing"
    );
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&admission.path).expect("remove fixture");
}

/// No-goal and complete-goal sessions are unmistakably distinguished from
/// each other and from a live assignment.
#[tokio::test]
async fn completed_turn_without_a_goal_hands_off_assignment_absent() {
    let PublishedSessionMessage {
        admission,
        caused_request_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "handoff-no-goal".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .expect("publish accepted session message");
    let node = &admission.node;
    crate::lifecycle_hooks::attach(node).unwrap();
    let did = &admission.node_did;
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    gents_inference::test_support::install_test_agent(node, did, "general").await;
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "child done",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;

    let settled = settle_running_session_message_rows_with_access(
        node,
        &admission.tool.access().unwrap(),
        &SessionGoals,
        did,
    )
    .await
    .unwrap();
    assert_eq!(settled, 1);
    // One settle call persists the suffix, publishes the notice and
    // advances the row; the notice is where the absent-assignment
    // distinction lives (a no-goal session never reads as success).
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["status"], "completed");
    let notifications = session_message_notifications(
        &admission.tool.access().unwrap(),
        did,
        admission.tool.session_id(),
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].contains("<reason>assignment_absent</reason>"),
        "first notice names the absent assignment: {}",
        notifications[0]
    );
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&admission.path).expect("remove fixture");
}

#[tokio::test]
async fn completed_turn_with_complete_goal_is_the_reasonless_success_form() {
    let PublishedSessionMessage {
        admission,
        caused_request_id,
        caused_request_doc_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "handoff-complete-goal".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .expect("publish accepted session message");
    let node = &admission.node;
    crate::lifecycle_hooks::attach(node).unwrap();
    let did = &admission.node_did;
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    gents_inference::test_support::install_test_agent(node, did, "general").await;
    crate::goal::set_goal_from_access(
        &admission.tool.access().unwrap(),
        did,
        &caused_session_id(&admission.tool.access().unwrap(), &caused_request_doc_id).await,
        Some("done"),
        Some(gents_protocol::goal_status::GoalStatus::Complete),
        None,
        None,
    )
    .await
    .expect("persist a complete Goal in the caused session");
    bind_caused_assignment(
        &admission.tool.access().unwrap(),
        did,
        &caused_request_doc_id,
    )
    .await;
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "child done",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;

    let settled = settle_running_session_message_rows_with_access(
        node,
        &admission.tool.access().unwrap(),
        &SessionGoals,
        did,
    )
    .await
    .unwrap();
    assert_eq!(settled, 1);
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["status"], "completed");
    let notifications = session_message_notifications(
        &admission.tool.access().unwrap(),
        did,
        admission.tool.session_id(),
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert!(
        !notifications[0].contains("<reason>"),
        "a complete assignment stays reasonless: {}",
        notifications[0]
    );
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&admission.path).expect("remove fixture");
}

/// The persisted suffix, not live assignment state, is the replay
/// authority: settlement persists `completionPending:<reason>` and a
/// crash before the notice leaves it outstanding; the redrive re-renders
/// the suffix byte-identically and advances the row to `completed`. The
/// replayed reason is invariant across a Goal status change between
/// settlement and redrive (the source snapshot never changes on replay).
#[tokio::test]
async fn crash_before_notice_redrive_renders_the_persisted_assignment_reason() {
    let PublishedSessionMessage {
        admission,
        caused_request_id,
        caused_request_doc_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "handoff-redrive".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .expect("publish accepted session message");
    let node = &admission.node;
    crate::lifecycle_hooks::attach(node).unwrap();
    let did = &admission.node_did;
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    let session_id = admission.tool.session_id().to_owned();
    gents_inference::test_support::install_test_agent(node, did, "general").await;
    let goal_session_id =
        caused_session_id(&admission.tool.access().unwrap(), &caused_request_doc_id).await;
    crate::goal::set_goal_from_access(
        &admission.tool.access().unwrap(),
        did,
        &goal_session_id,
        Some("hold"),
        Some(gents_protocol::goal_status::GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("persist an active Goal in the caused session");
    bind_caused_assignment(
        &admission.tool.access().unwrap(),
        did,
        &caused_request_doc_id,
    )
    .await;
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "child done",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;

    // Settle the lifecycle row only: a crash before the notice side
    // effect leaves `completionPending:assignment_active` outstanding.
    // The row is owned by the invoking Agent; rehydrate it as that Agent
    // exactly as settlement does.
    let agent_access = admission.tool.access().unwrap();
    let requester = agent_access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ requester_did }} }}"#,
            escape_graphql_string(&tool_doc_id)
        ))
        .await
        .unwrap()["data"]["AgentToolCall"][0]["requester_did"]
        .as_str()
        .map(str::to_owned);
    let mut lifecycle = agent_access
        .transact_readonly("test.session_message.rehydrate", |txn| {
            let (node, tool_doc_id, session_id, requester) =
                (node.clone(), &tool_doc_id, &session_id, &requester);
            Box::pin(async move {
                let Some(mut lifecycle) = ToolCallLifecycle::load_by_doc_id_in_txn(
                    txn,
                    node,
                    tool_doc_id,
                    did,
                    session_id,
                    requester.as_deref(),
                )
                .await?
                else {
                    return Ok::<_, anyhow::Error>(None);
                };
                lifecycle.bind_agent_signer(txn.managed_agent_signer())?;
                Ok(Some(lifecycle))
            })
        })
        .await
        .unwrap()
        .expect("rehydrated session-message row");
    let caused = match gents_toolcalls::background::caused::observe_caused_request(node, &lifecycle)
        .await
        .unwrap()
    {
        gents_toolcalls::background::caused::CausedObservation::Bound(caused) => caused,
        gents_toolcalls::background::caused::CausedObservation::Unbound(reason) => {
            panic!("expected a bound caused request, got Unbound({reason})")
        }
        gents_toolcalls::background::caused::CausedObservation::NotVisible => {
            panic!("expected a bound caused request, got NotVisible")
        }
    };
    let caused_doc_id = caused.doc_id.as_deref().unwrap().to_owned();
    let Some(terminal) = gents_toolcalls::background::status::load_caused_request_terminals(
        &agent_access,
        std::slice::from_ref(&caused_doc_id),
    )
    .await
    .unwrap()
    .remove(&caused_doc_id) else {
        panic!("the completed child is a terminal observation");
    };
    let handoff = turn_assignment_handoff(
        &SessionGoals,
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            lifecycle.actor().clone(),
        ),
        &caused,
        &terminal,
    )
    .await;
    let handoff_reason = handoff.reason();
    assert_eq!(handoff_reason, Some("assignment_active"));
    assert!(lifecycle
        .settle_session_message(&terminal, handoff_reason)
        .await
        .unwrap());
    // Pre-delivery persisted snapshot: the suffix is durable before any
    // notification exists (lifecycle state is terminal; only the notice
    // side effect is outstanding).
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["status"], "completionPending:assignment_active");
    assert!(
        session_message_notifications(&admission.tool.access().unwrap(), did, &session_id)
            .await
            .is_empty(),
        "no notice exists yet: this is the crash-before-notice state"
    );

    // The assignment changes under the replay (the Goal completes), but
    // the redrive must re-render the persisted suffix, never re-observe
    // live state.
    crate::goal::set_goal_from_access(
        &admission.tool.access().unwrap(),
        did,
        &goal_session_id,
        Some("hold"),
        Some(gents_protocol::goal_status::GoalStatus::Complete),
        None,
        None,
    )
    .await
    .expect("the Goal completes after settlement");

    // The redrive re-renders from the row's persisted suffix.
    let report =
        gents_toolcalls::call::lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
            node, did, &recovery_identities(&admission),
        )
        .await
        .unwrap();
    assert_eq!(report.side_effects_converged, 1);
    let notifications =
        session_message_notifications(&admission.tool.access().unwrap(), did, &session_id).await;
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].contains("<reason>assignment_active</reason>"),
        "replay renders the persisted assignment reason, not live state: {}",
        notifications[0]
    );
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["status"], "completed", "side effects converged");
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&admission.path).expect("remove fixture");
}

async fn bind_caused_assignment(
    access: &gents_store::config_client::ConfigAccess,
    did: &str,
    request_doc: &str,
) {
    let session = caused_session_id(access, request_doc).await;
    let goal = access
        .transact_readonly("test.load_caused_goal", |txn| {
            Box::pin(crate::goal::load_canonical_goal_in_txn(txn, did, &session))
        })
        .await
        .unwrap()
        .unwrap();
    access.write("test.bind_caused_assignment",
        &format!("mutation {{update_Goal(docID: \"{}\", input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
            escape_graphql_string(&goal.doc_id), escape_graphql_string(request_doc))).await.unwrap();
}
