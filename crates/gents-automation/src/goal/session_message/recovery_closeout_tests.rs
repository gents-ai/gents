use super::SessionGoals;
use gents_protocol::tool_call::AwaitMode;
use gents_toolcalls::call::lifecycle::admission_fixture::{
    complete_child_managed_verified, completion_obligations, published_admission,
    published_session_message, settle_with, update_request, PublishedAdmissionOptions,
};

#[tokio::test]
async fn generated_session_message_recovery_cases_use_accepted_call() {
    let cases = gents_lean_vocab::lean_recovery_sweep_cases()
        .iter()
        .filter(|case| case.sweep_id == "tool_call_lifecycle_recover_session_message_rows")
        .collect::<Vec<_>>();
    assert!(
        !cases.is_empty(),
        "Lean emitted no session-message recovery rows"
    );
    for case in cases {
        let name = case.name.as_str();
        assert_eq!(case.pre_state, "running", "{name}");
        let cause = case
            .recovery_cause
            .as_deref()
            .unwrap_or_else(|| panic!("{name}: session-message row carries its observed cause"));
        if cause == "causedRequestUnbound" {
            let admission = published_admission(PublishedAdmissionOptions {
                name: format!("recovery-closeout-{name}"),
                real_identity: true,
                await_mode: AwaitMode::Background,
                tool_name: Some(gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.to_owned()),
                start_running: true,
                ..Default::default()
            })
            .await
            .expect("publish a running session-message row without a receipt");
            let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
            crate::lifecycle_hooks::attach(&admission.node).unwrap();
            let session_id = admission.tool.session_id().to_owned();
            let settled = settle_with(&admission, &SessionGoals).await;
            assert_eq!(settled, case.measure_before - case.measure_after, "{name}");
            let escaped = gents_store::graphql::escape_graphql_string(&tool_doc_id);
            let response = admission
                .tool
                .access()
                .unwrap()
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state tool_failure_class }} }}"#
                ))
                .await
                .unwrap();
            let row = &response["data"]["AgentToolCall"][0];
            assert_eq!(
                row["lifecycle_state"],
                case.terminal_state.as_str(),
                "{name}"
            );
            assert_eq!(row["tool_failure_class"], "external", "{name}");
            let (notifications, _) = completion_obligations(
                &admission.tool.access().unwrap(),
                &session_id,
                &admission.node_did,
                admission.tool.requester_did(),
            )
            .await;
            assert_eq!(
                notifications.len(),
                1,
                "{name}: fails closed with a notification"
            );
            assert_notification_reason(&notifications[0], case);
            assert_eq!(
                settle_with(&admission, &SessionGoals).await,
                0,
                "{name}: fails closed once"
            );
            admission
                .node
                .shutdown()
                .await
                .expect("shut down test node");
            std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
            continue;
        }
        let message = published_session_message(PublishedAdmissionOptions {
            name: format!("recovery-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted session message and materialize its request");
        let admission = &message.admission;
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        crate::lifecycle_hooks::attach(&admission.node).unwrap();
        let caused_state = match cause {
            "requestCompleted" => {
                // Each Lean sessionMessageRecoveryCase handoff binds a real
                // Goal observation in the receipt-named caused session:
                // complete (reasonless success form), absent (no Goal at
                // all), unknown (a Goal whose persisted status is malformed,
                // so parsed_status cannot resolve it), and live
                // observations. The fixture states must be distinct or the
                // generated witnesses do not exercise the vocabulary.
                let caused_session_id = {
                    let doc_id =
                        gents_store::graphql::escape_graphql_string(&message.caused_request_doc_id);
                    let response = admission
                        .tool
                        .access()
                        .unwrap()
                        .execute(&format!(
                            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ session_id }} }}"#
                        ))
                        .await
                        .unwrap();
                    response["data"]["AgentRequest"][0]["session_id"]
                        .as_str()
                        .expect("the caused request names its session")
                        .to_owned()
                };
                if name != "session_message_request_completed_no_goal_to_completed" {
                    let access = admission.tool.access().unwrap();
                    let response = access
                        .execute(&format!(
                            r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}) {{{}}}}}"#,
                            gents_store::graphql::escape_graphql_string(
                                &message.caused_request_doc_id
                            ),
                            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
                        ))
                        .await
                        .unwrap();
                    let caller: gents_protocol::row::AgentRequestRow =
                        serde_json::from_value(response["data"]["AgentRequest"][0].clone())
                            .unwrap();
                    let created = crate::goal::create_goal_for_request_from_access(
                        &admission.node,
                        Some(&access),
                        &caller,
                        "Handoff assignment",
                        None,
                    )
                    .await
                    .expect("bind the modeled Goal to its physical caused request");
                    let goal = created.goal();
                    assert_eq!(
                        goal.assignment_root_request_doc_id.as_deref(),
                        Some(message.caused_request_doc_id.as_str())
                    );
                    let modeled = tokio::task::spawn_blocking(|| {
                        gents_lean_vocab::lean_contract_snapshot()
                            .goal_link_cases
                            .iter()
                            .find(|case| {
                                case.collection == "Goal"
                                    && case.actor == "agent"
                                    && case.mode == "fresh"
                                    && case.commit
                            })
                            .unwrap()
                            .clone()
                    })
                    .await
                    .unwrap();
                    let escaped = gents_store::graphql::escape_graphql_string(&goal.doc_id);
                    let visible = access.execute(&format!(r#"{{ Goal(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ creator_did agent_did parent_linked }} }}"#)).await.unwrap();
                    let selected = caller.agent_did.as_deref().unwrap();
                    assert_eq!(visible["data"]["Goal"][0]["creator_did"], selected);
                    assert_eq!(visible["data"]["Goal"][0]["agent_did"], selected);
                    assert_eq!(
                        visible["data"]["Goal"][0]["parent_linked"],
                        modeled.initial.linked
                    );
                    let owners = access
                        .transact_readonly("test.closeout_goal_owner", |txn| {
                            let doc = &goal.doc_id;
                            Box::pin(async move {
                                txn.document_owners("Goal", std::slice::from_ref(doc)).await
                            })
                        })
                        .await
                        .unwrap();
                    assert_eq!(
                        owners[0].owner.as_ref().map(|owner| owner.as_str()),
                        Some(selected)
                    );
                }
                match name {
                    // The reasonless success form must be produced by an
                    // actually-complete assignment, not by a missing Goal:
                    // those are different production paths.
                    "session_message_request_completed_to_completed" => {
                        crate::goal::set_goal_from_access(
                            &admission.tool.access().unwrap(),
                            &admission.node_did,
                            &caused_session_id,
                            Some("done"),
                            Some(gents_protocol::goal_status::GoalStatus::Complete),
                            None,
                            None,
                        )
                        .await
                        .expect("persist a complete Goal in the caused session");
                    }
                    // Absent: the caused session carries no Goal document.
                    "session_message_request_completed_no_goal_to_completed" => {}
                    // Unknown: a Goal whose persisted status is malformed, so
                    // the canonical read resolves the document but cannot
                    // parse its status into the vocabulary.
                    "session_message_request_completed_unknown_goal_to_completed" => {
                        let goal = crate::goal::set_goal_from_access(
                            &admission.tool.access().unwrap(),
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(gents_protocol::goal_status::GoalStatus::Active),
                            None,
                            None,
                        )
                        .await
                        .expect("persist a Goal in the caused session");
                        seed_goal_status(
                            &admission.tool.access().unwrap(),
                            &goal,
                            r#"status: "corrupted-status""#,
                        )
                        .await;
                    }
                    "session_message_request_completed_live_goal_to_completed" => {
                        crate::goal::set_goal_from_access(
                            &admission.tool.access().unwrap(),
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(gents_protocol::goal_status::GoalStatus::Active),
                            None,
                            None,
                        )
                        .await
                        .expect("persist an active Goal in the caused session");
                    }
                    "session_message_request_completed_blocked_goal_to_completed"
                    | "session_message_request_completed_stopped_goal_to_completed" => {
                        crate::goal::set_goal_from_access(
                            &admission.tool.access().unwrap(),
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(
                                if name
                                    == "session_message_request_completed_stopped_goal_to_completed"
                                {
                                    gents_protocol::goal_status::GoalStatus::Stopped
                                } else {
                                    gents_protocol::goal_status::GoalStatus::Blocked
                                },
                            ),
                            None,
                            None,
                        )
                        .await
                        .expect("persist a blocked Goal in the caused session");
                    }
                    other => panic!("{other}: unhandled completed session-message case"),
                }
                complete_child_managed_verified(
                    &admission.node,
                    &message.caused_request_id,
                    "done",
                    &admission
                        .tool
                        .access()
                        .unwrap()
                        .managed_agent_signer()
                        .unwrap(),
                    admission.runtime.clone(),
                )
                .await;
                None
            }
            "requestFailed" => Some("failed"),
            "requestDead" => Some("dead"),
            "requestInterrupted" => Some("interrupted"),
            "requestSuperseded" => Some("superseded"),
            other => panic!("{name}: unknown session-message recovery cause {other}"),
        };
        if let Some(state) = caused_state {
            update_request(
                &admission.tool.access().unwrap(),
                &message.caused_request_doc_id,
                &format!(r#"lifecycle_state: "{state}""#),
            )
            .await;
        }
        let settled = settle_with(admission, &SessionGoals).await;
        assert_eq!(settled, case.measure_before - case.measure_after, "{name}");
        let escaped = gents_store::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .tool
            .access()
            .unwrap()
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state cancel_cause tool_failure_class }} }}"#
            ))
            .await
            .unwrap();
        let row = &response["data"]["AgentToolCall"][0];
        assert_eq!(
            row["lifecycle_state"],
            case.terminal_state.as_str(),
            "{name}"
        );
        assert_eq!(row["status"], "completed", "{name}");
        if case.terminal_state == "cancelled" {
            assert_eq!(row["cancel_cause"], "interrupted", "{name}");
        }
        // After settle() the row has converged: settlement persists
        // completionPending:<reason>, then append_background_tool_completion
        // publishes the notice and advances the row to completed (verified in
        // the settlement sequence). The persisted pre-delivery suffix is
        // pinned separately, in the crash-before-notice regression where the
        // side effect is actually skipped.
        assert_eq!(row["status"], "completed", "{name}");
        let session_id = admission.tool.session_id().to_owned();
        let (notifications, _) = completion_obligations(
            &admission.tool.access().unwrap(),
            &session_id,
            &admission.node_did,
            admission.tool.requester_did(),
        )
        .await;
        assert_eq!(notifications.len(), 1, "{name}");
        // F3: the notification reason must be validated against the modeled
        // handoff for the case, not only against the raw suffix: each
        // generated case pins its own assignment_* vocabulary (or the
        // reasonless success form).
        let expected_reason = handoff_reason_for_case(name);
        assert_eq!(
            case.notification_reason.as_deref(),
            expected_reason,
            "{name}: the Lean contract's notificationReason must match the case's modeled handoff"
        );
        assert_notification_reason(&notifications[0], case);
        assert_eq!(settle_with(admission, &SessionGoals).await, 0, "{name}");
        message
            .admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(&message.admission.path).expect("remove exact recovery fixture");
    }
}

