//! Recovery conformance after real native tool and session-message
//! admissions. Faults alter only recovery observations; assistant headers and
//! tool rows come from the production stream owner.

use crate::identity::AgentIdentity;
use crate::tool_call_lifecycle::admission_fixture::{
    complete_child, published_admission, published_session_message, PublishedAdmissionOptions,
};
use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use std::sync::Arc;

/// Fixture writes to a `@branchable` collection also advance its collection
/// head, so two back-to-back writes can conflict; retry those, bounded.
async fn execute_fixture_write(node: &crate::defra_node::EmbeddedNode, mutation: &str) {
    let mut backoff = std::time::Duration::from_millis(5);
    for _ in 0..8 {
        let result = node.execute(mutation).await;
        let conflicted = result.errors.iter().any(|error| {
            error
                .extensions
                .as_ref()
                .is_some_and(|extensions| extensions.code == "TXN_CONFLICT")
        });
        if !conflicted {
            assert!(!result.has_errors(), "{:?}", result.errors);
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_millis(200));
    }
    panic!("fixture write kept conflicting: {mutation}");
}

async fn update(node: &crate::defra_node::EmbeddedNode, doc_id: &str, fields: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(node, &format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

async fn update_request(node: &crate::defra_node::EmbeddedNode, doc_id: &str, fields: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(node, &format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

async fn remove_parent(node: &crate::defra_node::EmbeddedNode, doc_id: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let result = node
        .execute(&format!(
            r#"mutation {{ delete_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
}

async fn completion_obligations(
    node: &crate::defra_node::EmbeddedNode,
    session_id: &str,
    agent_did: &str,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }} AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ input }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    let mut notifications = Vec::new();
    for row in data["AgentMessage"].as_array().unwrap() {
        let (_, message) = crate::session::load_canonical_message_from_node(
            node,
            row["_docID"].as_str().unwrap(),
            agent_did,
            Some(agent_did),
        )
        .await
        .expect("reconstruct canonical recovery notification");
        if let crate::llm::message::Message::User { content } = message {
            for item in content {
                if let crate::llm::message::UserContent::Text(text) = item {
                    if text.text.contains("<tool-completion") {
                        notifications.push(text.text);
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
    (notifications, wakes)
}

/// A missing parent replica is never evidence for terminalizing a row: native
/// and session-message rows alike stay running until the parent is observed.
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
                .then(|| crate::toolset::CREATE_SESSION_TOOL_NAME.to_owned()),
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
        let report = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
            .await
            .unwrap();
        assert_eq!(report.tool_calls_recovered, 0, "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
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

/// A `create_session`/`send_message` row ends only on its own deadline or on
/// the terminal of the request it caused. The Lean row carries the observed
/// cause; the fixture builds exactly that premise on an accepted call whose
/// caused request was materialized by the session-message owner.
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
            "deadlineExceeded" => {
                update(
                    &admission.node,
                    &tool_doc_id,
                    r#"deadline_at: "2020-01-01T00:00:00Z""#,
                )
                .await;
                None
            }
            "requestCompleted" => {
                complete_child(
                    &admission.node,
                    &message.caused_request_id,
                    &admission.agent_did,
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
        let report = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
            .await
            .unwrap();
        assert_eq!(
            report.tool_calls_recovered,
            case.measure_before - case.measure_after,
            "{name}"
        );
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
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
        if case.terminal_state == "timedOut" {
            assert_eq!(row["cancel_cause"], "deadline", "{name}");
            assert_eq!(row["tool_failure_class"], "external", "{name}");
        } else if case.terminal_state == "cancelled" {
            assert_eq!(row["cancel_cause"], "interrupted", "{name}");
        }
        let second = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
            .await
            .unwrap();
        assert_eq!(second.tool_calls_recovered, 0, "{name}");
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
        // The restarted runtime's owner reads the record a crashed runtime left
        // for a surviving test-owned process group.
        let records = tempfile::tempdir().unwrap();
        let registry = crate::BackgroundExecutionRegistry::default()
            .with_process_records(records.path().to_path_buf());
        let process = crate::managed_exec::ownership::test_support::process_for_generated_outcome(
            &registry,
            admission.tool.tool_call_id(),
            &tool_doc_id,
            case.process_outcome.as_deref().unwrap(),
        )
        .await;
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.agent_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1, "{name}");
        if let Some(process) = process {
            assert_ne!(
                process.identity.observe(),
                crate::managed_exec::ownership::ProcessObservation::Running,
                "{name}: settled row left its process running"
            );
            process.finish().await;
        }
        assert!(registry.process_record_list().is_empty(), "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
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
            Some("TerminalizeBackgroundedAsInterrupted" | "parentInterrupted") => {
                assert_eq!(row["cancel_cause"], "interrupted", "{name}");
                assert!(row["tool_failure_class"].is_null(), "{name}");
            }
            other => panic!("{name}: unsupported Lean recovery cause {other:?}"),
        }
        let session_id = format!("session-{fixture_name}");
        let (notifications, wakes) =
            completion_obligations(&admission.node, &session_id, &admission.agent_did).await;
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
            &admission.agent_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(second.tool_calls_terminalized, 0, "{name}");
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

/// A live worker keeps its row until the task that started its request is
/// deleted; then the cancellation is persisted and the worker's process
/// stopped through the same owner.
#[cfg(unix)]
#[tokio::test]
async fn generated_registered_background_task_deletion_cases_use_live_worker() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    let left = cases
        .iter()
        .find(|case| case.name == "registered_background_tool_left_to_worker_deferred")
        .unwrap();
    let deleted = cases
        .iter()
        .find(|case| case.name == "registered_background_tool_task_deleted_to_cancelled")
        .unwrap();
    assert_eq!(left.execution_registered, Some(true));
    assert_eq!(left.owner_task_deleted, Some(false));
    assert_eq!(deleted.owner_task_deleted, Some(true));

    let name = "task-deleted";
    let path =
        std::env::temp_dir().join(format!("recovery-closeout-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let identity = crate::KeyIdentity::load_or_create(path.join("agent.key"), None).unwrap();
    let agent_did = identity.did().to_owned();
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .data_path(&path)
            .with_node_identity_did(&agent_did)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_behavior(&node, &agent_did, "general").await;
    let session_id = format!("session-{name}");
    let mut parent =
        crate::tool_call_lifecycle::admission_fixture::claimed_signed_request_with_trigger(
            &node,
            &format!("request-{name}"),
            &session_id,
            &identity,
            None,
            Some("trigger-1858"),
        )
        .await;
    let tool = crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
        node.clone(),
        &mut parent,
        &agent_did,
        0,
        crate::toolset::SPAWN_PROCESS_TOOL_NAME,
        "task-native-tool",
        serde_json::json!({"tool_name": "bash", "args": {}}),
        None,
        AwaitMode::Background,
        CancelPolicy::Cascade,
        true,
    )
    .await
    .unwrap();
    let tool_doc_id = tool.doc_id().unwrap().to_owned();
    let tool_call_id = tool.tool_call_id().to_owned();

    // The live worker: it owns a test process group until its token fires,
    // then stops it, releases its record and its execution.
    let registry = crate::BackgroundExecutionRegistry::default();
    let token = tokio_util::sync::CancellationToken::new();
    let reservation = registry.reserve(tool_call_id.clone(), token.clone());
    let (identity_tx, identity_rx) = tokio::sync::oneshot::channel();
    let worker = {
        let registry = registry.clone();
        let recorder = registry.process_recorder(&tool_call_id, &tool_doc_id);
        let tool_call_id = tool_call_id.clone();
        let tool_doc_id = tool_doc_id.clone();
        tokio::spawn(async move {
            let process = crate::managed_exec::ownership::test_support::OwnedTestProcess::spawn(
                Some(recorder),
            )
            .await;
            let _ = identity_tx.send(process.identity.clone());
            token.cancelled().await;
            process.finish().await;
            registry
                .release_process_record(&tool_call_id, &tool_doc_id)
                .await;
            drop(reservation);
        })
    };
    let process = identity_rx.await.unwrap();

    let escaped_agent = crate::graphql::escape_graphql_string(&agent_did);
    for mutation in [
        format!(
            r#"mutation {{ create_Task(input: {{ task_id: "task-1858", agent_did: "{escaped_agent}", behavior_id: "general", prompt_template: "tick" }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{ create_Trigger(input: {{ trigger_id: "trigger-1858", agent_did: "{escaped_agent}", task_id: "task-1858" }}) {{ _docID }} }}"#
        ),
    ] {
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    let report =
        ToolCallLifecycle::reconcile_orphaned_background_tools(&node, &agent_did, &registry)
            .await
            .unwrap();
    assert_eq!(report.tool_calls_terminalized, 0, "{}", left.name);
    assert_eq!(
        process.observe(),
        crate::managed_exec::ownership::ProcessObservation::Running,
        "{}",
        left.name
    );

    let response = node
        .execute(&format!(
            r#"mutation {{ delete_Task(filter: {{ task_id: {{ _eq: "task-1858" }}, agent_did: {{ _eq: "{escaped_agent}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let report =
        ToolCallLifecycle::reconcile_orphaned_background_tools(&node, &agent_did, &registry)
            .await
            .unwrap();
    assert_eq!(report.tool_calls_terminalized, 1, "{}", deleted.name);
    worker.await.unwrap();
    assert_ne!(
        process.observe(),
        crate::managed_exec::ownership::ProcessObservation::Running,
        "{}",
        deleted.name
    );
    let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state cancel_cause }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = &response.data.unwrap()["AgentToolCall"][0];
    assert_eq!(row["lifecycle_state"], deleted.terminal_state.as_str());
    assert_eq!(row["cancel_cause"], "interrupted");
    let (notifications, _) = completion_obligations(&node, &session_id, &agent_did).await;
    let reason = deleted.notification_reason.as_deref().unwrap();
    assert_eq!(notifications.len(), 1);
    assert!(notifications[0].contains(&format!("<reason>{reason}</reason>")));
    node.shutdown().await;
    std::fs::remove_dir_all(path).unwrap();
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
            &admission.agent_did,
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
            &admission.agent_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 0, "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
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
        .bridge_failure(crate::tool_call_lifecycle::ChildTerminal::Failed {
            reason: "seed terminal background failure".into(),
            failure_class: crate::tool_call_lifecycle::FailureClass::External,
        })
        .await
        .unwrap();
    let report = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.agent_did,
    )
    .await
    .unwrap();
    assert_eq!(report.side_effects_converged, 1, "{}", case.name);
    let second = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.agent_did,
    )
    .await
    .unwrap();
    assert!(second.is_noop(), "{}", case.name);
    let session_id = format!("session-{fixture_name}");
    let (notifications, wakes) =
        completion_obligations(&admission.node, &session_id, &admission.agent_did).await;
    assert_eq!(notifications.len(), 1, "{}", case.name);
    assert_eq!(wakes.len(), 1, "{}", case.name);
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
}
