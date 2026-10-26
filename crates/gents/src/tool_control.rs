use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_protocol::job::{JobControlKind, JobControlOutcome, JobReadError};

use crate::background_tools::ProcessControlScope;
use crate::config_client::ConfigAccess;
use crate::hook::BackgroundExecutionRegistry;
use crate::plugin_job::control::control;
use crate::tool_call_lifecycle::{AwaitMode, CancelCause, ToolCallLifecycle, ToolRowKind};

#[derive(Debug, thiserror::Error)]
pub enum ToolControlError {
    #[error(transparent)]
    JobRead(#[from] JobReadError),
    #[error("background cancellation lacks physical identity")]
    LacksPhysicalIdentity,
    #[error(transparent)]
    SessionMessage(Box<crate::session_message::SessionMessageError>),
    #[error(transparent)]
    ToolCallQuery(#[from] crate::tool_call_lifecycle::query::ToolCallQueryError),
    #[error(transparent)]
    ToolCallRecovery(#[from] crate::tool_call_lifecycle::ToolCallRecoveryError),
    #[error(transparent)]
    Transition(#[from] crate::tool_call_lifecycle::TransitionError),
}

impl From<crate::session_message::SessionMessageError> for ToolControlError {
    fn from(error: crate::session_message::SessionMessageError) -> Self {
        Self::SessionMessage(Box::new(error))
    }
}

type Result<T, E = ToolControlError> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelBackgroundToolCallOutcome {
    Cancelled { live_execution_cancelled: bool },
    Lost,
    Unverified,
    AlreadyTerminal { state: String },
    NotBackground,
    NotFound,
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
    let Some(mut lifecycle) =
        ToolCallLifecycle::load(node.clone(), session_id, tool_call_id).await?
    else {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    };

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
        return Ok(
            match control(&access, &scope, tool_call_id, JobControlKind::Stop).await {
                Ok(JobControlOutcome::Applied { .. }) => {
                    CancelBackgroundToolCallOutcome::Cancelled {
                        live_execution_cancelled: false,
                    }
                }
                Ok(JobControlOutcome::AlreadyTerminal { .. }) => {
                    match ToolCallLifecycle::load(node, session_id, tool_call_id).await? {
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
                Err(error) => return Err(ToolControlError::from(error)),
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

    if !background_executions.contains(tool_call_id).await {
        let (process, _) = ToolCallLifecycle::cancel_unowned_background_tool(
            &node,
            &mut lifecycle,
            background_executions,
            CancelCause::UserCancelled,
            "explicit_cancel",
        )
        .await?;
        return Ok(outcome_for_process(process, false));
    }

    let persisted = lifecycle
        .cancel_during_run(CancelCause::UserCancelled)
        .await;
    let doc_id = lifecycle
        .doc_id()
        .ok_or(ToolControlError::LacksPhysicalIdentity)?
        .to_owned();
    let process = background_executions
        .stop_execution(tool_call_id, &doc_id)
        .await;
    let live_execution_cancelled = true;
    let process = Some(process);
    if let Err(error) = persisted {
        tracing::error!(
            tool_call_id,
            live_execution_cancelled,
            %error,
            "failed to persist background cancellation after stopping live execution",
        );
        return Err(error.into());
    }

    if lifecycle.is_cancelled() {
        Ok(match process {
            Some(process) => outcome_for_process(process, live_execution_cancelled),
            None => CancelBackgroundToolCallOutcome::Cancelled {
                live_execution_cancelled,
            },
        })
    } else {
        Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
            state: lifecycle.state().as_str().to_string(),
        })
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
    use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
    use crate::streaming::DefraStreamWriter;
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

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
            crate::watcher::AGENT_REQUEST_FIELDS,
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

        let node_did = "did:test:test";
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
        let registry = BackgroundExecutionRegistry::default();
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
                registry.reserve(
                    id.clone(),
                    lifecycle.doc_id().unwrap().to_owned(),
                    token.clone(),
                ),
            );
            tokens.insert(id, token);
        }
        let token = tokens["cancel-native-tool"].clone();
        let reservation = reservations.remove("cancel-native-tool").unwrap();
        let worker = {
            let token = token.clone();
            tokio::spawn(async move {
                token.cancelled().await;
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

        let outcome = cancel_session_background_process(
            node.clone(),
            &registry,
            node_did,
            None,
            "session-1",
            "cancel-native-tool",
        )
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
                    assert_eq!(outcome, CancelBackgroundToolCallOutcome::Lost);
                    assert_eq!(
                        row.state(),
                        crate::tool_call_lifecycle::ToolCallState::Failed
                    );
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
        use crate::identity::NodeIdentity;
        use crate::SIGNED_REQUEST_FIELDS;
        let data_path = std::env::temp_dir().join(format!(
            "agent-tool-control-custom-cancel-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&data_path).unwrap();
        let identity = Arc::new(
            crate::identity::KeyIdentity::load_or_create(data_path.join("agent.key"), None)
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
