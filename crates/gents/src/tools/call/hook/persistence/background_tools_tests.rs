#![cfg(unix)]

use super::*;
use crate::tools::call::hook::BackgroundExecutionRegistry;
use crate::tools::call::lifecycle::admission_fixture::{
    published_admission_with_owner, PublishedAdmissionOptions,
};
use crate::tools::exec::ownership::test_support::OwnedTestProcess;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn stop_rechecks_a_process_record_created_after_cancellation_began() {
    let registry = BackgroundExecutionRegistry::default();
    let token = CancellationToken::new();
    let reservation = registry
        .reserve("late-call".into(), "late-doc".into(), token.clone())
        .unwrap();
    let recorder = registry.process_recorder("late-call", "late-doc");
    let stopping = tokio::spawn({
        let registry = registry.clone();
        async move { registry.stop_native_process("late-call", "late-doc").await }
    });
    token.cancelled().await;
    let process = OwnedTestProcess::spawn(Some(recorder)).await;
    let (_, after) = process.identity.stop().await;
    assert_eq!(
        after,
        crate::tools::exec::ownership::ProcessObservation::Exited
    );
    process.finish().await;
    drop(reservation);
    assert_eq!(
        stopping.await.unwrap(),
        crate::tools::exec::ProcessStopOutcome::Stopped
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_worker_keeps_running_row_when_process_exit_is_unverified() {
    let expected = crate::lean_vocab_test::lean_process_stop_cases()
        .iter()
        .find(|case| case.before == "running" && case.after == "running")
        .unwrap();
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "unverified-worker-stop".into(),
        tool_name: Some("bash".into()),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let tool = admission.tool;
    let call_id = tool.tool_call_id().to_owned();
    let doc_id = tool.doc_id().unwrap().to_owned();
    let session = tool.session_id().to_owned();
    let records = tempfile::tempdir().unwrap();
    let registry =
        BackgroundExecutionRegistry::default().with_process_records(records.path().into());
    let process = OwnedTestProcess::spawn(Some(registry.process_recorder(&call_id, &doc_id))).await;
    let reservation = registry
        .reserve(call_id.clone(), doc_id.clone(), CancellationToken::new())
        .unwrap();
    tokio::time::pause();
    let finishing = tokio::spawn(finish_background_execution(
        tool,
        node.clone(),
        registry.live_outputs.clone(),
        registry.clone(),
        reservation,
        Ok(crate::tools::call::lifecycle::ToolOutcome::Cancelled),
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(crate::tools::exec::ownership::KILL_GRACE).await;
    finishing.await.unwrap();
    tokio::time::resume();
    let row = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &doc_id,
        &admission.node_did,
        &session,
        Some(&admission.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.state().as_str(), expected.durable_state);
    assert!(registry.process_record(&call_id, &doc_id).is_some());
    process.finish().await;
    drop(request_owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}

#[tokio::test]
async fn later_stop_preserves_first_timeout_or_failure_and_projects_completion() {
    let model = crate::lean_vocab_test::lean_process_stop_cases()
        .iter()
        .find(|case| case.live_evidence == "exited" && case.worker_released)
        .unwrap();
    for (name, outcome, expected, allowed) in [
        (
            "timeout-first",
            crate::tools::call::lifecycle::ToolOutcome::TimedOut { deadline_at: None },
            "timedOut",
            model.preserve_timeout,
        ),
        (
            "failure-first",
            crate::tools::call::lifecycle::ToolOutcome::tool_returned_error("failed before stop"),
            "failed",
            model.preserve_failure,
        ),
    ] {
        assert!(allowed);
        let (admission, request_owner) =
            published_admission_with_owner(PublishedAdmissionOptions {
                name: name.into(),
                tool_name: Some("bash".into()),
                real_identity: true,
                await_mode: AwaitMode::Background,
                ..Default::default()
            })
            .await
            .unwrap();
        let node = admission.node.clone();
        let tool = admission.tool;
        let call = tool.tool_call_id().to_owned();
        let doc = tool.doc_id().unwrap().to_owned();
        let session = tool.session_id().to_owned();
        let registry = BackgroundExecutionRegistry::default();
        let reservation = registry
            .reserve(call.clone(), doc.clone(), CancellationToken::new())
            .unwrap();
        let process = OwnedTestProcess::spawn(Some(registry.process_recorder(&call, &doc))).await;
        finish_background_execution(
            tool,
            node.clone(),
            registry.live_outputs.clone(),
            registry.clone(),
            reservation,
            Ok(outcome),
        )
        .await;
        assert!(registry.contains(&call, &doc).await);
        let lifecycle = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &doc,
            &admission.node_did,
            &session,
            Some(&admission.node_did),
        )
        .await
        .unwrap()
        .unwrap();
        let response = crate::tools::call::control::cancel_loaded_background_tool_call(
            node.clone(),
            &registry,
            lifecycle,
            "later operator stop",
        )
        .await
        .unwrap();
        assert!(
            matches!(response, crate::tools::call::control::CancelBackgroundToolCallOutcome::AlreadyTerminal { ref state } if state == expected)
        );
        let row = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &doc,
            &admission.node_did,
            &session,
            Some(&admission.node_did),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(row.state().as_str(), expected);
        assert!(!registry.contains(&call, &doc).await);
        let key =
            crate::tools::background::completion::background_completion_notification_message_key(
                &doc, "tool",
            );
        let escaped = crate::graphql::escape_graphql_string(&key);
        let result = crate::config_client::ConfigAccess::local(node.clone()).execute(&format!(r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#)).await.unwrap();
        assert_eq!(result["data"]["AgentMessage"].as_array().unwrap().len(), 1);
        process.finish().await;
        drop(request_owner);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(admission.path);
    }
}

#[tokio::test]
async fn confirmed_background_stop_survives_terminal_write_failure() {
    let subscriber = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::WARN)
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    let model = crate::lean_vocab_test::lean_process_stop_cases()
        .iter()
        .find(|case| case.live_evidence == "never_spawned" && case.worker_released)
        .unwrap();
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "background-terminal-failure".into(),
        tool_name: Some("bash".into()),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let tool = admission.tool;
    let call = tool.tool_call_id().to_owned();
    let doc = tool.doc_id().unwrap().to_owned();
    let session = tool.session_id().to_owned();
    let registry = BackgroundExecutionRegistry::default();
    let reservation = registry
        .reserve(call.clone(), doc.clone(), CancellationToken::new())
        .unwrap();
    let _recorder = registry.process_recorder(&call, &doc);
    registry.set_cancellation_reason(&call, &doc, "explicit stop", false);
    let (_, count) = crate::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(1),
        finish_background_execution(
            tool,
            node.clone(),
            registry.live_outputs.clone(),
            registry.clone(),
            reservation,
            Ok(crate::tools::call::lifecycle::ToolOutcome::Cancelled),
        ),
    )
    .await;
    assert_eq!(count, 1);
    assert!(registry.contains(&call, &doc).await);
    let before_retry = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &doc,
        &admission.node_did,
        &session,
        Some(&admission.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        before_retry.is_running(),
        "failed terminal transaction must leave the invocation running"
    );
    registry.retire_confirmed_stop(&call, &doc, model.retire_before_commit);
    assert!(registry.contains(&call, &doc).await);
    let entry = registry.lock_executions().get(&doc).unwrap().clone();
    assert!(
        *entry.worker_release_tx.borrow(),
        "worker release must be observable independently"
    );
    assert_eq!(
        *entry.completion_tx.borrow(),
        model.retained_completion_notified
    );
    let waiter = tokio::spawn({
        let registry = registry.clone();
        let call = call.clone();
        async move { registry.wait_for_completion(&call).await }
    });
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "durable completion must wait for retained settlement"
    );
    registry.recover_retained_foreground().await;
    waiter.await.unwrap();
    assert_eq!(
        *entry.completion_tx.borrow(),
        model.retired_completion_notified
    );
    let row = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &doc,
        &admission.node_did,
        &session,
        Some(&admission.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.state().as_str(), model.durable_state);
    assert_eq!(row.request_id(), request_owner.request().request_id);
    assert!(!registry.contains(&call, &doc).await);
    let key = crate::tools::background::completion::background_completion_notification_message_key(
        &doc, "tool",
    );
    let escaped = crate::graphql::escape_graphql_string(&key);
    let result = crate::config_client::ConfigAccess::local(node.clone())
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    let notifications = result["data"]["AgentMessage"].as_array().unwrap();
    assert_eq!(notifications.len(), 1);
    drop(request_owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}

#[tokio::test]
async fn live_process_identity_survives_host_record_write_failure() {
    let directory = tempfile::tempdir().unwrap();
    let invalid_directory = directory.path().join("not-a-directory");
    tokio::fs::write(&invalid_directory, b"file").await.unwrap();
    let registry = BackgroundExecutionRegistry::default().with_process_records(invalid_directory);
    let reservation = registry
        .reserve(
            "volatile-call".into(),
            "volatile-doc".into(),
            CancellationToken::new(),
        )
        .unwrap();
    let process = OwnedTestProcess::spawn(Some(
        registry.process_recorder("volatile-call", "volatile-doc"),
    ))
    .await;
    let owned = registry
        .process_record("volatile-call", "volatile-doc")
        .expect("live identity remains owned");
    assert_eq!(owned.identity, process.identity);
    assert!(registry
        .process_record("volatile-call", "wrong-doc")
        .is_none());
    let (before, after) = owned.identity.stop().await;
    assert_eq!(
        crate::tools::exec::ProcessStopOutcome::from_observations(before, after),
        crate::tools::exec::ProcessStopOutcome::Stopped
    );
    process.finish().await;
    drop(reservation);
}

#[tokio::test]
async fn native_failure_decision_precedes_cancel_during_cleanup() {
    let model = crate::lean_vocab_test::lean_process_stop_cases()
        .iter()
        .find(|case| case.live_evidence == "exited" && case.worker_released)
        .unwrap();
    let fallback_registry = BackgroundExecutionRegistry::default();
    let fallback_reservation = fallback_registry
        .reserve(
            "fallback".into(),
            "fallback-doc".into(),
            CancellationToken::new(),
        )
        .unwrap();
    let fallback_recorder = fallback_registry.process_recorder("fallback", "fallback-doc");
    crate::tools::exec::ownership::record_failure_decision(Some(&fallback_recorder));
    fallback_registry.set_cancellation_reason("fallback", "fallback-doc", "later cancel", false);
    assert_eq!(
        fallback_registry.has_pending_failure("fallback", "fallback-doc"),
        model.pending_failure_beats_cancel
    );
    assert!(matches!(
        fallback_registry.finalize_pending_failure("fallback", "fallback-doc"),
        Some(crate::tools::call::lifecycle::ToolOutcome::Failed { .. })
    ));
    assert_eq!(
        !fallback_registry.has_pending_failure("fallback", "fallback-doc"),
        model.pending_failure_resolves
    );
    drop(fallback_reservation);
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "native-failure-before-cancel".into(),
        tool_name: Some("bash".into()),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let lifecycle = admission.tool;
    let call = lifecycle.tool_call_id().to_owned();
    let doc = lifecycle.doc_id().unwrap().to_owned();
    let session = lifecycle.session_id().to_owned();
    let registry = BackgroundExecutionRegistry::default();
    let reservation = registry
        .reserve(call.clone(), doc.clone(), CancellationToken::new())
        .unwrap();
    let recorder = registry.process_recorder(&call, &doc);
    let process = OwnedTestProcess::spawn(Some(recorder.clone())).await;
    let (decided, observed) = tokio::sync::oneshot::channel();
    let (release, blocked) = tokio::sync::oneshot::channel();
    let worker_registry = registry.clone();
    let worker_call = call.clone();
    let worker_doc = doc.clone();
    let worker = tokio::spawn(async move {
        crate::tools::exec::ownership::record_failure_decision(Some(&recorder));
        decided.send(()).unwrap();
        blocked.await.unwrap();
        worker_registry.set_stop_receipt(
            &worker_call,
            &worker_doc,
            crate::tools::call::hook::RetainedStopReceipt::Failure(
                crate::tools::call::lifecycle::ToolOutcome::tool_returned_error(
                    "native wait failed",
                ),
            ),
        );
    });
    observed.await.unwrap();
    registry.set_cancellation_reason(&call, &doc, "later stop", false);
    assert_eq!(
        registry.has_pending_failure(&call, &doc),
        model.pending_failure_beats_cancel
    );
    release.send(()).unwrap();
    worker.await.unwrap();
    assert_eq!(
        !registry.has_pending_failure(&call, &doc),
        model.pending_failure_resolves
    );
    reservation.retain_stopped_worker(lifecycle.clone(), false);
    let _ = crate::tools::call::control::cancel_loaded_background_tool_call(
        node.clone(),
        &registry,
        lifecycle,
        "later stop",
    )
    .await
    .unwrap();
    let row = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &doc,
        &admission.node_did,
        &session,
        Some(&admission.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.state().as_str(), "failed");
    process.finish().await;
    drop(request_owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}

#[tokio::test]
async fn failed_replay_adopts_competing_durable_terminal() {
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "competing-terminal-replay".into(),
        tool_name: Some("bash".into()),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let mut stale = admission.tool;
    let mut competing = stale.clone();
    let call = stale.tool_call_id().to_owned();
    let doc = stale.doc_id().unwrap().to_owned();
    let registry = BackgroundExecutionRegistry::default();
    let reservation = registry
        .reserve(call.clone(), doc.clone(), CancellationToken::new())
        .unwrap();
    let _recorder = registry.process_recorder(&call, &doc);
    registry.set_stop_receipt(
        &call,
        &doc,
        crate::tools::call::hook::RetainedStopReceipt::Failure(
            crate::tools::call::lifecycle::ToolOutcome::tool_returned_error("earlier failure"),
        ),
    );
    reservation.retain_stopped_worker(stale.clone(), false);
    let execution = registry.lock_executions().get(&doc).unwrap().clone();
    execution.process_evidence.exited();
    registry.remember_confirmed_stop(&execution);
    assert!(competing
        .cancel_during_run(CancelCause::UserCancelled)
        .await
        .unwrap());
    assert!(registry.settle_retained_stop(&mut stale).await.unwrap());
    assert!(stale.is_cancelled());
    assert!(!registry.contains(&call, &doc).await);
    drop(request_owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}

#[tokio::test]
async fn timeout_decision_is_owned_before_cleanup_and_later_cancel() {
    use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
    struct PendingNative;
    impl ToolDyn for PendingNative {
        fn name(&self) -> String {
            "pending-native".into()
        }
        fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
            Box::pin(async {
                ToolDefinition {
                    name: "pending-native".into(),
                    description: String::new(),
                    parameters: serde_json::json!({}),
                }
            })
        }
        fn call<'a>(&'a self, _: String) -> BoxFuture<'a, Result<String, ToolError>> {
            Box::pin(std::future::pending())
        }
    }
    let model = crate::lean_vocab_test::lean_process_stop_cases()
        .iter()
        .find(|case| case.live_evidence == "exited" && case.worker_released)
        .unwrap();
    let registry = BackgroundExecutionRegistry::default();
    let reservation = registry
        .reserve(
            "deadline-call".into(),
            "deadline-doc".into(),
            CancellationToken::new(),
        )
        .unwrap();
    let (decided, observed) = tokio::sync::oneshot::channel();
    let sender = std::sync::Arc::new(std::sync::Mutex::new(Some(decided)));
    let worker_registry = registry.clone();
    let worker_sender = sender.clone();
    let token = CancellationToken::new();
    let worker_token = token.clone();
    let deadline = chrono::Utc::now() + chrono::Duration::seconds(1);
    tokio::time::pause();
    let worker = tokio::spawn(async move {
        crate::tools::call::lifecycle::runtime::scope_request_tool_execution(
            Some(deadline),
            worker_token,
            crate::tools::call::lifecycle::runtime::call_tool_managed_with_cleanup(
                &PendingNative,
                "{}".into(),
                move |outcome| {
                    if let Some(crate::tools::call::lifecycle::ToolOutcome::TimedOut { .. }) =
                        outcome
                    {
                        worker_registry.set_stop_receipt(
                            "deadline-call",
                            "deadline-doc",
                            crate::tools::call::hook::RetainedStopReceipt::Timeout,
                        );
                        if let Some(sender) = worker_sender.lock().unwrap().take() {
                            let _ = sender.send(());
                        }
                    }
                    true
                },
            ),
        )
        .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    observed.await.unwrap();
    registry.set_cancellation_reason("deadline-call", "deadline-doc", "later stop", false);
    token.cancel();
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    assert!(matches!(
        worker.await.unwrap(),
        crate::tools::call::lifecycle::ToolOutcome::TimedOut { .. }
    ));
    tokio::time::resume();
    let entry = registry
        .lock_executions()
        .get("deadline-doc")
        .unwrap()
        .clone();
    let preserved = matches!(
        entry.requested_stop.lock().unwrap().as_ref(),
        Some(crate::tools::call::hook::RetainedStopReceipt::Timeout)
    );
    assert_eq!(preserved, model.preserve_timeout);
    drop(reservation);
}

#[tokio::test]
async fn failed_terminal_write_keeps_prepared_failure_presentation_for_retry() {
    use gents_loop::live_output::{CapturedChannel, CommandPresentationLayout, LiveOutputStream};
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "prepared-failure-retry".into(),
        tool_name: Some("bash".into()),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let tool = admission.tool;
    let call = tool.tool_call_id().to_owned();
    let doc = tool.doc_id().unwrap().to_owned();
    let session = tool.session_id().to_owned();
    let registry = BackgroundExecutionRegistry::default();
    let writer = registry
        .live_outputs
        .canonical_writer_for(tool.tool_output_binding().unwrap())
        .await;
    writer.append(LiveOutputStream::Stdout, b"hello").await;
    let result = "stdout:\nhello\nstderr:\n(empty)";
    writer
        .prepare_command_presentation(
            result,
            CommandPresentationLayout::Labeled {
                head: "",
                json_string: false,
            },
            CapturedChannel {
                raw: "hello",
                rendered: "hello",
                shown: 0..5,
            },
            CapturedChannel {
                raw: "",
                rendered: "",
                shown: 0..0,
            },
        )
        .await
        .unwrap();
    let prepared = registry
        .live_outputs
        .registry
        .prepared_presentation(&doc, result)
        .await
        .unwrap()
        .unwrap();
    let reservation = registry
        .reserve(call.clone(), doc.clone(), CancellationToken::new())
        .unwrap();
    let _recorder = registry.process_recorder(&call, &doc);
    let (_, count) = crate::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(1),
        finish_background_execution(
            tool,
            node.clone(),
            registry.live_outputs.clone(),
            registry.clone(),
            reservation,
            Ok(crate::tools::call::lifecycle::ToolOutcome::tool_returned_error(result)),
        ),
    )
    .await;
    assert_eq!(count, 1);
    assert!(registry.contains(&call, &doc).await);
    assert_eq!(
        registry
            .live_outputs
            .registry
            .prepared_presentation(&doc, result)
            .await
            .unwrap(),
        Some(prepared)
    );
    registry.recover_retained_foreground().await;
    assert!(!registry.contains(&call, &doc).await);
    assert!(registry
        .live_outputs
        .registry
        .prepared_presentation(&doc, result)
        .await
        .unwrap()
        .is_none());
    let shown = crate::tools::call::lifecycle::query::load_tool_call_presentation(
        &crate::config_client::ConfigAccess::local(node.clone()),
        &doc,
        &admission.node_did,
        &session,
        Some(&admission.node_did),
    )
    .await
    .unwrap();
    assert_eq!(shown.result.as_deref(), Some(result));
    let key = crate::tools::background::completion::background_completion_notification_message_key(
        &doc, "tool",
    );
    let escaped = crate::graphql::escape_graphql_string(&key);
    let notification = crate::config_client::ConfigAccess::local(node.clone())
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    assert_eq!(
        notification["data"]["AgentMessage"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    drop(request_owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}

#[tokio::test]
async fn completed_background_call_resolves_parent_from_physical_request() {
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "completed-background-parent-binding".into(),
        tool_name: Some("read_file".into()),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let tool = admission.tool;
    let doc = tool.doc_id().unwrap().to_owned();
    assert!(
        tool.request_id().is_empty(),
        "accepted invocation begins with its physical request binding"
    );
    assert_eq!(
        tool.calling_request_id().await.unwrap(),
        request_owner.request().request_id
    );
    let registry = BackgroundExecutionRegistry::default();
    let reservation = registry
        .reserve(
            tool.tool_call_id().to_owned(),
            doc.clone(),
            CancellationToken::new(),
        )
        .unwrap();
    finish_background_execution(
        tool,
        node.clone(),
        registry.live_outputs.clone(),
        registry.clone(),
        reservation,
        Ok(crate::tools::call::lifecycle::ToolOutcome::Completed(
            "done".into(),
        )),
    )
    .await;
    let key = crate::tools::background::completion::background_completion_notification_message_key(
        &doc, "tool",
    );
    let escaped = crate::graphql::escape_graphql_string(&key);
    let result = crate::config_client::ConfigAccess::local(node.clone())
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    assert_eq!(result["data"]["AgentMessage"].as_array().unwrap().len(), 1);
    drop(request_owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}
