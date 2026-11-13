use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, ToolControlReceiptOutcome,
};
use tokio_util::sync::CancellationToken;

use super::admission_fixture::{published_admission_with_owner, PublishedAdmissionOptions};
use super::{AwaitMode, ToolCallLifecycle};
use crate::call::control::client::{apply_claimed, ClaimedTarget};
use crate::call::hook::BackgroundExecutionRegistry;

/// Applies a claimed `process_stop` the way the client-signed control owner
/// does after verifying the client's intent: as the selected Agent, through
/// the existing background process owner.
async fn client_stop(
    owner: &gents_session::request::lifecycle::RequestLifecycle,
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
    access: &gents_store::config_client::ConfigAccess,
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
fn cancel_reply_of(outcome: &ProcessControlOutcome) -> gents_session::exec::CancelProcessReply {
    use gents_session::exec::CancelProcessReply;
    match outcome {
        ProcessControlOutcome::Cancelled { .. } => CancelProcessReply::Cancelled,
        ProcessControlOutcome::Lost => CancelProcessReply::Lost,
        ProcessControlOutcome::Unverified => CancelProcessReply::Unverified,
        other => panic!("{other:?} is not a background process stop"),
    }
}

async fn load_as_agent(
    owner: &gents_session::request::lifecycle::RequestLifecycle,
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
    let process = gents_session::exec::ownership::test_support::OwnedTestProcess::spawn(Some(
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
async fn process_control_terminal_order_matches_lean_model() {
    for case in gents_lean_vocab::families::process_control_terminal_order::cases() {
        let cancel_first = case.input.cancel_first;
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
        let (stop, completion) = if cancel_first {
            let process = gents_session::exec::ownership::test_support::OwnedTestProcess::spawn(
                Some(registry.process_recorder(&request.tool_call_id, &request.tool_call_doc_id)),
            )
            .await;
            let worker = tokio::spawn(async move {
                token.cancelled().await;
                process.finish().await;
                drop(reservation);
            });
            let stop = client_stop(&request_owner, &registry, &request)
                .await
                .unwrap();
            worker.await.unwrap();
            let late = tool.complete("late completion").await;
            (stop, late)
        } else {
            let completion = tool.complete("first completion").await;
            drop(reservation);
            let stop = client_stop(&request_owner, &registry, &request)
                .await
                .unwrap();
            (stop, completion)
        };
        let (stop_cancelled, live_execution_cancelled, already_terminal_state) = match stop {
            ProcessControlOutcome::Cancelled {
                live_execution_cancelled,
            } => (true, Some(live_execution_cancelled), None),
            ProcessControlOutcome::AlreadyTerminal { state } => (false, None, Some(state)),
            other => panic!("unexpected control outcome: {other:?}"),
        };
        let (completion_accepted, completion_refused_terminal) = match completion {
            Ok(_) => (true, false),
            Err(error) => (
                false,
                error
                    .to_string()
                    .contains("terminal raw tool result is not an exact extension"),
            ),
        };
        let row = load_as_agent(&request_owner, &request, requester_did.as_deref()).await;
        case.check(
            gents_lean_vocab::families::process_control_terminal_order::RaceOutput {
                stop_cancelled,
                live_execution_cancelled,
                already_terminal_state,
                completion_accepted,
                completion_refused_terminal,
                terminal_state: row.state().as_str().to_owned(),
            },
        );
        drop(request_owner);
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(admission.path);
    }
}

#[tokio::test]
async fn client_unverified_stop_preserves_running_tool() {
    for case in gents_lean_vocab::families::process_stop_client_unverified::cases::<
        gents_session::exec::CancelProcessReply,
        String,
        String,
        String,
    >() {
        let (admission, request_owner) =
            published_admission_with_owner(PublishedAdmissionOptions {
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
        let registry = BackgroundExecutionRegistry::default()
            .with_stop_release_observer(release_observed.clone());
        let token = CancellationToken::new();
        let reservation = registry
            .reserve(
                request.tool_call_id.clone(),
                request.tool_call_doc_id.clone(),
                token.clone(),
            )
            .unwrap();
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
        tokio::time::advance(crate::call::hook::LIVE_STOP_RELEASE_WAIT).await;
        release_observed.notified().await;
        tokio::time::resume();
        let outcome = control.await.unwrap().unwrap();
        let row = load_as_agent(&request_owner, &request, requester_did.as_deref()).await;
        case.check(
            gents_lean_vocab::families::process_stop_client_unverified::ReplyOutput {
                cancel_reply: cancel_reply_of(&outcome),
                durable_state: row.state().as_str().to_owned(),
            },
        );
        drop(reservation);
        drop(request_owner);
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(admission.path);
    }
}

#[tokio::test]
async fn client_cancels_released_native_future_without_process_record() {
    for case in gents_lean_vocab::families::process_stop_client_released_native::cases::<
        gents_session::exec::CancelProcessReply,
        String,
        String,
        String,
    >() {
        let (admission, request_owner) =
            published_admission_with_owner(PublishedAdmissionOptions {
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
        if let ProcessControlOutcome::Cancelled {
            live_execution_cancelled,
        } = outcome
        {
            assert!(live_execution_cancelled);
        }
        worker.await.unwrap();
        let row = load_as_agent(&request_owner, &request, requester_did.as_deref()).await;
        case.check(
            gents_lean_vocab::families::process_stop_client_released_native::ReplyOutput {
                cancel_reply: cancel_reply_of(&outcome),
                durable_state: row.state().as_str().to_owned(),
            },
        );
        drop(request_owner);
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(admission.path);
    }
}
