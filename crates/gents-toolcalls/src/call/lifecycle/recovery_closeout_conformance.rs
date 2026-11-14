use crate::call::lifecycle::admission_fixture::{published_admission, PublishedAdmissionOptions};
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
    for case in
        gents_lean_vocab::families::contracts_restart_missing_parent::cases::<String, String>()
    {
        let input = &case.input;
        let name = input.name.as_str();
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("restart-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::from_persisted(&input.await_mode).expect("modeled await mode"),
            tool_name: input
                .session_message
                .then(|| gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.to_owned()),
            ..Default::default()
        })
        .await
        .expect("publish accepted restart call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let access = admission.tool.access().unwrap();
        remove_parent(&admission.client, admission.tool.request_doc_id().unwrap()).await;
        if input.deadline_expired {
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
        let escaped = gents_store::graphql::escape_graphql_string(&tool_doc_id);
        let response = access
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state await_mode cancel_cause tool_failure_class }} }}"#
            ))
            .await
            .unwrap();
        let session_id = admission.tool.session_id().to_owned();
        let (messages, wakes) = completion_obligations(
            &access,
            &session_id,
            &admission.node_did,
            admission.tool.requester_did(),
        )
        .await;
        let observed = super::recovery_conformance::observed_restart(
            report.tool_calls_recovered,
            &response["data"]["AgentToolCall"][0],
            &messages,
            &wakes,
            &session_id,
        );
        admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
        case.check(gents_lean_vocab::family::recode(&observed));
    }
}

pub(super) fn swept(count: usize) -> u64 {
    u64::try_from(count).expect("a sweep count fits u64")
}

pub(super) fn notice_reason(notifications: &[String]) -> Option<String> {
    match notifications {
        [] => None,
        [notice] => notice
            .split_once("<reason>")
            .and_then(|(_, rest)| rest.split_once("</reason>"))
            .map(|(reason, _)| reason.to_owned()),
        many => panic!("at most one completion notice: {many:?}"),
    }
}

async fn lifecycle_state(
    access: &gents_store::config_client::ConfigAccess,
    tool_doc_id: &str,
) -> String {
    let escaped = gents_store::graphql::escape_graphql_string(tool_doc_id);
    let response = access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state }} }}"#
        ))
        .await
        .unwrap();
    response["data"]["AgentToolCall"][0]["lifecycle_state"]
        .as_str()
        .expect("the tool row has a lifecycle state")
        .to_owned()
}

#[cfg(unix)]
#[tokio::test]
async fn generated_orphan_background_recovery_cases_use_accepted_native_call() {
    for case in gents_lean_vocab::families::recovery_sweeps_orphan_accepted::cases() {
        let input = &case.input;
        let name = input.name.as_str();
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
        let access = admission.tool.access().unwrap();
        assert_eq!(
            lifecycle_state(&access, &tool_doc_id).await,
            input.pre_state,
            "{name}"
        );
        if input.parent_terminal {
            update_request(&access, &parent_doc_id, r#"lifecycle_state: "completed""#).await;
        }
        if input.parent_interrupted {
            update_request(&access, &parent_doc_id, r#"lifecycle_state: "interrupted""#).await;
        }
        if input.deadline_expired {
            update(
                &access,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        if input.owner_session_stopped {
            gents_session::session::stop_session_with_access(
                &admission.node,
                &access,
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
            &input.process_outcome,
        )
        .await;
        let first = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &recovery_identities(&admission),
            &registry,
        )
        .await
        .unwrap();
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
        let response = access
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state }} }}"#
            ))
            .await
            .unwrap();
        let row = &response["data"]["AgentToolCall"][0];
        assert_eq!(row["status"], "completed", "{name}");
        let session_id = format!("session-{fixture_name}");
        let (notifications, wakes) = completion_obligations(
            &access,
            &session_id,
            &admission.node_did,
            admission.tool.requester_did(),
        )
        .await;
        assert_eq!(wakes.len(), notifications.len(), "{name}");
        let second = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &recovery_identities(&admission),
            &registry,
        )
        .await
        .unwrap();
        admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
        case.check(
            gents_lean_vocab::families::recovery_sweeps_orphan_accepted::OrphanSettled {
                terminal_state: row["lifecycle_state"]
                    .as_str()
                    .expect("the tool row has a lifecycle state")
                    .to_owned(),
                measure_before: swept(first.tool_calls_terminalized),
                measure_after: swept(second.tool_calls_terminalized),
                notification_reason: notice_reason(&notifications),
            },
        );
    }
}

