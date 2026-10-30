use gents_protocol::process_control::{OperatorProcessControlRequest, ProcessControlOutcome};
use tokio_util::sync::CancellationToken;

use super::admission_fixture::{published_admission_with_owner, PublishedAdmissionOptions};
use super::{AwaitMode, ToolCallLifecycle};
use crate::tools::call::control::operator_process_control;
use crate::tools::call::hook::BackgroundExecutionRegistry;

#[cfg(unix)]
#[tokio::test]
async fn operator_cancel_races_completion_without_replacing_terminal_state() {
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "operator-cancel-completion-race".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let mut tool = admission.tool;
    let request = OperatorProcessControlRequest {
        node_did: admission.node_did.clone(),
        session_id: tool.session_id().to_owned(),
        tool_call_id: tool.tool_call_id().to_owned(),
        tool_call_doc_id: tool.doc_id().unwrap().to_owned(),
    };
    let mut stale = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &request.tool_call_doc_id,
        &request.node_did,
        &request.session_id,
        Some(&admission.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    let records = tempfile::tempdir().unwrap();
    let registry =
        BackgroundExecutionRegistry::default().with_process_records(records.path().into());
    let token = CancellationToken::new();
    let reservation = registry
        .reserve(
            request.tool_call_id.clone(),
            request.tool_call_doc_id.clone(),
            token,
        )
        .unwrap();
    let process = crate::managed_exec::ownership::test_support::OwnedTestProcess::spawn(Some(
        registry.process_recorder(&request.tool_call_id, &request.tool_call_doc_id),
    ))
    .await;

    let (completed, cancelled) = tokio::join!(
        async {
            let result = tool.complete("finished").await;
            process.finish().await;
            drop(reservation);
            result
        },
        operator_process_control(node.clone(), &registry, Some(&admission.node_did), &request),
    );
    let cancelled = cancelled.unwrap();
    let row = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &request.tool_call_doc_id,
        &request.node_did,
        &request.session_id,
        Some(&admission.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(row.is_terminal());
    let terminal = row.state().as_str().to_owned();
    assert!(matches!(terminal.as_str(), "completed" | "cancelled"));
    if let Err(error) = completed {
        assert_eq!(terminal, "cancelled");
        assert!(error
            .to_string()
            .contains("terminal raw tool result is not an exact extension"));
    }
    match cancelled {
        ProcessControlOutcome::Cancelled { .. } => assert_eq!(terminal, "cancelled"),
        ProcessControlOutcome::AlreadyTerminal { state } => assert_eq!(state, terminal),
        other => panic!("unexpected operator outcome: {other:?}"),
    }
    let late = stale.complete("late completion").await.unwrap_err();
    assert!(late
        .to_string()
        .contains("terminal raw tool result is not an exact extension"));
    let repeated =
        operator_process_control(node.clone(), &registry, Some(&admission.node_did), &request)
            .await
            .unwrap();
    assert_eq!(
        repeated,
        ProcessControlOutcome::AlreadyTerminal {
            state: terminal.clone()
        }
    );
    let reloaded = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &request.tool_call_doc_id,
        &request.node_did,
        &request.session_id,
        Some(&admission.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reloaded.state().as_str(), terminal);
    drop(request_owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}

#[cfg(unix)]
#[tokio::test]
async fn operator_cancel_and_completion_preserve_each_winning_terminal() {
    for cancel_first in [false, true] {
        let (admission, request_owner) =
            published_admission_with_owner(PublishedAdmissionOptions {
                name: format!("operator-terminal-order-{cancel_first}"),
                real_identity: true,
                await_mode: AwaitMode::Background,
                ..Default::default()
            })
            .await
            .unwrap();
        let node = admission.node.clone();
        let mut tool = admission.tool;
        let request = OperatorProcessControlRequest {
            node_did: admission.node_did.clone(),
            session_id: tool.session_id().to_owned(),
            tool_call_id: tool.tool_call_id().to_owned(),
            tool_call_doc_id: tool.doc_id().unwrap().to_owned(),
        };
        let records = tempfile::tempdir().unwrap();
        let registry =
            BackgroundExecutionRegistry::default().with_process_records(records.path().into());
        let token = CancellationToken::new();
        let reservation = registry
            .reserve(
                request.tool_call_id.clone(),
                request.tool_call_doc_id.clone(),
                token.clone(),
            )
            .unwrap();
        if cancel_first {
            let process = crate::managed_exec::ownership::test_support::OwnedTestProcess::spawn(
                Some(registry.process_recorder(&request.tool_call_id, &request.tool_call_doc_id)),
            )
            .await;
            let worker = tokio::spawn(async move {
                token.cancelled().await;
                process.finish().await;
                drop(reservation);
            });
            assert_eq!(
                operator_process_control(
                    node.clone(),
                    &registry,
                    Some(&admission.node_did),
                    &request
                )
                .await
                .unwrap(),
                ProcessControlOutcome::Cancelled {
                    live_execution_cancelled: true
                },
            );
            worker.await.unwrap();
            let late = tool.complete("late completion").await.unwrap_err();
            assert!(late
                .to_string()
                .contains("terminal raw tool result is not an exact extension"));
        } else {
            tool.complete("first completion").await.unwrap();
            drop(reservation);
            assert_eq!(
                operator_process_control(
                    node.clone(),
                    &registry,
                    Some(&admission.node_did),
                    &request
                )
                .await
                .unwrap(),
                ProcessControlOutcome::AlreadyTerminal {
                    state: "completed".into()
                },
            );
        }
        let terminal = if cancel_first {
            "cancelled"
        } else {
            "completed"
        };
        let row = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &request.tool_call_doc_id,
            &request.node_did,
            &request.session_id,
            Some(&admission.node_did),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(row.state().as_str(), terminal);
        drop(request_owner);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(admission.path);
    }
}

#[tokio::test]
async fn operator_unverified_stop_preserves_running_tool() {
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "operator-unverified-stop".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let tool = admission.tool;
    let request = OperatorProcessControlRequest {
        node_did: admission.node_did.clone(),
        session_id: tool.session_id().to_owned(),
        tool_call_id: tool.tool_call_id().to_owned(),
        tool_call_doc_id: tool.doc_id().unwrap().to_owned(),
    };
    let release_observed = std::sync::Arc::new(tokio::sync::Notify::new());
    let registry =
        BackgroundExecutionRegistry::default().with_stop_release_observer(release_observed.clone());
    let token = CancellationToken::new();
    let reservation = registry
        .reserve(
            request.tool_call_id.clone(),
            request.tool_call_doc_id.clone(),
            token.clone(),
        )
        .unwrap();
    let expected = crate::lean_vocab_test::lean_process_stop_cases()
        .iter()
        .find(|case| case.before == "running" && case.after == "running")
        .expect("Lean running process observation");
    let mut control = tokio::spawn({
        let node = node.clone();
        let registry = registry.clone();
        let request = request.clone();
        async move { operator_process_control(node, &registry, Some(&request.node_did), &request).await }
    });
    tokio::select! {
        result = &mut control => panic!("control ended before signalling the live execution: {result:?}"),
        _ = token.cancelled() => {}
    }
    tokio::time::pause();
    tokio::time::advance(crate::tools::call::hook::LIVE_STOP_RELEASE_WAIT).await;
    release_observed.notified().await;
    tokio::time::resume();
    let outcome = control.await.unwrap().unwrap();
    assert_eq!(outcome, ProcessControlOutcome::Unverified);
    let row = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &request.tool_call_doc_id,
        &request.node_did,
        &request.session_id,
        Some(&admission.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.state().as_str(), expected.durable_state);
    drop(reservation);
    drop(request_owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}

#[tokio::test]
async fn operator_cancels_released_native_future_without_process_record() {
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "operator-generic-native-stop".into(),
        tool_name: Some("read_file".into()),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let tool = admission.tool;
    let request = OperatorProcessControlRequest {
        node_did: admission.node_did.clone(),
        session_id: tool.session_id().to_owned(),
        tool_call_id: tool.tool_call_id().to_owned(),
        tool_call_doc_id: tool.doc_id().unwrap().to_owned(),
    };
    let registry = BackgroundExecutionRegistry::default();
    let token = CancellationToken::new();
    let reservation = registry
        .reserve(
            request.tool_call_id.clone(),
            request.tool_call_doc_id.clone(),
            token.clone(),
        )
        .unwrap();
    let worker = tokio::spawn(async move {
        token.cancelled().await;
        drop(reservation);
    });
    let outcome =
        operator_process_control(node.clone(), &registry, Some(&admission.node_did), &request)
            .await
            .unwrap();
    worker.await.unwrap();
    let row = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &request.tool_call_doc_id,
        &request.node_did,
        &request.session_id,
        Some(&admission.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    let expected = crate::lean_vocab_test::lean_process_stop_cases()
        .iter()
        .find(|case| case.before == "running" && case.after == "exited")
        .unwrap();
    assert_eq!(row.state().as_str(), expected.durable_state);
    assert_eq!(
        outcome,
        ProcessControlOutcome::Cancelled {
            live_execution_cancelled: true
        }
    );
    drop(request_owner);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(admission.path);
}