fn assert_notification_reason(notification: &str, case: &gents_lean_vocab::LeanRecoverySweepCase) {
    match case.notification_reason.as_deref() {
        Some(reason) => assert!(
            notification.contains(&format!("<reason>{reason}</reason>")),
            "{}: {notification}",
            case.name
        ),
        None => assert!(!notification.contains("<reason>"), "{}", case.name),
    }
}

/// Corrupt a persisted Goal status through the document mutation path the
/// goal controller tests use, producing an unreadable assignment observation
/// (`parsed_status` resolves the document but cannot parse the status).
async fn seed_goal_status(
    access: &gents_store::config_client::ConfigAccess,
    goal: &crate::goal::GoalDocument,
    fields: &str,
) {
    let doc_id = gents_store::graphql::escape_graphql_string(&goal.doc_id);
    let node_did = gents_store::graphql::escape_graphql_string(&goal.node_did);
    let response = access
        .write(
            "fixture.goal.seed_status",
            &format!(
                r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{node_did}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
            ),
        )
        .await
        .expect("seed Goal status as its owning Agent");
    assert_eq!(
        response["data"]["update_Goal"].as_array().map(Vec::len),
        Some(1),
        "seed Goal status must change the Agent's Goal: {response}"
    );
}

fn handoff_reason_for_case(name: &str) -> Option<&'static str> {
    match name {
        "session_message_request_failed_to_failed" => Some("request_failed"),
        "session_message_request_interrupted_to_cancelled" => Some("request_interrupted"),
        "session_message_request_dead_to_failed" => Some("request_dead"),
        "session_message_request_superseded_to_failed" => Some("request_superseded"),
        "session_message_request_completed_to_completed" => None,
        "session_message_request_completed_no_goal_to_completed" => Some("assignment_absent"),
        "session_message_request_completed_unknown_goal_to_completed" => Some("assignment_unknown"),
        "session_message_request_completed_live_goal_to_completed" => Some("assignment_active"),
        "session_message_request_completed_blocked_goal_to_completed" => Some("assignment_blocked"),
        "session_message_request_completed_stopped_goal_to_completed" => Some("assignment_stopped"),
        other => panic!("{other}: unhandled session-message case"),
    }
}
