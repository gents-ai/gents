use crate::call::lifecycle::admission_fixture::{
    published_admission_with_owner, PublishedAdmissionOptions,
};
use crate::call::lifecycle::ToolCallLifecycle;
use gents_protocol::tool_call::AwaitMode;
use gents_store::identity::NodeIdentity;
use std::sync::Arc;

use crate::call::lifecycle::admission_fixture::{completion_obligations, update_request};

#[cfg(unix)]
#[tokio::test]
async fn generated_registered_background_owner_stop_cases_use_live_worker() {
    let mut owned_blocked = None;
    let mut owned_background = None;
    for session_stopped in [false, true] {
        let cases = gents_lean_vocab::lean_recovery_sweep_cases();
        let left = cases
            .iter()
            .find(|case| case.name == "registered_background_tool_left_to_worker_deferred")
            .unwrap();
        let deleted = cases
            .iter()
            .find(|case| {
                case.name
                    == if session_stopped {
                        "registered_background_tool_session_stopped_to_cancelled"
                    } else {
                        "registered_background_tool_task_deleted_to_cancelled"
                    }
            })
            .unwrap();
        assert_eq!(left.execution_registered, Some(true));
        assert_eq!(left.owner_task_deleted, Some(false));
        assert_eq!(deleted.owner_task_deleted, Some(!session_stopped));
        assert_eq!(deleted.owner_session_stopped, Some(session_stopped));

        let name = if session_stopped {
            "session-stopped"
        } else {
            "task-deleted"
        };
        let db = gents_inference::test_support::test_db(
            &format!("recovery-closeout-{name}"),
            gents_session::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = db.node.clone();
        let identity = gents_store::test_support::held_runtime_identity(db.node_identity.did());
        let node_did = db.node_identity.did().to_owned();
        let node_scope = gents_store::graphql::escape_graphql_string(&node_did);
        gents_store::config_client::ConfigAccess::local(node.clone())
            .write(
                "test.recovery_owner.node",
                &format!(r#"mutation {{ create_Node(input: {{ node_did: "{node_scope}" }}) {{ _docID }} }}"#),
            )
            .await
            .unwrap();
        let session_id = format!("session-{name}");
        let mut parent =
            crate::call::lifecycle::admission_fixture::claimed_signed_request_with_trigger(
                &node,
                &format!("request-{name}"),
                &session_id,
                &identity,
                None,
                Some("trigger-1858"),
            )
            .await;
        let tool = crate::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            node.clone(),
            &mut parent,
            &node_did,
            0,
            gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME,
            "task-native-tool",
            serde_json::json!({"tool_name": "bash", "args": {}}),
            AwaitMode::Background,
            true,
        )
        .await
        .unwrap();
        let signer = tool.access().unwrap().managed_agent_signer().unwrap();
        let identities: Vec<Arc<dyn NodeIdentity>> = vec![Arc::new(signer)];
        let agent_did = parent.request().agent_did.clone();
        let tool_doc_id = tool.doc_id().unwrap().to_owned();
        let tool_call_id = tool.tool_call_id().to_owned();

        let registry = crate::call::hook::BackgroundExecutionRegistry::default();
        let token = tokio_util::sync::CancellationToken::new();
        let reservation = registry
            .reserve(tool_call_id.clone(), tool_doc_id.clone(), token.clone())
            .unwrap();
        let (identity_tx, identity_rx) = tokio::sync::oneshot::channel();
        let worker = {
            let registry = registry.clone();
            let recorder = registry.process_recorder(&tool_call_id, &tool_doc_id);
            let tool_call_id = tool_call_id.clone();
            let tool_doc_id = tool_doc_id.clone();
            tokio::spawn(async move {
                let process =
                    gents_session::exec::ownership::test_support::OwnedTestProcess::spawn(Some(
                        recorder,
                    ))
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
        let job_home = tempfile::tempdir().unwrap();
        let job_driver = crate::background::job::driver::JobDriver::new(
            crate::background::job::driver::JobRuntime {
                node: node.clone(),
                agent_snapshots: Default::default(),
                plugins: Arc::new(crate::plugin::executor::PluginExecutor::new(None)),
                home: job_home.path().to_path_buf(),
                tool_root: None,
                session_goals: Arc::new(gents_session::session::message::NoSessionMessageGoals),
            },
        );

        let escaped_agent = gents_store::graphql::escape_graphql_string(&node_did);
        let selected_agent = gents_store::graphql::escape_graphql_string(&agent_did);
        for mutation in [
            format!(
                r#"mutation {{ create_Task(input: {{ task_id: "task-1858", node_did: "{escaped_agent}", agent_did: "{selected_agent}", prompt_template: "tick" }}) {{ _docID }} }}"#
            ),
            format!(
                r#"mutation {{ create_Trigger(input: {{ trigger_id: "trigger-1858", node_did: "{escaped_agent}", task_id: "task-1858" }}) {{ _docID }} }}"#
            ),
        ] {
            let response = node.execute(&mutation).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &node,
            &node_did,
            &identities,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 0, "{}", left.name);
        assert_eq!(
            process.observe(),
            gents_session::exec::ownership::ProcessObservation::Running,
            "{}",
            left.name
        );

        if session_stopped {
            update_request(
                &tool.access().unwrap(),
                tool.request_doc_id().unwrap(),
                r#"lifecycle_state: "completed""#,
            )
            .await;
            gents_session::session::stop_session_with_access(
                &node,
                &tool.access().unwrap(),
                &node_did,
                &session_id,
                tool.requester_did(),
            )
            .await
            .unwrap();
            let first_resume = gents_session::session::resume_session_with_access(
                &node,
                &tool.access().unwrap(),
                &node_did,
                &session_id,
                tool.requester_did(),
                std::sync::Arc::new(crate::call::hook::RuntimeExecutionWitness {
                    background_executions: registry.clone(),
                    job_driver: job_driver.clone(),
                }),
            )
            .await;
            owned_blocked = Some(first_resume.is_err());
            let stopped = tool
                .access()
                .unwrap()
                .execute(&format!(
                    r#"{{ AgentSession(filter: {{ {} }}, limit: 1) {{ stopped_at }} }}"#,
                    gents_protocol::session::session_scope_filter(
                        &node_did,
                        &session_id,
                        tool.requester_did(),
                    ),
                ))
                .await
                .unwrap();
            assert!(stopped["data"]["AgentSession"][0]["stopped_at"].is_string());
        } else {
            let response = node
        .execute(&format!(
            r#"mutation {{ delete_Task(filter: {{ task_id: {{ _eq: "task-1858" }}, node_did: {{ _eq: "{escaped_agent}" }} }}) {{ _docID }} }}"#
        ))
        .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &node,
            &node_did,
            &identities,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1, "{}", deleted.name);
        worker.await.unwrap();
        assert_ne!(
            process.observe(),
            gents_session::exec::ownership::ProcessObservation::Running,
            "{}",
            deleted.name
        );
        if session_stopped {
            gents_session::session::resume_session_with_access(
                &node,
                &tool.access().unwrap(),
                &node_did,
                &session_id,
                tool.requester_did(),
                std::sync::Arc::new(crate::call::hook::RuntimeExecutionWitness {
                    background_executions: registry.clone(),
                    job_driver: job_driver.clone(),
                }),
            )
            .await
            .expect("resume succeeds after the existing recovery owner verifies process exit");
            let resumed = tool
                .access()
                .unwrap()
                .execute(&format!(
                    r#"{{ AgentSession(filter: {{ {} }}, limit: 1) {{ stopped_at }} }}"#,
                    gents_protocol::session::session_scope_filter(
                        &node_did,
                        &session_id,
                        tool.requester_did(),
                    ),
                ))
                .await
                .unwrap();
            owned_background = Some(
                gents_lean_vocab::families::session_control_contracts_resume_waits_for_live_worker::OwnedBackgroundOutput {
                    blocked: owned_blocked.expect("the stopped session tried an early resume"),
                    stopped_after_settlement: resumed["data"]["AgentSession"][0]["stopped_at"]
                        .is_string(),
                },
            );
        }
        let escaped = gents_store::graphql::escape_graphql_string(&tool_doc_id);
        let response = tool
            .access()
            .unwrap()
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state cancel_cause }} }}"#
            ))
            .await
            .unwrap();
        let row = &response["data"]["AgentToolCall"][0];
        assert_eq!(row["lifecycle_state"], deleted.terminal_state.as_str());
        assert_eq!(
            row["cancel_cause"],
            if session_stopped {
                "userCancelled"
            } else {
                "interrupted"
            }
        );
        let (notifications, _) = completion_obligations(
            &tool.access().unwrap(),
            &session_id,
            &node_did,
            tool.requester_did(),
        )
        .await;
        let reason = deleted.notification_reason.as_deref().unwrap();
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].contains(&format!("<reason>{reason}</reason>")));
        node.shutdown().await.expect("shut down test node");
    }
    let observed = owned_background.expect("the stopped-session case resumed after settlement");
    for case in
        gents_lean_vocab::families::session_control_contracts_resume_waits_for_live_worker::cases()
    {
        case.check(gents_lean_vocab::families::session_control_contracts_resume_waits_for_live_worker::OwnedBackgroundOutput {
            blocked: observed.blocked,
            stopped_after_settlement: observed.stopped_after_settlement,
        });
    }
}

