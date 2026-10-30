use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobReadError};

use crate::background_tools::ProcessControlScope;
use crate::config_client::ConfigAccess;
use crate::hook::BackgroundExecutionRegistry;
use crate::plugin_job::control::control_by_doc_id;
use crate::tool_call_lifecycle::{AwaitMode, CancelCause, ToolCallLifecycle, ToolRowKind};

pub use gents_protocol::process_control::ProcessControlOutcome as CancelBackgroundToolCallOutcome;

/// Native adapter for `Background.ProcessControl.operatorProcessAuthorized`.
pub fn operator_process_authorized(
    executing: &str,
    requested: &str,
    owner: &str,
    actor: Option<&str>,
    requested_invocation: (&str, &str),
    owned_invocation: (&str, &str),
    eligibility: (bool, bool),
) -> bool {
    crate::plugin_job::control::operator_control_authorized(executing, requested, owner, actor)
        && requested_invocation == owned_invocation
        && eligibility.0
        && eligibility.1
}

/// The transport authenticates `actor` through DefraDB. The process owner then
/// revalidates the displayed invocation before signalling its existing guard.
pub async fn operator_process_control(
    node: Arc<EmbeddedNode>,
    executions: &BackgroundExecutionRegistry,
    actor: Option<&str>,
    request: &gents_protocol::process_control::OperatorProcessControlRequest,
) -> Result<CancelBackgroundToolCallOutcome> {
    let executing = node
        .node_identity_did()
        .context("runtime has no node identity")?;
    anyhow::ensure!(
        crate::plugin_job::control::operator_control_authorized(
            executing,
            &request.node_did,
            executing,
            actor,
        ),
        "runtime operator authorization is required"
    );
    let response = ConfigAccess::local(node.clone()).execute(&format!(
        "{{AgentToolCall(filter: {{_docID: {{_eq: \"{}\"}}, node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}, tool_call_id: {{_eq: \"{}\"}}}}, limit: 2) {{requester_did}}}}",
        crate::graphql::escape_graphql_string(&request.tool_call_doc_id),
        crate::graphql::escape_graphql_string(&request.node_did),
        crate::graphql::escape_graphql_string(&request.session_id),
        crate::graphql::escape_graphql_string(&request.tool_call_id),
    )).await?;
    #[derive(serde::Deserialize)]
    struct Scope {
        requester_did: Option<String>,
    }
    let scopes: Vec<Scope> = serde_json::from_value(response["data"]["AgentToolCall"].clone())
        .context("reading the native invocation scope")?;
    let [scope] = scopes.as_slice() else {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    };
    let Some(lifecycle) = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &request.tool_call_doc_id,
        &request.node_did,
        &request.session_id,
        scope.requester_did.as_deref(),
    )
    .await?
    else {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    };
    let native = matches!(lifecycle.kind(), ToolRowKind::Native);
    let physical = lifecycle
        .doc_id()
        .context("native invocation lacks physical identity")?;
    if !operator_process_authorized(
        executing,
        &request.node_did,
        lifecycle.node_did(),
        actor,
        (&request.tool_call_id, &request.tool_call_doc_id),
        (lifecycle.tool_call_id(), physical),
        (lifecycle.await_mode() == AwaitMode::Background, native),
    ) {
        return Ok(CancelBackgroundToolCallOutcome::NotBackground);
    }
    cancel_loaded_background_tool_call(node, executions, lifecycle, "explicit_cancel").await
}

pub async fn cancel_session_background_process(
    node: Arc<EmbeddedNode>,
    executions: &BackgroundExecutionRegistry,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    tool_call_id: &str,
) -> Result<CancelBackgroundToolCallOutcome> {
    let Some(lifecycle) = ToolCallLifecycle::load(node.clone(), session_id, tool_call_id).await?
    else {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    };
    let scope = crate::background_tools::ProcessControlScope {
        request_id: String::new(),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: requester_did.map(str::to_owned),
    };
    if !scope.authorizes(
        lifecycle.session_id(),
        lifecycle.node_did(),
        lifecycle.requester_did(),
    ) || lifecycle.kind() == crate::tool_call_lifecycle::ToolRowKind::SessionMessage
    {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    }
    cancel_background_tool_call(node, executions, session_id, tool_call_id).await
}

