use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, ToolControlReceiptOutcome,
};
use tokio_util::sync::CancellationToken;

use super::admission_fixture::{published_admission_with_owner, PublishedAdmissionOptions};
use super::{AwaitMode, ToolCallLifecycle};
use crate::tools::call::control::client::{apply_claimed, ClaimedTarget};
use crate::tools::call::hook::BackgroundExecutionRegistry;

/// Applies a claimed `process_stop` the way the client-signed control owner
/// does after verifying the client's intent: as the selected Agent, through
/// the existing background process owner.
async fn client_stop(
    owner: &crate::request::lifecycle::RequestLifecycle,
    registry: &BackgroundExecutionRegistry,
    request: &OperatorProcessControlRequest,
) -> anyhow::Result<ProcessControlOutcome> {
    let access = owner.access()?;
    let node = access
        .local_node()
        .cloned()
        .expect("embedded request owner");
    let requester_did = owner.request().requester_did.clone();
    stop_as(
        &node,
        &access,
        &owner.actor_did()?,
        registry,
        request,
        requester_did.as_deref(),
    )
    .await
}

async fn stop_as(
    node: &std::sync::Arc<defra_node::Node>,
    access: &crate::ConfigAccess,
    agent: &::identity::Did,
    registry: &BackgroundExecutionRegistry,
    request: &OperatorProcessControlRequest,
    requester_did: Option<&str>,
) -> anyhow::Result<ProcessControlOutcome> {
    let target = ClaimedTarget {
        node_did: &request.node_did,
        session_id: &request.session_id,
        requester_did,
        tool_call_id: &request.tool_call_id,
        tool_call_doc_id: &request.tool_call_doc_id,
        action: "process_stop",
    };
    match apply_claimed(node, access, registry, agent, target).await? {
        ToolControlReceiptOutcome::Process(outcome) => Ok(outcome),
        other => anyhow::bail!("process stop produced {other:?}"),
    }
}

/// Loads as the owning Agent and binds its signer, as the runtime's tool owner does.
async fn load_as_agent(
    owner: &crate::request::lifecycle::RequestLifecycle,
    request: &OperatorProcessControlRequest,
    requester_did: Option<&str>,
) -> ToolCallLifecycle {
    let access = owner.access().unwrap();
    let mut lifecycle = ToolCallLifecycle::load_by_doc_id_as(
        access
            .local_node()
            .cloned()
            .expect("embedded request owner"),
        owner.actor_did().unwrap(),
        &request.tool_call_doc_id,
        &request.node_did,
        &request.session_id,
        requester_did,
    )
    .await
    .unwrap()
    .expect("selected agent can read its tool call");
    lifecycle
        .bind_agent_signer(owner.managed_agent_signer())
        .unwrap();
    lifecycle
}

#[cfg(unix)]
#[tokio::test]
async fn client_cancel_races_completion_without_replacing_terminal_state() {
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "operator-cancel-completion-race".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let requester_did = request_owner.request().requester_did.clone();
    let node = admission.node.clone();
    let mut tool = admission.tool;
    let request = OperatorProcessControlRequest {
        node_did: admission.node_did.clone(),
        session_id: tool.session_id().to_owned(),
        tool_call_id: tool.tool_call_id().to_owned(),
        tool_call_doc_id: tool.doc_id().unwrap().to_owned(),
    };
    let mut stale = load_as_agent(&request_owner, &request, requester_did.as_deref()).await;
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
    let process = crate::tools::exec::ownership::test_support::OwnedTestProcess::spawn(Some(
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
        client_stop(&request_owner, &registry, &request),
    );
    let cancelled = cancelled.unwrap();
    let row = load_as_agent(&request_owner, &request, requester_did.as_deref()).await;
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
        other => panic!("unexpected control outcome: {other:?}"),
    }
    let late = stale.complete("late completion").await.unwrap_err();
    assert!(late
        .to_string()
        .contains("terminal raw tool result is not an exact extension"));
    let repeated = client_stop(&request_owner, &registry, &request)
        .await
        .unwrap();
    assert_eq!(
        repeated,
        ProcessControlOutcome::AlreadyTerminal {
            state: terminal.clone()
        }
    );
    let reloaded = load_as_agent(&request_owner, &request, requester_did.as_deref()).await;
    assert_eq!(reloaded.state().as_str(), terminal);
    drop(request_owner);
    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(admission.path);
}

#[cfg(unix)]
#[tokio::test]
async fn client_cancel_and_completion_preserve_each_winning_terminal() {
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
        let requester_did = request_owner.request().requester_did.clone();
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
            let process = crate::tools::exec::ownership::test_support::OwnedTestProcess::spawn(
                Some(registry.process_recorder(&request.tool_call_id, &request.tool_call_doc_id)),
            )
            .await;
            let worker = tokio::spawn(async move {
                token.cancelled().await;
                process.finish().await;
                drop(reservation);
            });
            assert_eq!(
                client_stop(&request_owner, &registry, &request)
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
                client_stop(&request_owner, &registry, &request)
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
        let row = load_as_agent(&request_owner, &request, requester_did.as_deref()).await;
        assert_eq!(row.state().as_str(), terminal);
        drop(request_owner);
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(admission.path);
    }
}

#[tokio::test]
async fn client_unverified_stop_preserves_running_tool() {
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "operator-unverified-stop".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let requester_did = request_owner.request().requester_did.clone();
    let agent_access = request_owner.access().unwrap();
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
        let node_for_control = node.clone();
        let agent = request_owner.actor_did().unwrap();
        let registry = registry.clone();
        let request = request.clone();
        let requester_did = requester_did.clone();
        async move {
            stop_as(
                &node_for_control,
                &agent_access,
                &agent,
                &registry,
                &request,
                requester_did.as_deref(),
            )
            .await
        }
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
    let row = load_as_agent(&request_owner, &request, requester_did.as_deref()).await;
    assert_eq!(row.state().as_str(), expected.durable_state);
    drop(reservation);
    drop(request_owner);
    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(admission.path);
}

#[tokio::test]
async fn client_cancels_released_native_future_without_process_record() {
    let (admission, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "operator-generic-native-stop".into(),
        tool_name: Some("read_file".into()),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let requester_did = request_owner.request().requester_did.clone();
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
    let outcome = client_stop(&request_owner, &registry, &request)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        ProcessControlOutcome::Cancelled {
            live_execution_cancelled: true
        },
    );
    worker.await.unwrap();
    let row = load_as_agent(&request_owner, &request, requester_did.as_deref()).await;
    let expected = crate::lean_vocab_test::lean_process_stop_cases()
        .iter()
        .find(|case| case.before == "running" && case.after == "exited")
        .unwrap();
    assert_eq!(row.state().as_str(), expected.durable_state);
    drop(request_owner);
    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(admission.path);
}
