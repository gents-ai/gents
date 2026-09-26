use super::{AwaitMode, ToolCallLifecycle};
use crate::tool_call_lifecycle::admission_fixture::{
    published_admission, PublishedAdmission, PublishedAdmissionOptions,
};
use defra_node::EmbeddedNode;

async fn update(node: &EmbeddedNode, collection: &str, doc_id: &str, fields: &str) {
    let response = node
        .execute(&format!(
            r#"mutation {{ update_{collection}(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(doc_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

async fn row(node: &EmbeddedNode, doc_id: &str) -> serde_json::Value {
    let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_doc_id lifecycle_state await_mode cancel_cause tool_failure_class }} }}"#, crate::graphql::escape_graphql_string(doc_id))).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.unwrap()["AgentToolCall"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 1);
    rows.into_iter().next().unwrap()
}

async fn teardown(admission: PublishedAdmission) {
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

async fn restart_obligations(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{
                AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }}
                AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ input }}
            }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    let mut messages = Vec::new();
    for row in data["AgentMessage"].as_array().unwrap() {
        let doc_id = row["_docID"].as_str().unwrap();
        let (_, message) = crate::session::load_canonical_message_from_node(
            node,
            doc_id,
            agent_did,
            requester_did,
        )
        .await
        .expect("reconstruct canonical restart notification");
        if let crate::llm::message::Message::User { content } = message {
            for item in content {
                if let crate::llm::message::UserContent::Text(text) = item {
                    if text.text.contains("<tool-completion") {
                        messages.push(text.text);
                    }
                }
            }
        }
    }
    let wakes = data["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["input"].clone())
        .collect();
    (messages, wakes)
}