pub async fn cancel_background_tool_call(
    node: Arc<EmbeddedNode>,
    background_executions: &BackgroundExecutionRegistry,
    session_id: &str,
    tool_call_id: &str,
) -> Result<CancelBackgroundToolCallOutcome> {
    let Some(lifecycle) = ToolCallLifecycle::load(node.clone(), session_id, tool_call_id).await?
    else {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    };

    cancel_loaded_background_tool_call(node, background_executions, lifecycle, "explicit_cancel")
        .await
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
        let access = ConfigAccess::local(node.clone());
        let doc_id = lifecycle
            .doc_id()
            .context("job cancellation lacks physical identity")?;
        return Ok(
            match control_by_doc_id(&access, &scope, doc_id, JobControlKind::Stop).await {
                Ok(JobControlOutcome::Applied { .. }) => {
                    CancelBackgroundToolCallOutcome::Cancelled {
                        live_execution_cancelled: false,
                    }
                }
                Ok(JobControlOutcome::AlreadyTerminal { .. }) => {
                    match ToolCallLifecycle::load_by_doc_id(
                        node,
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

    if lifecycle.kind() == crate::tool_call_lifecycle::ToolRowKind::SessionMessage {
        return Ok(
            match crate::session_message::kill(&node, &mut lifecycle).await? {
                crate::session_message::KillOutcome::Settled => {
                    CancelBackgroundToolCallOutcome::AlreadyTerminal {
                        state: lifecycle.state().as_str().to_string(),
                    }
                }
                crate::session_message::KillOutcome::Interrupting { .. }
                | crate::session_message::KillOutcome::Cancelled => {
                    CancelBackgroundToolCallOutcome::Cancelled {
                        live_execution_cancelled: false,
                    }
                }
            },
        );
    }

    // Lean `Background.ProcessControl.stopAction`: an undispatched invocation
    // has no process, so the stop races dispatch on the durable pending row.
    if lifecycle.state() == crate::tool_call_lifecycle::ToolCallState::Pending {
        if lifecycle
            .cancel_before_dispatch_owned(CancelCause::UserCancelled)
            .await?
        {
            return Ok(CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled: false,
            });
        }
        let Some(current) = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
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
    let process = if crate::toolset::is_bash_tool_name(lifecycle.tool_name()) {
        background_executions
            .stop_native_process(tool_call_id, &doc_id)
            .await
    } else {
        background_executions
            .stop_execution(tool_call_id, &doc_id)
            .await
    };
    if process == crate::managed_exec::ProcessStopOutcome::StillRunning {
        if let Some(reloaded) = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
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
    if process == crate::managed_exec::ProcessStopOutcome::AlreadyExited
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
    if process != crate::managed_exec::ProcessStopOutcome::Stopped {
        if let Some(reloaded) = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
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
    process: crate::managed_exec::ProcessStopOutcome,
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
    process: crate::managed_exec::ProcessStopOutcome,
    live_execution_cancelled: bool,
) -> CancelBackgroundToolCallOutcome {
    match process.cancel_reply() {
        crate::managed_exec::CancelProcessReply::Cancelled => {
            CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled,
            }
        }
        crate::managed_exec::CancelProcessReply::Lost => CancelBackgroundToolCallOutcome::Lost,
        crate::managed_exec::CancelProcessReply::Unverified => {
            CancelBackgroundToolCallOutcome::Unverified
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensure_runtime_schemas;
    use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
    use crate::request::streaming::DefraStreamWriter;
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    async fn spawned_child(
        name: &str,
    ) -> (
        crate::tool_call_lifecycle::admission_fixture::PublishedAdmission,
        RequestLifecycle,
        ToolCallLifecycle,
    ) {
        use crate::tool_call_lifecycle::admission_fixture::{
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
                crate::tool_call_lifecycle::SpawnedBackgroundToolAdmission {
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

    async fn reload(child: &ToolCallLifecycle, node: &Arc<EmbeddedNode>) -> ToolCallLifecycle {
        ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            child.doc_id().unwrap(),
            child.node_did(),
            child.session_id(),
            child.requester_did(),
        )
        .await
        .unwrap()
        .unwrap()
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
                    .fail("failed", crate::tool_call_lifecycle::FailureClass::External)
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
            fixture.node.shutdown().await;
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
        fixture.node.shutdown().await;
        let _ = std::fs::remove_dir_all(&fixture.path);
    }

    #[test]
    fn operator_process_control_matches_executable_model() {
        let cases =
            &crate::lean_vocab_test::lean_contract_snapshot().operator_process_control_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let string = |key: &str| case[key].as_str().unwrap();
            assert_eq!(
                operator_process_authorized(
                    string("executing"),
                    string("requested"),
                    string("owner"),
                    case["actor"].as_str(),
                    (string("requested_id"), string("requested_doc")),
                    (string("owned_id"), string("owned_doc")),
                    (
                        case["background"].as_bool().unwrap(),
                        case["native"].as_bool().unwrap()
                    ),
                ),
                case["expected"].as_bool().unwrap(),
                "{case}",
            );
        }
    }

    async fn claimed_request(
        node: &Arc<EmbeddedNode>,
        request_id: &str,
        session_id: &str,
        node_did: &str,
    ) -> RequestLifecycle {
        let now = crate::graphql::escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let request_id = crate::graphql::escape_graphql_string(request_id);
        let session_id = crate::graphql::escape_graphql_string(session_id);
        let node_did = crate::graphql::escape_graphql_string(node_did);
        let created = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", purpose: "normal", node_did: "{node_did}", agent_id: "general", session_id: "{session_id}", retry_parent_request: "", retry_root_request: "{request_id}", superseded_by_request: "", content: "cancel fixture", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, request_hop: 0 }}) {{ _docID }} }}"#)).await;
        assert!(!created.has_errors(), "{:#?}", created.errors);
        let row = node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::request::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        let row: gents_protocol::row::AgentRequestRow =
            crate::graphql::first_row(&row, "AgentRequest")
                .unwrap()
                .unwrap();
        let mut lifecycle = RequestLifecycle::new_with_node_did(
            node.clone(),
            "general",
            &node_did,
            row.try_into().unwrap(),
            60,
        );
        assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
        lifecycle
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_background_tool_call_terminalizes_row_and_token() {
        let data_path = std::env::temp_dir().join(format!(
            "agent-tool-control-cancel-{}",
            uuid::Uuid::new_v4()
        ));
        let node = Arc::new(
            crate::test_support::node_builder()
                .data_path(&data_path)
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();

        let owner = node.node_identity_did().unwrap().to_owned();
        let node_did = owner.as_str();
        let mut request = claimed_request(&node, "request-cancel", "session-1", node_did).await;
        let writer = DefraStreamWriter::new(node.clone(), node_did, Duration::from_millis(1));
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
            id: Some("cancel-provider-message".into()),
            content: ["cancel-native-tool", "sibling-native-tool"]
                .into_iter()
                .map(|id| {
                    AssistantContent::ToolCall(ToolCall {
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
                None,
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
        let physical = ToolCallLifecycle::load(node.clone(), "session-1", "cancel-native-tool")
            .await
            .unwrap()
            .unwrap()
            .doc_id()
            .unwrap()
            .to_owned();
        let process = crate::managed_exec::ownership::test_support::OwnedTestProcess::spawn(Some(
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
            node.clone(),
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

        let command = gents_protocol::process_control::OperatorProcessControlRequest {
            node_did: owner.clone(),
            session_id: "session-1".into(),
            tool_call_id: "cancel-native-tool".into(),
            tool_call_doc_id: physical,
        };
        assert!(
            operator_process_control(node.clone(), &registry, Some("foreign"), &command)
                .await
                .is_err()
        );
        let mut mismatches = [command.clone(), command.clone(), command.clone()];
        mismatches[0].tool_call_id = "sibling-native-tool".into();
        mismatches[1].session_id = "another-session".into();
        mismatches[2].tool_call_doc_id = "missing-physical-row".into();
        for wrong in mismatches {
            assert_eq!(
                operator_process_control(node.clone(), &registry, Some(node_did), &wrong)
                    .await
                    .unwrap(),
                CancelBackgroundToolCallOutcome::NotFound
            );
        }
        assert!(!token.is_cancelled());
        let outcome = operator_process_control(node.clone(), &registry, Some(node_did), &command)
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

        let row = ToolCallLifecycle::load(node.clone(), "session-1", "cancel-native-tool")
            .await
            .unwrap()
            .expect("tool row");
        assert!(row.is_cancelled());
        assert!(matches!(
            operator_process_control(node.clone(), &registry, Some(node_did), &command)
                .await
                .unwrap(),
            CancelBackgroundToolCallOutcome::AlreadyTerminal { .. }
        ));
        assert!(
            !tokens["sibling-native-tool"].is_cancelled(),
            "killing one background task must not signal its sibling"
        );
        let sibling = ToolCallLifecycle::load(node.clone(), "session-1", "sibling-native-tool")
            .await
            .unwrap()
            .expect("sibling row");
        assert!(sibling.is_running());

        let _ = std::fs::remove_dir_all(&data_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_without_live_worker_never_reports_an_unobserved_stop() {
        use crate::tool_call_lifecycle::admission_fixture::{
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
                crate::managed_exec::ownership::test_support::process_for_generated_outcome(
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
                admission.node.clone(),
                &registry,
                admission.tool.session_id(),
                admission.tool.tool_call_id(),
            )
            .await
            .unwrap();
            let row = ToolCallLifecycle::load(
                admission.node.clone(),
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
                        crate::managed_exec::ownership::ProcessObservation::Running
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
            admission.node.shutdown().await;
            let _ = std::fs::remove_dir_all(admission.path);
        }
    }

    #[tokio::test]
    async fn owned_cancel_persists_custom_completion_reason_for_redrive() {
        use crate::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
        use crate::store::identity::NodeIdentity;
        use crate::SIGNED_REQUEST_FIELDS;
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
            defra_node::EmbeddedNode::builder()
                .data_path(&data_path)
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        crate::test_support::install_test_agent(&node, identity.did(), "general").await;
        let node_did = identity.did().to_string();
        let mut request = crate::tool_call_lifecycle::admission_fixture::claimed_signed_request(
            &node,
            "request-custom",
            "session-custom",
            identity.as_ref(),
            None,
        )
        .await;
        let writer = DefraStreamWriter::new(node.clone(), &node_did, Duration::from_millis(1));
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
            id: Some("custom-provider-message".into()),
            content: vec![AssistantContent::ToolCall(ToolCall {
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
        let row_response = node . execute (r#"{ AgentToolCall(filter: { tool_call_id: { _eq: "custom-native-tool" } }, limit: 1) { _docID status lifecycle_state cancel_cause request_id request_doc_id session_id node_did } }"# ,) . await ;
        assert!(!row_response.has_errors(), "{:#?}", row_response.errors);
        let row = crate::graphql::first_row::<serde_json::Value>(&row_response, "AgentToolCall")
            .unwrap()
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
        let report =
            ToolCallLifecycle::reconcile_background_completion_side_effects(&node, &node_did)
                .await
                .unwrap();
        assert_eq!(
            report.side_effects_converged, 1,
            "the cancelled background row must redrive its completion side effects"
        );
        let message_key = format!("background-completion-notification:{tool_doc_id}:tool");
        let message_key = crate::graphql::escape_graphql_string(&message_key);
        let notification_response = node . execute (& format ! (r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{message_key}" }} }}, limit: 2) {{ message_key session_id node_did requester_did role }} }}"#)) . await ;
        assert!(
            !notification_response.has_errors(),
            "{:#?}",
            notification_response.errors
        );
        let notifications =
            crate::graphql::rows::<serde_json::Value>(&notification_response, "AgentMessage")
                .unwrap();
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
            Some(node_did.as_str())
        );
        assert_eq!(notification["role"].as_str(), Some("user"));
        let escaped_session = crate::graphql::escape_graphql_string("session-custom");
        let escaped_agent = crate::graphql::escape_graphql_string(&node_did);
        let wake_response = node
            .execute(&format!(
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
            ))
            .await;
        assert!(!wake_response.has_errors(), "{:#?}", wake_response.errors);
        let wakes =
            crate::graphql::rows::<serde_json::Value>(&wake_response, "AgentRequest").unwrap();
        assert_eq!(
            wakes.len(),
            1,
            "redrive must create exactly one scheduled background-completion wake request"
        );
        let wake = &wakes[0];
        assert_eq!(
            wake["admission_signer_did"].as_str(),
            Some(node_did.as_str())
        );
        assert!(
            !wake["admission_signature"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "wake request must be signed by the registered runtime node"
        );
        assert_eq!(wake["runtime_issuer_did"].as_str(), Some(node_did.as_str()));
        assert_eq!(
            wake["runtime_source_request_id"].as_str(),
            Some("request-custom")
        );
        assert_eq!(wake["runtime_source_kind"].as_str(), Some("local-control"));
        assert_eq!(wake["agent_id"].as_str(), Some("general"));
        assert_eq!(
            wake["content"].as_str(),
            Some(BACKGROUND_COMPLETION_WAKE_PROMPT)
        );
        let _ = std::fs::remove_dir_all(&data_path);
    }
}