#[cfg(unix)]
#[tokio::test]
async fn session_resume_waits_for_live_process_record_after_tool_terminalizes() {
    let (admission, _request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "resume-live-terminal-tool".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        start_running: true,
        ..Default::default()
    })
    .await
    .expect("publish accepted background call");
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    let tool_call_id = admission.tool.tool_call_id().to_owned();
    let session_id = admission.tool.session_id().to_owned();
    let node_did = admission.node_did.clone();
    let mut tool = admission.tool;
    update_request(
        &tool.access().unwrap(),
        tool.request_doc_id().unwrap(),
        r#"lifecycle_state: "completed""#,
    )
    .await;

    let registry = crate::call::hook::BackgroundExecutionRegistry::default();
    let token = tokio_util::sync::CancellationToken::new();
    let reservation = registry
        .reserve(tool_call_id.clone(), tool_doc_id.clone(), token.clone())
        .unwrap();
    let worker_token = token.clone();
    let (identity_tx, identity_rx) = tokio::sync::oneshot::channel();
    let (release_reservation_tx, release_reservation_rx) = tokio::sync::oneshot::channel();
    let (reservation_released_tx, reservation_released_rx) = tokio::sync::oneshot::channel();
    let worker = {
        let registry = registry.clone();
        let recorder = registry.process_recorder(&tool_call_id, &tool_doc_id);
        let tool_call_id = tool_call_id.clone();
        let tool_doc_id = tool_doc_id.clone();
        tokio::spawn(async move {
            let process = gents_session::exec::ownership::test_support::OwnedTestProcess::spawn(
                Some(recorder),
            )
            .await;
            let _ = identity_tx.send(process.identity.clone());
            let _ = release_reservation_rx.await;
            drop(reservation);
            reservation_released_tx.send(()).unwrap();
            worker_token.cancelled().await;
            process.finish().await;
            registry
                .release_process_record(&tool_call_id, &tool_doc_id)
                .await;
        })
    };
    let process = identity_rx.await.unwrap();
    assert_eq!(
        process.observe(),
        gents_session::exec::ownership::ProcessObservation::Running
    );
    release_reservation_tx.send(()).unwrap();
    reservation_released_rx.await.unwrap();
    let stop = gents_session::session::stop_session_with_access(
        &admission.node,
        &tool.access().unwrap(),
        &node_did,
        &session_id,
        tool.requester_did(),
    )
    .await
    .unwrap();
    assert_eq!(stop.status.as_str(), "stop_requested");
    tool.cancel_during_run(gents_session::session::tool_call_state::CancelCause::UserCancelled)
        .await
        .unwrap();
    let terminal_row = tool
        .access()
        .unwrap()
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(&tool_doc_id)
        ))
        .await
        .unwrap();
    assert_eq!(
        terminal_row["data"]["AgentToolCall"][0]["lifecycle_state"],
        "cancelled"
    );
    assert_eq!(
        process.observe(),
        gents_session::exec::ownership::ProcessObservation::Running
    );

    let job_home = tempfile::tempdir().unwrap();
    let job_driver = crate::background::job::driver::JobDriver::new(
        crate::background::job::driver::JobRuntime {
            node: admission.node.clone(),
            agent_snapshots: Default::default(),
            plugins: Arc::new(crate::plugin::executor::PluginExecutor::new(None)),
            home: job_home.path().to_path_buf(),
            tool_root: None,
            session_goals: Arc::new(gents_session::session::message::NoSessionMessageGoals),
        },
    );
    let blocked = gents_session::session::resume_session_with_access(
        &admission.node,
        &tool.access().unwrap(),
        &node_did,
        &session_id,
        tool.requester_did(),
        std::sync::Arc::new(crate::call::hook::RuntimeExecutionWitness {
            background_executions: registry.clone(),
            job_driver: job_driver.clone(),
        }),
    )
    .await;
    let resume_blocked = blocked.is_err();
    assert!(
        format!("{blocked:#?}").contains("has not been observed exited"),
        "the retained physical process witness should block resume: {blocked:#?}"
    );

    let other_session = format!("unrelated-{}", uuid::Uuid::new_v4());
    let other_agent = tool.actor().as_ref().to_owned();
    tool.access()
        .unwrap()
        .transact_write("fixture.session.create_unrelated", |txn| {
            let (other_session, node_did, other_agent) = (&other_session, &node_did, &other_agent);
            Box::pin(async move {
                gents_session::session::ensure_session_in_txn(
                    txn,
                    other_session,
                    node_did,
                    other_agent,
                    None,
                    None,
                    None,
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await
            })
        })
        .await
        .unwrap();
    gents_session::session::resume_session_with_access(
        &admission.node,
        &tool.access().unwrap(),
        &node_did,
        &other_session,
        None,
        std::sync::Arc::new(crate::call::hook::RuntimeExecutionWitness {
            background_executions: registry.clone(),
            job_driver: job_driver.clone(),
        }),
    )
    .await
    .expect("an unrelated session is not blocked by this process record");

    let stop_latch = tool
        .access()
        .unwrap()
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ {} }}, limit: 1) {{ stopped_at }} }}"#,
            gents_protocol::session::session_scope_filter(
                &node_did,
                &session_id,
                tool.requester_did(),
            )
        ))
        .await
        .unwrap();
    assert!(stop_latch["data"]["AgentSession"][0]["stopped_at"].is_string());
    token.cancel();
    worker.await.unwrap();
    assert_ne!(
        process.observe(),
        gents_session::exec::ownership::ProcessObservation::Running
    );
    gents_session::session::resume_session_with_access(
        &admission.node,
        &tool.access().unwrap(),
        &node_did,
        &session_id,
        tool.requester_did(),
        std::sync::Arc::new(crate::call::hook::RuntimeExecutionWitness {
            background_executions: registry,
            job_driver,
        }),
    )
    .await
    .expect("resume succeeds after the registered process owner records exit");
    let resumed = tool
        .access()
        .unwrap()
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ {} }}, limit: 1) {{ stopped_at }} }}"#,
            gents_protocol::session::session_scope_filter(
                &node_did,
                &session_id,
                tool.requester_did()
            )
        ))
        .await
        .unwrap();
    for case in
        gents_lean_vocab::families::session_control_contracts_resume_waits_for_process_record::cases(
        )
    {
        case.check(gents_lean_vocab::families::session_control_contracts_resume_waits_for_process_record::OwnedBackgroundOutput {
            blocked: resume_blocked,
            stopped_after_settlement: resumed["data"]["AgentSession"][0]["stopped_at"].is_string(),
        });
    }
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}
