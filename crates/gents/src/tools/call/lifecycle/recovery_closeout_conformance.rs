use crate::tools::call::lifecycle::admission_fixture::{
    complete_child, published_admission, published_session_message, PublishedAdmissionOptions,
};
use crate::tools::call::lifecycle::{AwaitMode, ToolCallLifecycle};

#[cfg(unix)]
mod background_owner;
mod delivery;
mod fixtures;
mod kill;

use fixtures::{completion_obligations, remove_parent, settle, update, update_request};

#[tokio::test]
async fn generated_native_missing_parent_restart_cases_defer() {
    let cases = crate::lean_vocab_test::lean_restart_disposition_cases()
        .iter()
        .filter(|case| case.parent_observation == "missing")
        .collect::<Vec<_>>();
    assert!(cases.iter().any(|case| case.session_message));
    assert!(cases.iter().any(|case| !case.session_message));
    for case in cases {
        let name = case.name.as_str();
        assert_eq!(case.disposition, "leave_running", "{name}");
        assert_eq!(case.await_mode, "background", "{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("restart-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            tool_name: case
                .session_message
                .then(|| crate::tools::native::AGENT_NEW_TOOL_NAME.to_owned()),
            ..Default::default()
        })
        .await
        .expect("publish accepted restart call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        remove_parent(&admission.node, admission.tool.request_doc_id().unwrap()).await;
        if case.deadline_expired {
            update(
                &admission.node,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        let report = ToolCallLifecycle::recover_all(&admission.node, &admission.node_did)
            .await
            .unwrap();
        assert_eq!(report.tool_calls_recovered, 0, "{name}");
        let escaped = crate::store::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission.node.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state }} }}"#)).await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        assert_eq!(
            response.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "running",
            "{name}"
        );
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