#[tokio::test]
async fn generated_missing_parent_deferred_cases_keep_accepted_row_running() {
    for case in gents_lean_vocab::families::recovery_sweeps_orphan_missing_parent::cases() {
        let input = &case.input;
        let name = input.name.as_str();
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("recovery-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted native background call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let access = admission.tool.access().unwrap();
        assert_eq!(
            lifecycle_state(&access, &tool_doc_id).await,
            input.pre_state,
            "{name}"
        );
        remove_parent(&admission.client, admission.tool.request_doc_id().unwrap()).await;
        if input.deadline_expired {
            update(
                &access,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
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
        let first = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &recovery_identities(&admission),
            &registry,
        )
        .await
        .unwrap();
        let terminal_state = lifecycle_state(&access, &tool_doc_id).await;
        let second = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &recovery_identities(&admission),
            &registry,
        )
        .await
        .unwrap();
        let (notifications, _) = completion_obligations(
            &access,
            admission.tool.session_id(),
            &admission.node_did,
            admission.tool.requester_did(),
        )
        .await;
        admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
        case.check(
            gents_lean_vocab::families::recovery_sweeps_orphan_missing_parent::OrphanSettled {
                terminal_state,
                measure_before: swept(first.tool_calls_terminalized),
                measure_after: swept(second.tool_calls_terminalized),
                notification_reason: notice_reason(&notifications),
            },
        );
    }
}

#[tokio::test]
async fn generated_background_completion_recovery_uses_accepted_native_call() {
    for case in gents_lean_vocab::families::recovery_sweeps_background_completion::cases() {
        let input = &case.input;
        let name = input.name.as_str();
        assert!(
            input.parent_resolvable && !input.side_effects_done,
            "{name}: the accepted call keeps its parent and owes its completion side effects"
        );
        let fixture_name = format!("recovery-closeout-{name}");
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: fixture_name.clone(),
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
        let access = admission.tool.access().unwrap();
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        assert_eq!(
            lifecycle_state(&access, &tool_doc_id).await,
            input.pre_state,
            "{name}"
        );
        let first = ToolCallLifecycle::reconcile_background_completion_side_effects(
            &admission.node,
            &admission.node_did,
            &recovery_identities(&admission),
        )
        .await
        .unwrap();
        let terminal_state = lifecycle_state(&access, &tool_doc_id).await;
        let second = ToolCallLifecycle::reconcile_background_completion_side_effects(
            &admission.node,
            &admission.node_did,
            &recovery_identities(&admission),
        )
        .await
        .unwrap();
        assert!(second.is_noop(), "{name}");
        let session_id = format!("session-{fixture_name}");
        let (notifications, wakes) = completion_obligations(
            &access,
            &session_id,
            &admission.node_did,
            admission.tool.requester_did(),
        )
        .await;
        assert_eq!(notifications.len(), 1, "{name}");
        assert_eq!(wakes.len(), 1, "{name}");
        admission
            .node
            .shutdown()
            .await
            .expect("shut down test node");
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
        case.check(
            gents_lean_vocab::families::recovery_sweeps_background_completion::Swept {
                terminal_state,
                measure_before: swept(first.side_effects_converged),
                measure_after: swept(second.side_effects_converged),
            },
        );
    }
}
