use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobReadError};

use crate::store::config_client::{ConfigAccess, ConfigClientError};
use crate::tools::background::job::control::control_by_doc_id;
use crate::tools::background::status::ProcessControlScope;
use crate::tools::call::hook::BackgroundExecutionRegistry;
use crate::tools::call::lifecycle::{CancelCause, ToolCallLifecycle, ToolRowKind};
use gents_protocol::tool_call::AwaitMode;

pub mod client;

pub use gents_protocol::process_control::ProcessControlOutcome as CancelBackgroundToolCallOutcome;

/// `agent` is the owning Agent's managed access: ACP hides the Agent's tool
/// calls from every other actor and their writes require its signer.
pub async fn cancel_session_background_process(
    agent: &ConfigAccess,
    executions: &BackgroundExecutionRegistry,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    tool_call_id: &str,
) -> Result<CancelBackgroundToolCallOutcome> {
    let Some((node, lifecycle)) =
        load_owned_background_call(agent, session_id, tool_call_id).await?
    else {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    };
    let scope = crate::tools::background::status::ProcessControlScope {
        request_id: String::new(),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: requester_did.map(str::to_owned),
    };
    if !scope.authorizes(
        lifecycle.session_id(),
        lifecycle.node_did(),
        lifecycle.requester_did(),
    ) || lifecycle.kind() == crate::tools::call::lifecycle::ToolRowKind::SessionMessage
    {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    }
    cancel_loaded_background_tool_call(node, executions, lifecycle, "explicit_cancel").await
}

/// `agent` is the owning Agent's managed access; see
/// [`cancel_session_background_process`].
pub async fn cancel_background_tool_call(
    agent: &ConfigAccess,
    background_executions: &BackgroundExecutionRegistry,
    session_id: &str,
    tool_call_id: &str,
) -> Result<CancelBackgroundToolCallOutcome> {
    let Some((node, lifecycle)) =
        load_owned_background_call(agent, session_id, tool_call_id).await?
    else {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    };

    cancel_loaded_background_tool_call(node, background_executions, lifecycle, "explicit_cancel")
        .await
}

/// Splits the owning Agent's embedded managed access into the parts a tool
/// lifecycle binds; an access without the Agent's signer cannot write its rows.
fn managed_agent_parts(
    agent: &ConfigAccess,
) -> Result<(
    Arc<EmbeddedNode>,
    identity::Did,
    crate::store::identity::ManagedAgentSigner,
)> {
    let node = agent
        .local_node()
        .cloned()
        .ok_or(ConfigClientError::EmbeddedAccessRequired)?;
    let signer = agent
        .managed_agent_signer()
        .ok_or(ConfigClientError::ManagedAgentSignerRequired)?;
    let actor = identity::Did::new(signer.did())?;
    Ok((node, actor, signer))
}

async fn load_owned_background_call(
    agent: &ConfigAccess,
    session_id: &str,
    tool_call_id: &str,
) -> Result<Option<(Arc<EmbeddedNode>, ToolCallLifecycle)>> {
    let (node, actor, signer) = managed_agent_parts(agent)?;
    let Some(mut lifecycle) =
        ToolCallLifecycle::load_as(node.clone(), actor.clone(), session_id, tool_call_id)
            .await?
            .filter(|lifecycle| lifecycle.actor() == &actor)
    else {
        return Ok(None);
    };
    lifecycle.bind_agent_signer(Some(signer))?;
    Ok(Some((node, lifecycle)))
}

