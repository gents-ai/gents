use crate::call::lifecycle::admission_fixture::{
    published_admission, PublishedAdmissionOptions,
};
use crate::call::lifecycle::ToolCallLifecycle;
use gents_protocol::tool_call::AwaitMode;

#[cfg(unix)]
mod background_owner;
mod delivery;
mod kill;

use crate::call::lifecycle::admission_fixture::{
    completion_obligations, remove_parent, update, update_request,
};

pub(super) use crate::call::lifecycle::admission_fixture::recovery_identities;

#[tokio::test]
async fn generated_native_missing_parent_restart_cases_defer() {
    let cases = gents_lean_vocab::lean_restart_disposition_cases()
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
                .then(|| gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.to_owned()),
            ..Default::default()
        })
        .await
        .expect("publish accepted restart call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let access = admission.tool.access().unwrap();
        remove_parent(&admission.client, admission.tool.request_doc_id().unwrap()).await;
        if case.deadline_expired {
            update(
                &access,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        let report = ToolCallLifecycle::recover_all(
            &admission.node,
            &admission.node_did,
            &recovery_identities(&admission),
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_recovered, 0, "{name}");
        let escaped = gents_store::graphql::escape_graphql_string(&tool_doc_id);
        let response = access.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state }} }}"#)).await.unwrap();
        assert_eq!(
            response["data"]["AgentToolCall"][0]["lifecycle_state"], "running",
            "{name}"
        );
        admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}





#[cfg(unix)]
#[tokio::test]
async fn generated_orphan_background_recovery_cases_use_accepted_native_call() {
    let cases = gents_lean_vocab::lean_recovery_sweep_cases();
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
                &admission.tool.access().unwrap(),
                &parent_doc_id,
                r#"lifecycle_state: "completed""#,
            )
            .await;
        }
        if case.parent_interrupted == Some(true) {
            update_request(
                &admission.tool.access().unwrap(),
                &parent_doc_id,
                r#"lifecycle_state: "interrupted""#,
            )
            .await;
        }
        if case.deadline_expired == Some(true) {
            update(
                &admission.tool.access().unwrap(),
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        if case.owner_session_stopped == Some(true) {
            gents_session::session::stop_session_with_access(
                &admission.node,
                &admission.tool.access().unwrap(),
                &admission.node_did,
                admission.tool.session_id(),
                admission.tool.requester_did(),
            )
            .await
            .unwrap();
        }
        let records = tempfile::tempdir().unwrap();
        let registry = crate::call::hook::BackgroundExecutionRegistry::default()
            .with_process_records(records.path().to_path_buf());
        let process = crate::call::hook::tests::process_records::process_for_generated_outcome(
            &registry,
            admission.tool.tool_call_id(),
            &tool_doc_id,
            case.process_outcome.as_deref().unwrap(),
        )
        .await;
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &recovery_identities(&admission),
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1, "{name}");
        if let Some(process) = process {
            assert_ne!(
                process.identity.observe(),
                gents_session::exec::ownership::ProcessObservation::Running,
                "{name}: settled row left its process running"
            );
            process.finish().await;
        }
        assert!(registry.process_record_list().is_empty(), "{name}");
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
        let (notifications, wakes) = completion_obligations(
            &admission.tool.access().unwrap(),
            &session_id,
            &admission.node_did,
            admission.tool.requester_did(),
        )
        .await;
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
            &recovery_identities(&admission),
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(second.tool_calls_terminalized, 0, "{name}");
        admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

#[tokio::test]
async fn generated_missing_parent_deferred_cases_keep_accepted_row_running() {
    let cases = gents_lean_vocab::lean_recovery_sweep_cases();
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
        remove_parent(&admission.client, admission.tool.request_doc_id().unwrap()).await;
        assert_eq!(case.deadline_expired, Some(true), "{name}");
        update(
            &admission.tool.access().unwrap(),
            &tool_doc_id,
            r#"deadline_at: "2020-01-01T00:00:00Z""#,
        )
        .await;
        let error = match ToolCallLifecycle::load_by_doc_id_as(
            admission.node.clone(),
            admission.tool.actor().clone(),
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
        let registry = crate::call::hook::BackgroundExecutionRegistry::default();
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &recovery_identities(&admission),
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 0, "{name}");
        let escaped = gents_store::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .tool
            .access()
            .unwrap()
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state }} }}"#
            ))
            .await
            .unwrap();
        assert_eq!(
            response["data"]["AgentToolCall"][0]["lifecycle_state"], "running",
            "{name}"
        );
        admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}
#[tokio::test]
async fn generated_background_completion_recovery_uses_accepted_native_call() {
    let cases = gents_lean_vocab::lean_recovery_sweep_cases();
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
            crate::call::lifecycle::FailureClass::External,
            None,
        )
        .await
        .unwrap();
    let report = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.node_did,
        &recovery_identities(&admission),
    )
    .await
    .unwrap();
    assert_eq!(report.side_effects_converged, 1, "{}", case.name);
    let second = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.node_did,
        &recovery_identities(&admission),
    )
    .await
    .unwrap();
    assert!(second.is_noop(), "{}", case.name);
    let session_id = format!("session-{fixture_name}");
    let (notifications, wakes) = completion_obligations(
        &admission.tool.access().unwrap(),
        &session_id,
        &admission.node_did,
        admission.tool.requester_did(),
    )
    .await;
    assert_eq!(notifications.len(), 1, "{}", case.name);
    assert_eq!(wakes.len(), 1, "{}", case.name);
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
}