/// Drives the native restart classifier rows through canonical provider
/// publication. Session-message rows are exercised separately; the missing
/// parent rows remain an integration fault-injection fixture by design.
#[cfg(unix)]
#[tokio::test]
async fn generated_native_restart_dispositions_use_canonical_admission_owner() {
    let cases = crate::lean_vocab_test::lean_restart_disposition_cases();
    for name in [
        "restart_native_background_live_parent_interrupted",
        "restart_native_background_deadline_expired_times_out",
        "restart_native_background_interrupted_parent_lost_on_restart",
        "restart_native_background_terminal_parent_lost_on_restart",
        "restart_native_background_unowned_process_lost",
        "restart_native_background_exited_process_lost",
        "restart_foreground_live_parent_left_running",
        "restart_foreground_interrupted_parent_cancelled",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert!(
            !case.session_message,
            "{name} must remain a native tool row"
        );
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("restart-{name}"),
            // Restart terminalization may owe a scheduled wake. That owner
            // signs a new AgentRequest, so this fixture must use the same
            // registered KeyIdentity boundary as production rather than the
            // literal identity used by transition-only tests.
            real_identity: true,
            await_mode: if case.await_mode == "background" {
                AwaitMode::Background
            } else {
                AwaitMode::Foreground
            },
            start_running: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool_doc = admission.tool.doc_id().unwrap().to_owned();
        let request_doc = admission.tool.request_doc_id().unwrap().to_owned();
        if case.deadline_expired {
            update(
                admission.node.as_ref(),
                "AgentToolCall",
                &tool_doc,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        match case.parent_observation.as_str() {
            "live" => {}
            "interrupted" => {
                update(
                    admission.node.as_ref(),
                    "AgentRequest",
                    &request_doc,
                    r#"lifecycle_state: "interrupted""#,
                )
                .await;
            }
            "otherTerminal" => {
                update(
                    admission.node.as_ref(),
                    "AgentRequest",
                    &request_doc,
                    r#"lifecycle_state: "failed""#,
                )
                .await;
            }
            other => panic!("unsupported native restart parent observation {other}"),
        }

        // The crashed runtime left a durable record for a surviving
        // test-owned group; the restarted runtime's owner reads it.
        let records = tempfile::tempdir().unwrap();
        let registry = crate::hook::BackgroundExecutionRegistry::default()
            .with_process_records(records.path().to_path_buf());
        let process = if case.await_mode == "background" {
            crate::managed_exec::ownership::test_support::process_for_generated_outcome(
                &registry,
                admission.tool.tool_call_id(),
                &tool_doc,
                &case.process_outcome,
            )
            .await
        } else {
            None
        };
        let report = ToolCallLifecycle::recover_all_with_executions(
            &admission.node,
            &admission.agent_did,
            &registry,
        )
        .await
        .unwrap();
        if let Some(process) = process {
            assert_ne!(
                process.identity.observe(),
                crate::managed_exec::ownership::ProcessObservation::Running,
                "{name}: restart recovery left the recorded process running"
            );
            process.finish().await;
        }
        let actual = row(&admission.node, &tool_doc).await;
        match case.disposition.as_str() {
            "leave_running" => {
                assert_eq!(report.tool_calls_recovered, 0, "{name}");
                assert_eq!(actual["lifecycle_state"], "running", "{name}");
            }
            "terminalize" => {
                assert_eq!(report.tool_calls_recovered, 1, "{name}");
                assert_eq!(
                    actual["lifecycle_state"],
                    case.terminal_state.as_deref().unwrap(),
                    "{name}"
                );
            }
            other => panic!("unsupported restart disposition {other}"),
        }
        match case.cause.as_deref() {
            None => {
                assert!(actual["cancel_cause"].is_null(), "{name}");
                assert!(actual["tool_failure_class"].is_null(), "{name}");
            }
            Some("deadlineExceeded") => {
                assert_eq!(actual["cancel_cause"], "deadline", "{name}");
                assert_eq!(actual["tool_failure_class"], "external", "{name}");
            }
            Some("parentInterrupted" | "TerminalizeBackgroundedAsInterrupted") => {
                assert_eq!(actual["cancel_cause"], "interrupted", "{name}");
                assert!(actual["tool_failure_class"].is_null(), "{name}");
            }
            Some("parentTerminal" | "processLost") => {
                assert!(actual["cancel_cause"].is_null(), "{name}");
                assert_eq!(actual["tool_failure_class"], "external", "{name}");
            }
            other => panic!("unsupported native restart cause {other:?}"),
        }
        let session_id = format!("session-restart-{name}");
        let (messages, wakes) = restart_obligations(
            &admission.node,
            &session_id,
            &admission.agent_did,
            Some(&admission.agent_did),
        )
        .await;
        if let Some(reason) = case.notification_reason.as_deref() {
            assert_eq!(
                messages.len(),
                1,
                "{name}: report={report:?}, tool={actual:?}, wakes={wakes:?}"
            );
            assert!(messages[0].contains(&format!("<reason>{reason}</reason>")));
            assert_eq!(wakes.len(), 1, "{name}");
            assert_eq!(
                wakes[0]["queue"]["source"],
                case.queue_source.as_deref().unwrap(),
                "{name}"
            );
            assert_eq!(
                wakes[0]["queue"]["key"],
                format!(
                    "{}{}",
                    case.queue_key_prefix.as_deref().unwrap(),
                    session_id
                ),
                "{name}"
            );
        } else {
            assert_eq!(report.notifications_repaired, 0, "{name}");
            assert!(messages.is_empty(), "{name}");
            assert!(wakes.is_empty(), "{name}");
        }
        let second = ToolCallLifecycle::recover_all_with_executions(
            &admission.node,
            &admission.agent_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(second.tool_calls_recovered, 0, "{name}");
        let (messages_after, wakes_after) = restart_obligations(
            &admission.node,
            &session_id,
            &admission.agent_did,
            Some(&admission.agent_did),
        )
        .await;
        assert_eq!(messages_after, messages, "{name}");
        assert_eq!(wakes_after, wakes, "{name}");
        teardown(admission).await;
    }
}

/// A started session is an ordinary agent's session, not a subordinate: no
/// parent observation ends its `create_session`/`send_message` row on restart,
/// only the row's own expired deadline does. The rows come from the Lean
/// classifier; the fixture publishes an accepted session-message call.
#[tokio::test]
async fn generated_session_message_restart_dispositions_use_canonical_admission_owner() {
    let cases = crate::lean_vocab_test::lean_restart_disposition_cases()
        .iter()
        .filter(|case| case.session_message && case.parent_observation != "missing")
        .collect::<Vec<_>>();
    assert!(
        !cases.is_empty(),
        "Lean emitted no session-message restart rows"
    );
    for case in cases {
        let name = case.name.as_str();
        assert_eq!(case.await_mode, "background", "{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("restart-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            tool_name: Some(crate::toolset::CREATE_SESSION_TOOL_NAME.to_owned()),
            start_running: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool_doc = admission.tool.doc_id().unwrap().to_owned();
        let request_doc = admission.tool.request_doc_id().unwrap().to_owned();
        if case.deadline_expired {
            update(
                admission.node.as_ref(),
                "AgentToolCall",
                &tool_doc,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        let parent_state = match case.parent_observation.as_str() {
            "live" => None,
            "interrupted" => Some("interrupted"),
            "cleanlyCompleted" => Some("completed"),
            "otherTerminal" => Some("failed"),
            other => panic!("unsupported session-message restart parent observation {other}"),
        };
        if let Some(state) = parent_state {
            update(
                admission.node.as_ref(),
                "AgentRequest",
                &request_doc,
                &format!(r#"lifecycle_state: "{state}""#),
            )
            .await;
            // The periodic terminal-parent sweep never ends a background row.
            let periodic = ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
                &admission.node,
                &admission.agent_did,
            )
            .await
            .unwrap();
            assert_eq!(periodic.tool_calls_terminalized, 0, "{name}");
        }

        let report = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
            .await
            .unwrap();
        let actual = row(&admission.node, &tool_doc).await;
        assert_eq!(actual["await_mode"], case.await_mode.as_str(), "{name}");
        match case.disposition.as_str() {
            "leave_running" => {
                assert_eq!(report.tool_calls_recovered, 0, "{name}");
                assert_eq!(actual["lifecycle_state"], "running", "{name}");
                assert!(actual["cancel_cause"].is_null(), "{name}");
                assert!(actual["tool_failure_class"].is_null(), "{name}");
            }
            "terminalize" => {
                assert_eq!(report.tool_calls_recovered, 1, "{name}");
                assert_eq!(
                    actual["lifecycle_state"],
                    case.terminal_state.as_deref().unwrap(),
                    "{name}"
                );
                assert_eq!(case.cause.as_deref(), Some("deadlineExceeded"), "{name}");
                assert_eq!(actual["cancel_cause"], "deadline", "{name}");
                assert_eq!(actual["tool_failure_class"], "external", "{name}");
            }
            other => panic!("unsupported session-message restart disposition {other}"),
        }
        let session_id = format!("session-restart-{name}");
        let (messages, wakes) = restart_obligations(
            &admission.node,
            &session_id,
            &admission.agent_did,
            Some(&admission.agent_did),
        )
        .await;
        assert_eq!(
            messages.len(),
            usize::from(case.notification_reason.is_some()),
            "{name}"
        );
        assert_eq!(
            wakes.len(),
            usize::from(case.queue_source.is_some()),
            "{name}"
        );
        let second = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
            .await
            .unwrap();
        assert_eq!(second.tool_calls_recovered, 0, "{name}");
        teardown(admission).await;
    }
}

#[tokio::test]
async fn generated_native_recovery_cases_use_canonical_admission_owner() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    for name in [
        "tool_backgrounded_running_unowned_process_to_failed",
        "tool_running_deadline_exceeded_to_timed_out",
        "tool_running_parent_interrupted_to_cancelled",
        "live_running_native_tool_parent_interrupted_to_cancelled",
        "tool_running_terminal_parent_to_failed",
        "live_running_tool_parent_terminal_to_failed",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("recovery-{name}"),
            await_mode: if name.starts_with("tool_backgrounded") {
                AwaitMode::Background
            } else {
                AwaitMode::Foreground
            },
            start_running: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool_doc = admission.tool.doc_id().unwrap().to_owned();
        let request_doc = admission.tool.request_doc_id().unwrap().to_owned();
        // The current Recovery contract export pins the post-state but does
        // not yet serialize these boundary premises. Retain the original
        // native fixture inputs explicitly; do not misdescribe absent export
        // fields as modeled inputs.
        let deadline_expired = name == "tool_running_deadline_exceeded_to_timed_out";
        let parent_interrupted = matches!(
            name,
            "tool_running_parent_interrupted_to_cancelled"
                | "live_running_native_tool_parent_interrupted_to_cancelled"
        );
        let parent_terminal = matches!(
            name,
            "tool_running_terminal_parent_to_failed"
                | "live_running_tool_parent_terminal_to_failed"
        );
        if deadline_expired {
            update(
                admission.node.as_ref(),
                "AgentToolCall",
                &tool_doc,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        if parent_interrupted {
            update(
                admission.node.as_ref(),
                "AgentRequest",
                &request_doc,
                r#"lifecycle_state: "interrupted""#,
            )
            .await;
        } else if parent_terminal {
            update(
                admission.node.as_ref(),
                "AgentRequest",
                &request_doc,
                r#"lifecycle_state: "completed""#,
            )
            .await;
        }
        let report = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
            .await
            .unwrap();
        assert_eq!(report.tool_calls_recovered, 1, "{name}");
        let actual = row(&admission.node, &tool_doc).await;
        assert_eq!(actual["request_doc_id"], request_doc, "{name}");
        assert_eq!(actual["lifecycle_state"], case.terminal_state, "{name}");
        if parent_interrupted {
            assert_eq!(actual["cancel_cause"], "interrupted", "{name}");
            assert!(actual["tool_failure_class"].is_null(), "{name}");
        } else if name.starts_with("tool_backgrounded") || deadline_expired || parent_terminal {
            assert_eq!(actual["tool_failure_class"], "external", "{name}");
        }
        teardown(admission).await;
    }
}