pub(crate) async fn cancel_loaded_background_tool_call(
    node: Arc<EmbeddedNode>,
    background_executions: &BackgroundExecutionRegistry,
    mut lifecycle: ToolCallLifecycle,
    reason: &str,
) -> Result<CancelBackgroundToolCallOutcome> {
    let session = lifecycle.session_id().to_owned();
    let tool = lifecycle.tool_call_id().to_owned();
    let session_id = session.as_str();
    let tool_call_id = tool.as_str();

    if lifecycle.await_mode() != AwaitMode::Background {
        return Ok(CancelBackgroundToolCallOutcome::NotBackground);
    }
    if lifecycle.is_terminal() {
        return Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
            state: lifecycle.state().as_str().to_string(),
        });
    }

    if lifecycle.kind() == ToolRowKind::PluginJob {
        // The operator holds the node, so the job's own scope authorizes it.
        let scope = ProcessControlScope {
            request_id: String::new(),
            session_id: session_id.into(),
            node_did: lifecycle.node_did().into(),
            requester_did: lifecycle.requester_did().map(str::to_owned),
        };
        let access = lifecycle.access()?;
        let doc_id = lifecycle
            .doc_id()
            .context("job cancellation lacks physical identity")?;
        return Ok(
            match control_by_doc_id(&access, &scope, doc_id, JobControlKind::Stop).await {
                Ok(JobControlOutcome::Queued { .. }) => {
                    anyhow::bail!("local job control returned a queued outcome")
                }
                Ok(JobControlOutcome::Applied { .. }) => {
                    CancelBackgroundToolCallOutcome::Cancelled {
                        live_execution_cancelled: false,
                    }
                }
                Ok(JobControlOutcome::AlreadyTerminal { .. }) => {
                    match ToolCallLifecycle::load_by_doc_id_as(
                        node,
                        lifecycle.actor().clone(),
                        doc_id,
                        &scope.node_did,
                        &scope.session_id,
                        scope.requester_did.as_deref(),
                    )
                    .await?
                    {
                        Some(reloaded) => CancelBackgroundToolCallOutcome::AlreadyTerminal {
                            state: reloaded.state().as_str().to_string(),
                        },
                        None => CancelBackgroundToolCallOutcome::NotFound,
                    }
                }
                Err(
                    JobReadError::NotAJob
                    | JobReadError::NotAuthorized
                    | JobReadError::NotYetObservable,
                ) => CancelBackgroundToolCallOutcome::NotFound,
                Err(error) => return Err(anyhow::Error::new(error)),
            },
        );
    }

    if lifecycle.kind() == crate::tools::call::lifecycle::ToolRowKind::SessionMessage {
        return Ok(
            match crate::session::message::kill(&node, &mut lifecycle).await? {
                crate::session::message::KillOutcome::Settled => {
                    CancelBackgroundToolCallOutcome::AlreadyTerminal {
                        state: lifecycle.state().as_str().to_string(),
                    }
                }
                crate::session::message::KillOutcome::Interrupting { .. }
                | crate::session::message::KillOutcome::Cancelled => {
                    CancelBackgroundToolCallOutcome::Cancelled {
                        live_execution_cancelled: false,
                    }
                }
            },
        );
    }

    // Lean `Background.ProcessControl.stopAction`: an undispatched invocation
    // has no process, so the stop races dispatch on the durable pending row.
    if lifecycle.state() == crate::tools::call::lifecycle::ToolCallState::Pending {
        if lifecycle
            .cancel_before_dispatch_owned(CancelCause::UserCancelled)
            .await?
        {
            return Ok(CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled: false,
            });
        }
        let Some(mut current) = ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            lifecycle.actor().clone(),
            lifecycle
                .doc_id()
                .context("background cancellation lacks physical identity")?,
            lifecycle.node_did(),
            lifecycle.session_id(),
            lifecycle.requester_did(),
        )
        .await?
        else {
            return Ok(CancelBackgroundToolCallOutcome::NotFound);
        };
        current.inherit_signer(&lifecycle)?;
        lifecycle = current;
        if lifecycle.is_terminal() {
            return Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
                state: lifecycle.state().as_str().to_string(),
            });
        }
    }

    if !background_executions
        .contains(
            tool_call_id,
            lifecycle
                .doc_id()
                .context("background cancellation lacks physical identity")?,
        )
        .await
    {
        let (process, _, retained) = ToolCallLifecycle::cancel_unowned_background_tool(
            &node,
            &mut lifecycle,
            background_executions,
            CancelCause::UserCancelled,
            reason,
        )
        .await?;
        return Ok(outcome_for_settled_process(process, &lifecycle, retained));
    }

    let doc_id = lifecycle
        .doc_id()
        .ok_or_else(|| anyhow::anyhow!("background cancellation lacks physical identity"))?
        .to_owned();
    background_executions.set_cancellation_reason(
        tool_call_id,
        &doc_id,
        reason,
        lifecycle.is_deadline_expired(chrono::Utc::now()),
    );
    let process = if crate::tools::native::is_bash_tool_name(lifecycle.tool_name()) {
        background_executions
            .stop_native_process(tool_call_id, &doc_id)
            .await
    } else {
        background_executions
            .stop_execution(tool_call_id, &doc_id)
            .await
    };
    if process == crate::tools::exec::ProcessStopOutcome::StillRunning {
        if let Some(reloaded) = ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            lifecycle.actor().clone(),
            &doc_id,
            lifecycle.node_did(),
            lifecycle.session_id(),
            lifecycle.requester_did(),
        )
        .await?
        {
            if reloaded.is_terminal() {
                return Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
                    state: reloaded.state().as_str().to_owned(),
                });
            }
        }
        return Ok(CancelBackgroundToolCallOutcome::Unverified);
    }
    if background_executions.has_pending_failure(tool_call_id, &doc_id) {
        return Ok(CancelBackgroundToolCallOutcome::Unverified);
    }
    if process == crate::tools::exec::ProcessStopOutcome::AlreadyExited
        && background_executions
            .settle_retained_stop(&mut lifecycle)
            .await?
    {
        return if lifecycle.is_cancelled() {
            Ok(CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled: true,
            })
        } else {
            Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
                state: lifecycle.state().as_str().to_owned(),
            })
        };
    }
    if process != crate::tools::exec::ProcessStopOutcome::Stopped {
        if let Some(reloaded) = ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            lifecycle.actor().clone(),
            &doc_id,
            lifecycle.node_did(),
            lifecycle.session_id(),
            lifecycle.requester_did(),
        )
        .await?
        {
            if reloaded.is_terminal() {
                return Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
                    state: reloaded.state().as_str().to_owned(),
                });
            }
        }
        let (process, _, retained) = ToolCallLifecycle::cancel_unowned_background_tool(
            &node,
            &mut lifecycle,
            background_executions,
            CancelCause::UserCancelled,
            reason,
        )
        .await?;
        return Ok(outcome_for_settled_process(process, &lifecycle, retained));
    }
    if !background_executions
        .settle_retained_stop(&mut lifecycle)
        .await?
    {
        lifecycle
            .cancel_during_run_owned(CancelCause::UserCancelled, reason)
            .await?;
    }
    if lifecycle.is_terminal() {
        background_executions.retire_confirmed_stop(tool_call_id, &doc_id, true);
    }
    if lifecycle.is_cancelled() {
        Ok(CancelBackgroundToolCallOutcome::Cancelled {
            live_execution_cancelled: true,
        })
    } else {
        Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
            state: lifecycle.state().as_str().to_string(),
        })
    }
}