fn assert_notification_reason(
    notification: &str,
    case: &crate::lean_vocab_test::LeanRecoverySweepCase,
) {
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
    node: &crate::defra_node::EmbeddedNode,
    goal: &crate::goal::GoalDocument,
    fields: &str,
) {
    let doc_id = crate::store::graphql::escape_graphql_string(&goal.doc_id);
    let node_did = crate::store::graphql::escape_graphql_string(&goal.node_did);
    let response = node.execute(&format!(
        r#"mutation {{ update_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{node_did}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
    assert!(
        !response.has_errors(),
        "seed Goal status: {:?}",
        response.errors
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

#[tokio::test]
async fn generated_session_message_recovery_cases_use_accepted_call() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases()
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
                tool_name: Some(crate::tools::native::AGENT_NEW_TOOL_NAME.to_owned()),
                start_running: true,
                ..Default::default()
            })
            .await
            .expect("publish a running session-message row without a receipt");
            let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
            let session_id = admission.tool.session_id().to_owned();
            let settled = settle(&admission).await;
            assert_eq!(settled, case.measure_before - case.measure_after, "{name}");
            let escaped = crate::store::graphql::escape_graphql_string(&tool_doc_id);
            let response = admission
                .node
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state tool_failure_class }} }}"#
                ))
                .await;
            assert!(!response.has_errors(), "{name}: {:?}", response.errors);
            let row = &response.data.unwrap()["AgentToolCall"][0];
            assert_eq!(
                row["lifecycle_state"],
                case.terminal_state.as_str(),
                "{name}"
            );
            assert_eq!(row["tool_failure_class"], "external", "{name}");
            let (notifications, _) =
                completion_obligations(&admission.node, &session_id, &admission.node_did).await;
            assert_eq!(
                notifications.len(),
                1,
                "{name}: fails closed with a notification"
            );
            assert_notification_reason(&notifications[0], case);
            assert_eq!(settle(&admission).await, 0, "{name}: fails closed once");
            admission.node.shutdown().await;
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
                    let doc_id = crate::store::graphql::escape_graphql_string(
                        &message.caused_request_doc_id,
                    );
                    let response = admission.node.execute(&format!(
                        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ session_id }} }}"#
                    ))
                    .await;
                    assert!(!response.has_errors(), "{name}: {:?}", response.errors);
                    response.data.unwrap()["AgentRequest"][0]["session_id"]
                        .as_str()
                        .expect("the caused request names its session")
                        .to_owned()
                };
                if name != "session_message_request_completed_no_goal_to_completed" {
                    let response = crate::ConfigAccess::local(admission.node.clone())
                        .execute(&format!(
                            r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}) {{{}}}}}"#,
                            crate::store::graphql::escape_graphql_string(
                                &message.caused_request_doc_id
                            ),
                            crate::request::admission::SIGNED_REQUEST_FIELDS,
                        ))
                        .await
                        .unwrap();
                    let caller: gents_protocol::row::AgentRequestRow =
                        serde_json::from_value(response["data"]["AgentRequest"][0].clone())
                            .unwrap();
                    crate::goal::create_goal_for_request(
                        &admission.node,
                        &caller,
                        "Handoff assignment",
                        None,
                    )
                    .await
                    .expect("bind the modeled Goal to its physical caused request");
                }
                match name {
                    // The reasonless success form must be produced by an
                    // actually-complete assignment, not by a missing Goal:
                    // those are different production paths.
                    "session_message_request_completed_to_completed" => {
                        crate::test_support::install_test_agent(
                            &admission.node,
                            &admission.node_did,
                            "general",
                        )
                        .await;
                        crate::goal::set_goal(
                            &admission.node,
                            &admission.node_did,
                            &caused_session_id,
                            Some("done"),
                            Some(crate::goal::GoalStatus::Complete),
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
                        crate::test_support::install_test_agent(
                            &admission.node,
                            &admission.node_did,
                            "general",
                        )
                        .await;
                        let goal = crate::goal::set_goal(
                            &admission.node,
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(crate::goal::GoalStatus::Active),
                            None,
                        )
                        .await
                        .expect("persist a Goal in the caused session");
                        seed_goal_status(&admission.node, &goal, r#"status: "corrupted-status""#)
                            .await;
                    }
                    "session_message_request_completed_live_goal_to_completed" => {
                        crate::test_support::install_test_agent(
                            &admission.node,
                            &admission.node_did,
                            "general",
                        )
                        .await;
                        crate::goal::set_goal(
                            &admission.node,
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(crate::goal::GoalStatus::Active),
                            None,
                        )
                        .await
                        .expect("persist an active Goal in the caused session");
                    }
                    "session_message_request_completed_blocked_goal_to_completed"
                    | "session_message_request_completed_stopped_goal_to_completed" => {
                        crate::test_support::install_test_agent(
                            &admission.node,
                            &admission.node_did,
                            "general",
                        )
                        .await;
                        crate::goal::set_goal(
                            &admission.node,
                            &admission.node_did,
                            &caused_session_id,
                            Some("hold"),
                            Some(
                                if name
                                    == "session_message_request_completed_stopped_goal_to_completed"
                                {
                                    crate::goal::GoalStatus::Stopped
                                } else {
                                    crate::goal::GoalStatus::Blocked
                                },
                            ),
                            None,
                        )
                        .await
                        .expect("persist a blocked Goal in the caused session");
                    }
                    other => panic!("{other}: unhandled completed session-message case"),
                }
                complete_child(
                    &admission.node,
                    &message.caused_request_id,
                    &admission.node_did,
                    "done",
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
                &admission.node,
                &message.caused_request_doc_id,
                &format!(r#"lifecycle_state: "{state}""#),
            )
            .await;
        }
        let settled = settle(admission).await;
        assert_eq!(settled, case.measure_before - case.measure_after, "{name}");
        let escaped = crate::store::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state cancel_cause tool_failure_class }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        let row = &response.data.unwrap()["AgentToolCall"][0];
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
        let (notifications, _) =
            completion_obligations(&admission.node, &session_id, &admission.node_did).await;
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
        assert_eq!(settle(admission).await, 0, "{name}");
        message.admission.node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).expect("remove exact recovery fixture");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn generated_orphan_background_recovery_cases_use_accepted_native_call() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    for name in [
        "orphaned_background_tool_without_execution_to_cancelled",
        "orphaned_background_tool_expired_terminal_parent_to_timed_out",
        "orphaned_background_tool_terminal_parent_to_cancelled",
        "orphaned_background_tool_interrupted_parent_to_cancelled",
        "orphaned_background_tool_unowned_process_to_failed",
        "orphaned_background_tool_exited_process_to_failed",
        "orphaned_background_tool_session_stopped_to_cancelled",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert_eq!(case.execution_registered, Some(false), "{name}");
        assert_eq!(case.owner_task_deleted, Some(false), "{name}");
        let fixture_name = format!("recovery-closeout-{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: fixture_name.clone(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted orphan background call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let parent_doc_id = admission.tool.request_doc_id().unwrap().to_owned();
        if case.parent_terminal == Some(true) {
            update_request(
                &admission.node,
                &parent_doc_id,
                r#"lifecycle_state: "completed""#,
            )
            .await;
        }
        if case.parent_interrupted == Some(true) {
            update_request(
                &admission.node,
                &parent_doc_id,
                r#"lifecycle_state: "interrupted""#,
            )
            .await;
        }
        if case.deadline_expired == Some(true) {
            update(
                &admission.node,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        if case.owner_session_stopped == Some(true) {
            crate::session::stop_session(
                &admission.node,
                &admission.node_did,
                admission.tool.session_id(),
                admission.tool.requester_did(),
            )
            .await
            .unwrap();
        }
        let records = tempfile::tempdir().unwrap();
        let registry = crate::BackgroundExecutionRegistry::default()
            .with_process_records(records.path().to_path_buf());
        let process = crate::tools::exec::ownership::test_support::process_for_generated_outcome(
            &registry,
            admission.tool.tool_call_id(),
            &tool_doc_id,
            case.process_outcome.as_deref().unwrap(),
        )
        .await;
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1, "{name}");
        if let Some(process) = process {
            assert_ne!(
                process.identity.observe(),
                crate::tools::exec::ownership::ProcessObservation::Running,
                "{name}: settled row left its process running"
            );
            process.finish().await;
        }
        assert!(registry.process_record_list().is_empty(), "{name}");
        let escaped = crate::store::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state cancel_cause tool_failure_class }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        let row = &response.data.unwrap()["AgentToolCall"][0];
        assert_eq!(row["status"], "completed", "{name}");
        assert_eq!(
            row["lifecycle_state"],
            case.terminal_state.as_str(),
            "{name}"
        );
        match case.recovery_cause.as_deref() {
            Some("deadlineExceeded" | "parentTerminal" | "processLost") => {
                assert_eq!(row["tool_failure_class"], "external", "{name}");
            }
            Some("sessionStopped") => {
                assert_eq!(row["cancel_cause"], "userCancelled", "{name}");
                assert!(row["tool_failure_class"].is_null(), "{name}");
            }
            Some("TerminalizeBackgroundedAsInterrupted" | "parentInterrupted") => {
                assert_eq!(row["cancel_cause"], "interrupted", "{name}");
                assert!(row["tool_failure_class"].is_null(), "{name}");
            }
            other => panic!("{name}: unsupported Lean recovery cause {other:?}"),
        }
        let session_id = format!("session-{fixture_name}");
        let (notifications, wakes) =
            completion_obligations(&admission.node, &session_id, &admission.node_did).await;
        if let Some(reason) = case.notification_reason.as_deref() {
            assert_eq!(notifications.len(), 1, "{name}");
            assert!(
                notifications[0].contains(&format!("<reason>{reason}</reason>")),
                "{name}"
            );
            assert_eq!(wakes.len(), 1, "{name}");
        } else {
            assert!(notifications.is_empty(), "{name}");
            assert!(wakes.is_empty(), "{name}");
        }
        let second = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(second.tool_calls_terminalized, 0, "{name}");
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

#[tokio::test]
async fn generated_missing_parent_deferred_cases_keep_accepted_row_running() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    for name in ["orphaned_background_tool_expired_missing_parent_deferred"] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert_eq!(case.terminal_state, "running", "{name}");
        assert_eq!((case.measure_before, case.measure_after), (0, 0), "{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("recovery-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted native background call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        remove_parent(&admission.node, admission.tool.request_doc_id().unwrap()).await;
        assert_eq!(case.deadline_expired, Some(true), "{name}");
        update(
            &admission.node,
            &tool_doc_id,
            r#"deadline_at: "2020-01-01T00:00:00Z""#,
        )
        .await;
        let error = match ToolCallLifecycle::load_by_doc_id(
            admission.node.clone(),
            &tool_doc_id,
            &admission.node_did,
            admission.tool.session_id(),
            admission.tool.requester_did(),
        )
        .await
        {
            Ok(_) => panic!("{name}: missing accepted parent must not authorize rehydration"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("tool owner request is missing or ambiguous"),
            "{name}: unexpected missing-owner guard: {error:#}"
        );
        let registry = crate::BackgroundExecutionRegistry::default();
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 0, "{name}");
        let escaped = crate::store::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission.node.execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state }} }}"#
        )).await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        assert_eq!(
            response.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "running",
            "{name}"
        );
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}
#[tokio::test]
async fn generated_background_completion_recovery_uses_accepted_native_call() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    let case = cases
        .iter()
        .find(|case| {
            case.name == "terminal_background_tool_missing_completion_side_effects_to_converged"
        })
        .unwrap();
    let fixture_name = "recovery-closeout-background-completion";
    let mut admission = published_admission(PublishedAdmissionOptions {
        name: fixture_name.into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted background call");
    admission
        .tool
        .fail_owned(
            "seed terminal background failure",
            crate::tools::call::lifecycle::FailureClass::External,
            None,
        )
        .await
        .unwrap();
    let report = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.node_did,
    )
    .await
    .unwrap();
    assert_eq!(report.side_effects_converged, 1, "{}", case.name);
    let second = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.node_did,
    )
    .await
    .unwrap();
    assert!(second.is_noop(), "{}", case.name);
    let session_id = format!("session-{fixture_name}");
    let (notifications, wakes) =
        completion_obligations(&admission.node, &session_id, &admission.node_did).await;
    assert_eq!(notifications.len(), 1, "{}", case.name);
    assert_eq!(wakes.len(), 1, "{}", case.name);
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
}