fn outcome_for_settled_process(
    process: crate::tools::exec::ProcessStopOutcome,
    lifecycle: &ToolCallLifecycle,
    retained: bool,
) -> CancelBackgroundToolCallOutcome {
    if retained && lifecycle.is_terminal() && !lifecycle.is_cancelled() {
        CancelBackgroundToolCallOutcome::AlreadyTerminal {
            state: lifecycle.state().as_str().to_owned(),
        }
    } else {
        outcome_for_process(process, false)
    }
}

fn outcome_for_process(
    process: crate::tools::exec::ProcessStopOutcome,
    live_execution_cancelled: bool,
) -> CancelBackgroundToolCallOutcome {
    match process.cancel_reply() {
        crate::tools::exec::CancelProcessReply::Cancelled => {
            CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled,
            }
        }
        crate::tools::exec::CancelProcessReply::Lost => CancelBackgroundToolCallOutcome::Lost,
        crate::tools::exec::CancelProcessReply::Unverified => {
            CancelBackgroundToolCallOutcome::Unverified
        }
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn cancellation_uses_the_selected_agent_and_native_access() {
        use crate::tools::call::lifecycle::admission_fixture::{
            published_admission, PublishedAdmissionOptions,
        };

        let admission = published_admission(PublishedAdmissionOptions {
            name: "control-actor-isolation".into(),
            real_identity: true,
            await_mode: super::AwaitMode::Background,
            ..Default::default()
        })
        .await
        .unwrap();
        let actor = admission.tool.actor().clone();
        // A second managed Agent of the same node: ACP hides the owner's rows.
        let binding =
            crate::store::identity::provision_agent_identity(admission.runtime.as_ref()).unwrap();
        crate::test_support::install_test_agent_binding(
            &admission.node,
            admission.runtime.did(),
            "foreign control Agent",
            &binding,
        )
        .await;
        let foreign =
            crate::store::identity::resolve_agent_identity(admission.runtime.as_ref(), &binding)
                .unwrap();
        let foreign_access = super::ConfigAccess::local_managed_agent(
            admission.node.clone(),
            identity::Did::new(foreign.did()).unwrap(),
            foreign.as_ref(),
        )
        .unwrap();
        let registry = super::BackgroundExecutionRegistry::default();
        let denied = super::cancel_background_tool_call(
            &foreign_access,
            &registry,
            admission.tool.session_id(),
            admission.tool.tool_call_id(),
        )
        .await
        .unwrap();
        assert_eq!(denied, super::CancelBackgroundToolCallOutcome::NotFound);
        // The owning Agent without its signer cannot settle its own rows.
        let unsigned = super::ConfigAccess::local_as(admission.node.clone(), actor.clone());
        let error = super::cancel_background_tool_call(
            &unsigned,
            &registry,
            admission.tool.session_id(),
            admission.tool.tool_call_id(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                gents_protocol::error_chain::find_source::<super::ConfigClientError>(
                    error.as_ref()
                ),
                Some(super::ConfigClientError::ManagedAgentSignerRequired)
            ),
            "{error:#}"
        );
        let (_, owned) = super::load_owned_background_call(
            &admission.tool.access().unwrap(),
            admission.tool.session_id(),
            admission.tool.tool_call_id(),
        )
        .await
        .unwrap()
        .expect("the owning Agent sees its tool call");
        assert_eq!(owned.actor(), &actor);
        assert!(owned.access().unwrap().has_managed_agent_signer());
    }
    use super::*;
    use crate::ensure_runtime_schemas;
    use crate::request::lifecycle::RequestLifecycle;
    use crate::request::streaming::DefraStreamWriter;
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    async fn spawned_child(
        name: &str,
    ) -> (
        crate::tools::call::lifecycle::admission_fixture::PublishedAdmission,
        RequestLifecycle,
        ToolCallLifecycle,
    ) {
        use crate::tools::call::lifecycle::admission_fixture::{
            published_admission_with_owner, PublishedAdmissionOptions,
        };
        let (mut fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
            name: name.into(),
            tool_name: Some("spawn_process".into()),
            real_identity: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let child = fixture
            .tool
            .admit_spawned_background(
                crate::tools::call::lifecycle::SpawnedBackgroundToolAdmission {
                    tool_name: "background_worker".into(),
                    deadline_at: chrono::Utc::now() + chrono::Duration::minutes(10),
                    selected_tool_identity: None,
                },
                "spawned",
            )
            .await
            .unwrap();
        (fixture, owner, child)
    }

    /// Reads the row as its owning Agent: ACP hides agent-owned rows from the node identity.
    async fn reload(child: &ToolCallLifecycle, node: &Arc<EmbeddedNode>) -> ToolCallLifecycle {
        let mut reloaded = ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            child.actor().clone(),
            child.doc_id().unwrap(),
            child.node_did(),
            child.session_id(),
            child.requester_did(),
        )
        .await
        .unwrap()
        .unwrap();
        reloaded.inherit_signer(child).unwrap();
        reloaded
    }

    /// Adapted from Ivan's #3264 stop races onto the owning cancellation path.
    #[tokio::test]
    async fn generated_stop_actions_select_the_durable_transition() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().process_stop_action_cases;
        assert_eq!(cases.len(), 6);
        for case in cases {
            let state = case["state"].as_str().unwrap();
            let (fixture, owner, mut child) = spawned_child(&format!("stop-action-{state}")).await;
            if state != "pending" {
                child.start_running().await.unwrap();
            }
            match state {
                "completed" => child.complete("done").await.unwrap(),
                "failed" => child
                    .fail(
                        "failed",
                        crate::tools::call::lifecycle::FailureClass::External,
                    )
                    .await
                    .unwrap(),
                "timedOut" => assert!(child.timeout().await.unwrap()),
                "cancelled" => assert!(child
                    .cancel_during_run(CancelCause::UserCancelled)
                    .await
                    .unwrap()),
                _ => {}
            }
            let registry = BackgroundExecutionRegistry::default();
            let token = CancellationToken::new();
            let reservation = registry
                .reserve(
                    child.tool_call_id().into(),
                    child.doc_id().unwrap().into(),
                    token.clone(),
                )
                .unwrap();
            let worker = tokio::spawn({
                let token = token.clone();
                async move {
                    token.cancelled().await;
                    drop(reservation);
                }
            });
            let observed = reload(&child, &fixture.node).await;
            let outcome = cancel_loaded_background_tool_call(
                fixture.node.clone(),
                &registry,
                observed,
                "explicit_cancel",
            )
            .await
            .unwrap();
            match case["expected"].as_str().unwrap() {
                "cancel_before_dispatch" => {
                    assert_eq!(
                        outcome,
                        CancelBackgroundToolCallOutcome::Cancelled {
                            live_execution_cancelled: false
                        },
                        "{case}"
                    );
                    assert!(!token.is_cancelled(), "{case}");
                    assert!(child.start_running().await.is_err(), "{case}");
                    assert!(reload(&child, &fixture.node).await.is_cancelled(), "{case}");
                    worker.abort();
                }
                "stop_process" => {
                    assert_eq!(
                        outcome,
                        CancelBackgroundToolCallOutcome::Cancelled {
                            live_execution_cancelled: true
                        },
                        "{case}"
                    );
                    worker.await.unwrap();
                    assert!(reload(&child, &fixture.node).await.is_cancelled(), "{case}");
                }
                "report_terminal" => {
                    assert_eq!(
                        outcome,
                        CancelBackgroundToolCallOutcome::AlreadyTerminal {
                            state: state.into()
                        },
                        "{case}"
                    );
                    assert!(!token.is_cancelled(), "{case}");
                    worker.abort();
                }
                expected => panic!("unexpected stop action {expected}: {case}"),
            }
            drop(owner);
            fixture.node.shutdown().await.expect("shut down test node");
            let _ = std::fs::remove_dir_all(&fixture.path);
        }
    }

    #[tokio::test]
    async fn stop_follows_dispatch_that_wins_the_pending_row() {
        let (fixture, owner, mut child) = spawned_child("stop-after-dispatch-wins").await;
        let stale = reload(&child, &fixture.node).await;
        let registry = BackgroundExecutionRegistry::default();
        let token = CancellationToken::new();
        let reservation = registry
            .reserve(
                child.tool_call_id().into(),
                child.doc_id().unwrap().into(),
                token.clone(),
            )
            .unwrap();
        child.start_running().await.unwrap();
        let worker = tokio::spawn({
            let token = token.clone();
            async move {
                token.cancelled().await;
                drop(reservation);
            }
        });
        let outcome = cancel_loaded_background_tool_call(
            fixture.node.clone(),
            &registry,
            stale,
            "explicit_cancel",
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled: true
            }
        );
        worker.await.unwrap();
        assert!(reload(&child, &fixture.node).await.is_cancelled());
        drop(owner);
        fixture.node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(&fixture.path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_background_tool_call_terminalizes_row_and_token() {
        use crate::store::identity::KeyIdentity;
        use crate::tools::call::lifecycle::admission_fixture::claimed_signed_request;

        let data_path = std::env::temp_dir().join(format!(
            "agent-tool-control-cancel-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&data_path).unwrap();
        let node_identity =
            Arc::new(KeyIdentity::load_or_create(data_path.join("agent.key"), None).unwrap());
        let node = Arc::new(
            defra_node::Node::builder()
                .data_path(&data_path)
                .with_node_identity_did(crate::store::identity::NodeIdentity::did(
                    node_identity.as_ref(),
                ))
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();

        let owner = node.node_identity_did().unwrap().to_owned();
        let node_did = owner.as_str();
        let mut request = claimed_signed_request(
            &node,
            "request-cancel",
            "session-1",
            node_identity.as_ref(),
            None,
        )
        .await;
        let agent = identity::Did::new(request.request().agent_did.as_str()).unwrap();
        let agent_access = request.access().unwrap();
        assert!(agent_access.has_managed_agent_signer());
        let writer =
            DefraStreamWriter::new_for_lifecycle(node.clone(), &request, Duration::from_millis(1))
                .unwrap();
        request.begin_owned_execution(&writer).await.unwrap();
        writer
            .start_provider_attempt(
                &request.request().doc_id,
                0,
                0,
                "inference.1".parse().unwrap(),
            )
            .await;
        let message = Message::Assistant {
            request_id: None,
            id: Some("cancel-provider-message".into()),
            content: ["cancel-native-tool", "sibling-native-tool"]
                .into_iter()
                .map(|id| {
                    AssistantContent::ToolCall(ToolCall {
                        raw_arguments: None,
                        id: id.into(),
                        call_id: Some(format!("{id}-provider-call")),
                        function: ToolFunction::new(
                            "bash_unrestricted".into(),
                            serde_json::json!({}),
                        ),
                        signature: None,
                        additional_params: None,
                    })
                })
                .collect(),
        };
        let published = writer
            .publish_native_turn(&request, 0, 0, &message)
            .await
            .unwrap();
        assert_eq!(published.accepted_tools.len(), 2);
        let deadline = request
            .claimed_deadline_at()
            .expect("claimed request deadline");
        let records = tempfile::tempdir().unwrap();
        let registry =
            BackgroundExecutionRegistry::default().with_process_records(records.path().into());
        let mut tokens = rapidhash::RapidHashMap::default();
        let mut reservations = rapidhash::RapidHashMap::default();
        for accepted in published.accepted_tools {
            let id = accepted.id.clone();
            let mut lifecycle = ToolCallLifecycle::from_accepted(
                node.clone(),
                node_did.to_string(),
                request.request().requester_did.clone(),
                accepted,
                deadline,
                AwaitMode::Background,
            )
            .unwrap();
            lifecycle.start_running().await.unwrap();
            let token = CancellationToken::new();
            reservations.insert(
                id.clone(),
                registry
                    .reserve(
                        id.clone(),
                        lifecycle.doc_id().unwrap().to_owned(),
                        token.clone(),
                    )
                    .unwrap(),
            );
            tokens.insert(id, token);
        }
        let token = tokens["cancel-native-tool"].clone();
        let reservation = reservations.remove("cancel-native-tool").unwrap();
        let physical = ToolCallLifecycle::load_as(
            node.clone(),
            agent.clone(),
            "session-1",
            "cancel-native-tool",
        )
        .await
        .unwrap()
        .unwrap()
        .doc_id()
        .unwrap()
        .to_owned();
        let process = crate::tools::exec::ownership::test_support::OwnedTestProcess::spawn(Some(
            registry.process_recorder("cancel-native-tool", &physical),
        ))
        .await;
        let worker = {
            let token = token.clone();
            tokio::spawn(async move {
                token.cancelled().await;
                process.finish().await;
                drop(reservation);
            })
        };

        let denied = cancel_session_background_process(
            &agent_access,
            &registry,
            node_did,
            Some("foreign"),
            "session-1",
            "cancel-native-tool",
        )
        .await
        .unwrap();
        assert_eq!(denied, CancelBackgroundToolCallOutcome::NotFound);
        assert!(
            !token.is_cancelled(),
            "unauthorized UI cancellation must not signal the worker"
        );

        let requester_did = request.request().requester_did.clone();
        let stop = |session_id: &'static str, tool_call_id: &'static str, doc: String| {
            let agent_access = agent_access.clone();
            let registry = registry.clone();
            let node = node.clone();
            let agent = agent.clone();
            let requester_did = requester_did.clone();
            async move {
                let outcome = client::apply_claimed(
                    &node,
                    &agent_access,
                    &registry,
                    &agent,
                    client::ClaimedTarget {
                        node_did,
                        session_id,
                        requester_did: requester_did.as_deref(),
                        tool_call_id,
                        tool_call_doc_id: &doc,
                        action: "process_stop",
                    },
                )
                .await?;
                match outcome {
                    gents_protocol::process_control::ToolControlReceiptOutcome::Process(
                        outcome,
                    ) => Ok(outcome),
                    other => anyhow::bail!("process stop produced {other:?}"),
                }
            }
        };
        for (session, call, doc) in [
            ("session-1", "sibling-native-tool", physical.clone()),
            ("another-session", "cancel-native-tool", physical.clone()),
            (
                "session-1",
                "cancel-native-tool",
                "missing-physical-row".to_owned(),
            ),
        ] {
            assert!(stop(session, call, doc).await.is_err());
        }
        assert!(!token.is_cancelled());
        let outcome = stop("session-1", "cancel-native-tool", physical.clone())
            .await
            .unwrap();

        assert_eq!(
            outcome,
            CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled: true
            }
        );
        assert!(token.is_cancelled());
        worker.await.unwrap();

        let row = ToolCallLifecycle::load_as(
            node.clone(),
            agent.clone(),
            "session-1",
            "cancel-native-tool",
        )
        .await
        .unwrap()
        .expect("tool row");
        assert!(row.is_cancelled());
        assert!(matches!(
            stop("session-1", "cancel-native-tool", physical.clone())
                .await
                .unwrap(),
            CancelBackgroundToolCallOutcome::AlreadyTerminal { .. }
        ));
        assert!(
            !tokens["sibling-native-tool"].is_cancelled(),
            "killing one background task must not signal its sibling"
        );
        let sibling = ToolCallLifecycle::load_as(
            node.clone(),
            agent.clone(),
            "session-1",
            "sibling-native-tool",
        )
        .await
        .unwrap()
        .expect("sibling row");
        assert!(sibling.is_running());

        let _ = std::fs::remove_dir_all(&data_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_without_live_worker_never_reports_an_unobserved_stop() {
        use crate::tools::call::lifecycle::admission_fixture::{
            published_admission, PublishedAdmissionOptions,
        };
        let modeled = crate::lean_vocab_test::lean_process_stop_cases()
            .iter()
            .find(|case| case.before == "unowned" && case.after == "unowned")
            .expect("modeled unowned process");
        let records = tempfile::tempdir().unwrap();
        let registry =
            BackgroundExecutionRegistry::default().with_process_records(records.path().into());
        for (name, recorded) in [("cancel-unrecorded", false), ("cancel-recorded", true)] {
            let admission = published_admission(PublishedAdmissionOptions {
                name: name.into(),
                real_identity: true,
                await_mode: AwaitMode::Background,
                ..Default::default()
            })
            .await
            .unwrap();
            let tool_doc = admission.tool.doc_id().unwrap().to_owned();
            let process = if recorded {
                crate::tools::exec::ownership::test_support::process_for_generated_outcome(
                    &registry,
                    admission.tool.tool_call_id(),
                    &tool_doc,
                    "stopped",
                )
                .await
            } else {
                None
            };
            let outcome = cancel_background_tool_call(
                &admission.tool.access().unwrap(),
                &registry,
                admission.tool.session_id(),
                admission.tool.tool_call_id(),
            )
            .await
            .unwrap();
            let row = ToolCallLifecycle::load_as(
                admission.node.clone(),
                admission.tool.actor().clone(),
                admission.tool.session_id(),
                admission.tool.tool_call_id(),
            )
            .await
            .unwrap()
            .expect("tool row");
            match process {
                Some(process) => {
                    assert_eq!(
                        outcome,
                        CancelBackgroundToolCallOutcome::Cancelled {
                            live_execution_cancelled: false
                        }
                    );
                    assert!(row.is_cancelled());
                    assert_ne!(
                        process.identity.observe(),
                        crate::tools::exec::ownership::ProcessObservation::Running
                    );
                    process.finish().await;
                }
                None => {
                    assert_eq!(modeled.cancel_reply, "lost");
                    assert_eq!(outcome, CancelBackgroundToolCallOutcome::Lost);
                    assert_eq!(row.state().as_str(), modeled.durable_state);
                }
            }
            assert!(registry.process_record_list().is_empty(), "{name}");
            admission
                .node
                .shutdown()
                .await
                .expect("shut down test node");
            let _ = std::fs::remove_dir_all(admission.path);
        }
    }

    #[tokio::test]
    async fn owned_cancel_persists_custom_completion_reason_for_redrive() {
        use crate::store::identity::NodeIdentity;
        use crate::tools::background::completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
        use gents_protocol::request_admission::SIGNED_REQUEST_FIELDS;
        let data_path = std::env::temp_dir().join(format!(
            "agent-tool-control-custom-cancel-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&data_path).unwrap();
        let identity = Arc::new(
            crate::store::identity::KeyIdentity::load_or_create(data_path.join("agent.key"), None)
                .unwrap(),
        );
        let node = Arc::new(
            defra_node::Node::builder()
                .data_path(&data_path)
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let node_did = identity.did().to_string();
        let node_scope = crate::store::graphql::escape_graphql_string(&node_did);
        crate::ConfigAccess::local(node.clone()).write(
            "test.custom_cancel.node",
            &format!(r#"mutation {{ create_Node(input: {{node_did: "{node_scope}"}}) {{ _docID }} }}"#),
        ).await.unwrap();
        let mut request = crate::tools::call::lifecycle::admission_fixture::claimed_signed_request(
            &node,
            "request-custom",
            "session-custom",
            identity.as_ref(),
            None,
        )
        .await;
        let writer =
            DefraStreamWriter::new_for_lifecycle(node.clone(), &request, Duration::from_millis(1))
                .unwrap();
        request.begin_owned_execution(&writer).await.unwrap();
        writer
            .start_provider_attempt(
                &request.request().doc_id,
                0,
                0,
                "inference.1".parse().unwrap(),
            )
            .await;
        let message = Message::Assistant {
            request_id: None,
            id: Some("custom-provider-message".into()),
            content: vec![AssistantContent::ToolCall(ToolCall {
                raw_arguments: None,
                id: "custom-native-tool".into(),
                call_id: Some("custom-provider-call".into()),
                function: ToolFunction::new("bash_unrestricted".into(), serde_json::json!({})),
                signature: None,
                additional_params: None,
            })],
        };
        let mut published = writer
            .publish_native_turn(&request, 0, 0, &message)
            .await
            .unwrap();
        let accepted = published
            .accepted_tools
            .pop()
            .expect("canonical publication accepted bash_unrestricted");
        let deadline = request
            .claimed_deadline_at()
            .expect("claimed request deadline");
        let mut lifecycle = ToolCallLifecycle::from_accepted(
            node.clone(),
            node_did.clone(),
            request.request().requester_did.clone(),
            accepted,
            deadline,
            AwaitMode::Background,
        )
        .unwrap();
        lifecycle.start_running().await.unwrap();
        assert!(
            lifecycle
                .cancel_during_run_owned(CancelCause::UserCancelled, "operator requested drain")
                .await
                .unwrap(),
            "owned cancel must win the durable running-state compare"
        );
        // Rows are agent-owned: read them through the owning Agent's access, since ACP hides them from the node.
        let agent_access = lifecycle.access().unwrap();
        let row_response = agent_access
            .execute(r#"{ AgentToolCall(filter: { tool_call_id: { _eq: "custom-native-tool" } }, limit: 1) { _docID status lifecycle_state cancel_cause request_id request_doc_id session_id node_did } }"#)
            .await
            .unwrap();
        let row = row_response["data"]["AgentToolCall"]
            .as_array()
            .and_then(|rows| rows.first())
            .cloned()
            .expect("cancelled background tool row");
        assert_eq!(row["lifecycle_state"].as_str(), Some("cancelled"));
        assert_eq!(
            row["cancel_cause"].as_str(),
            Some(CancelCause::UserCancelled.as_str())
        );
        assert_eq!(row["request_id"].as_str(), Some("request-custom"));
        let tool_doc_id = row["_docID"].as_str().unwrap().to_owned();
        assert_eq!(
            row["status"].as_str(),
            Some("completionPending:operator requested drain"),
            "owned cancel must persist the custom completion reason as the redrive cursor"
        );
        let agent_did = request.request().agent_did.clone();
        let signer = lifecycle.access().unwrap().managed_agent_signer().unwrap();
        let identities: Vec<Arc<dyn NodeIdentity>> = vec![Arc::new(signer)];
        let report = ToolCallLifecycle::reconcile_background_completion_side_effects(
            &node,
            &node_did,
            &identities,
        )
        .await
        .unwrap();
        assert_eq!(
            report.side_effects_converged, 1,
            "the cancelled background row must redrive its completion side effects"
        );
        let message_key = format!("background-completion-notification:{tool_doc_id}:tool");
        let message_key = crate::store::graphql::escape_graphql_string(&message_key);
        let notification_query = format!(
            r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{message_key}" }} }}, limit: 2) {{ message_key session_id node_did requester_did role }} }}"#
        );
        let notification_response = agent_access.execute(&notification_query).await.unwrap();
        let notifications = notification_response["data"]["AgentMessage"]
            .as_array()
            .cloned()
            .expect("AgentMessage rows");
        assert_eq!(
            notifications.len(),
            1,
            "completion notification dedupe must keep exactly one canonical row"
        );
        let notification = &notifications[0];
        assert_eq!(notification["session_id"].as_str(), Some("session-custom"));
        assert_eq!(notification["node_did"].as_str(), Some(node_did.as_str()));
        assert_eq!(
            notification["requester_did"].as_str(),
            request.request().requester_did.as_deref()
        );
        assert_eq!(notification["role"].as_str(), Some("user"));
        let escaped_session = crate::store::graphql::escape_graphql_string("session-custom");
        let escaped_agent = crate::store::graphql::escape_graphql_string(&node_did);
        let wake_query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session}" }},
                        node_did: {{ _eq: "{escaped_agent}" }},
                        execution_origin: {{ _eq: "scheduled" }}
                    }},
                    limit: 2
                ) {{ {SIGNED_REQUEST_FIELDS} }}
            }}"#
        );
        let wake_response = agent_access.execute(&wake_query).await.unwrap();
        let wakes = wake_response["data"]["AgentRequest"]
            .as_array()
            .cloned()
            .expect("AgentRequest rows");
        assert_eq!(
            wakes.len(),
            1,
            "redrive must create exactly one scheduled background-completion wake request"
        );
        let wake = &wakes[0];
        assert_eq!(
            wake["admission_signer_did"].as_str(),
            Some(agent_did.as_str())
        );
        assert!(
            !wake["admission_signature"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "wake request must be signed by the selected managed Agent"
        );
        assert_eq!(
            wake["runtime_issuer_did"].as_str(),
            Some(agent_did.as_str())
        );
        assert_eq!(
            wake["runtime_source_request_id"].as_str(),
            Some("request-custom")
        );
        assert_eq!(wake["runtime_source_kind"].as_str(), Some("local-control"));
        assert_eq!(wake["agent_did"].as_str(), Some(agent_did.as_str()));
        assert_eq!(
            wake["content"].as_str(),
            Some(BACKGROUND_COMPLETION_WAKE_PROMPT)
        );
        let _ = std::fs::remove_dir_all(&data_path);
    }
}
