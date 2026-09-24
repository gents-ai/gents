use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::hook::BackgroundExecutionRegistry;
use crate::tool_call_lifecycle::{AwaitMode, CancelCause, CascadeDispatch, ToolCallLifecycle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelBackgroundToolCallOutcome {
    Cancelled { live_execution_cancelled: bool },
    AlreadyTerminal { state: String },
    NotBackground,
    NotFound,
}

/// Session-principal boundary for client process controls. Keep authorization
/// identical to the model-facing process tools; the operator API below is
/// deliberately broader and must not be exposed directly to client IDs.
pub async fn cancel_session_background_process(
    node: Arc<EmbeddedNode>,
    executions: &BackgroundExecutionRegistry,
    agent_did: &str,
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
        agent_did: agent_did.into(),
        requester_did: requester_did.map(str::to_owned),
    };
    if !scope.authorizes(
        lifecycle.session_id(),
        lifecycle.agent_did(),
        lifecycle.requester_did(),
    ) || lifecycle.is_subagent_bridge()
    {
        return Ok(CancelBackgroundToolCallOutcome::NotFound);
    }
    cancel_background_tool_call(node, executions, agent_did, session_id, tool_call_id).await
}

pub async fn cancel_background_tool_call(
    node: Arc<EmbeddedNode>,
    background_executions: &BackgroundExecutionRegistry,
    agent_did: &str,
    session_id: &str,
    tool_call_id: &str,
) -> Result<CancelBackgroundToolCallOutcome> {
    cancel_background_tool_call_with_cause(
        node,
        background_executions,
        agent_did,
        session_id,
        tool_call_id,
        CancelCause::UserCancelled,
    )
    .await
}

pub(crate) async fn cancel_background_tool_call_with_cause(
    node: Arc<EmbeddedNode>,
    background_executions: &BackgroundExecutionRegistry,
    agent_did: &str,
    session_id: &str,
    tool_call_id: &str,
    cause: CancelCause,
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

    let persisted = lifecycle
        .cancel_during_run_with_cascade_dispatch(cause, agent_did)
        .await;
    // Persist the operator-authored terminal cause before signalling the live
    // worker. Otherwise the worker can observe cancellation first and win the
    // terminal write with the less-specific `interrupted` cause. A persistence
    // failure must still stop the live work: cancellation is best-effort state
    // control, not contingent on observability storage being available.
    let live_execution_cancelled = background_executions.cancel(tool_call_id).await;
    let dispatch = match persisted {
        Ok(dispatch) => dispatch,
        Err(error) => {
            tracing::error!(
                tool_call_id,
                live_execution_cancelled,
                %error,
                "failed to persist background cancellation after stopping live execution",
            );
            return Err(error);
        }
    };

    if let Some(CascadeDispatch::Local { child, .. }) = dispatch {
        crate::interrupt::interrupt_request_by_doc_id(
            node.as_ref(),
            child
                .doc_id
                .as_deref()
                .expect("verified physical cascade child"),
            child
                .agent_did
                .as_deref()
                .expect("verified local child principal"),
            child.requester_did.as_deref(),
        )
        .await?;
    }

    if lifecycle.is_cancelled() {
        Ok(CancelBackgroundToolCallOutcome::Cancelled {
            live_execution_cancelled,
        })
    } else {
        Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal {
            state: lifecycle.state().as_str().to_string(),
        })
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

    /// Canonical claimed-request fixture, mirroring
    /// `tool_call_lifecycle::delivery::claimed_request`.
    async fn claimed_request(
        node: &Arc<EmbeddedNode>,
        request_id: &str,
        session_id: &str,
        agent_did: &str,
    ) -> RequestLifecycle {
        let now = crate::graphql::escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let request_id = crate::graphql::escape_graphql_string(request_id);
        let session_id = crate::graphql::escape_graphql_string(session_id);
        let agent_did = crate::graphql::escape_graphql_string(agent_did);
        let created = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", purpose: "normal", agent_did: "{agent_did}", behavior_id: "general", session_id: "{session_id}", retry_parent_request: "", retry_root_request: "{request_id}", superseded_by_request: "", content: "cancel fixture", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, subagent_depth: 0 }}) {{ _docID }} }}"#)).await;
        assert!(!created.has_errors(), "{:#?}", created.errors);
        let row = node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        let row: gents_protocol::row::AgentRequestRow =
            crate::graphql::first_row(&row, "AgentRequest")
                .unwrap()
                .unwrap();
        let mut lifecycle = RequestLifecycle::new_with_agent_did(
            node.clone(),
            "general",
            &agent_did,
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
            defra_node::EmbeddedNode::builder()
                .data_path(&data_path)
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();

        let agent_did = "did:test:test";
        let mut request = claimed_request(&node, "request-cancel", "session-1", agent_did).await;
        let writer = DefraStreamWriter::new(node.clone(), agent_did, Duration::from_millis(1));
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
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: "cancel-native-tool".into(),
                call_id: Some("cancel-provider-call".into()),
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
            agent_did.to_string(),
            None,
            accepted,
            deadline,
            AwaitMode::Background,
            crate::tool_call_lifecycle::CancelPolicy::Cascade,
        )
        .unwrap();
        lifecycle.start_running().await.unwrap();

        let registry = BackgroundExecutionRegistry::default();
        let token = CancellationToken::new();
        registry
            .reserve("cancel-native-tool".to_string(), token.clone())
            .disarm();

        let denied = cancel_session_background_process(
            node.clone(),
            &registry,
            agent_did,
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
            agent_did,
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

        let row = ToolCallLifecycle::load(node.clone(), "session-1", "cancel-native-tool")
            .await
            .unwrap()
            .expect("tool row");
        assert!(row.is_cancelled());

        let _ = std::fs::remove_dir_all(&data_path);
    }

    /// Owned cancel of a background tool must persist the operator-authored
    /// completion reason, the cancellation cause, and leave the row in the
    /// `completionPending:<reason>` cursor so background-completion recovery
    /// can redrive the signed notification + wake side effects.
    ///
    /// Uses a real `KeyIdentity::load_or_create` registered as the node's
    /// signing identity: the redrive's wake request is authored with
    /// `RequestSigner::RegisteredTarget`, which resolves that registration at
    /// signing time, so the persisted wake row must carry a real admission
    /// signature rather than a fixture DID.
    #[tokio::test]
    async fn owned_cancel_persists_custom_completion_reason_for_redrive() {
        use crate::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
        use crate::identity::AgentIdentity;
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
        crate::test_support::install_test_behavior(&node, identity.did(), "general").await;

        let agent_did = identity.did().to_string();
        let mut request =
            claimed_request(&node, "request-custom", "session-custom", &agent_did).await;
        let writer = DefraStreamWriter::new(node.clone(), &agent_did, Duration::from_millis(1));
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
            agent_did.clone(),
            None,
            accepted,
            deadline,
            AwaitMode::Background,
            crate::tool_call_lifecycle::CancelPolicy::Cascade,
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

        let row_response = node.execute(
            r#"{ AgentToolCall(filter: { tool_call_id: { _eq: "custom-native-tool" } }, limit: 1) { _docID status lifecycle_state cancel_cause request_id request_doc_id session_id agent_did } }"#,
        ).await;
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

        // The operator-authored reason is the redrive cursor: recovery strips
        // `completionPending:` and carries the remainder into the wake. This
        // pins the required production behavior: cancel_during_run_owned must
        // thread its completion reason into terminalize_with_delivery's
        // terminal_persistence_status instead of discarding it.
        assert_eq!(
            row["status"].as_str(),
            Some("completionPending:operator requested drain"),
            "owned cancel must persist the custom completion reason as the redrive cursor"
        );

        // Redrive: recovery must converge the notification + wake side effects
        // exactly once for this row.
        let report =
            ToolCallLifecycle::reconcile_background_completion_side_effects(&node, &agent_did)
                .await
                .unwrap();
        assert_eq!(
            report.side_effects_converged, 1,
            "the cancelled background row must redrive its completion side effects"
        );

        // Exactly one canonical completion notification, keyed by the physical
        // tool-call document (the ExistingNotification dedupe identity).
        let message_key = format!("background-completion-notification:{tool_doc_id}:tool");
        let message_key = crate::graphql::escape_graphql_string(&message_key);
        let notification_response = node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{message_key}" }} }}, limit: 2) {{ message_key session_id agent_did requester_did role }} }}"#
        )).await;
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
        assert_eq!(notification["agent_did"].as_str(), Some(agent_did.as_str()));
        assert!(notification["requester_did"].is_null());
        assert_eq!(notification["role"].as_str(), Some("user"));

        // The wake continuation is authored with RequestSigner::RegisteredTarget,
        // so its persisted row must carry a real admission signature from the
        // registered runtime principal, plus the runtime-source lineage back to
        // the parent request.
        let escaped_session = crate::graphql::escape_graphql_string("session-custom");
        let escaped_agent = crate::graphql::escape_graphql_string(&agent_did);
        let wake_response = node
            .execute(&format!(
                r#"{{
                AgentRequest(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session}" }},
                        agent_did: {{ _eq: "{escaped_agent}" }},
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
            Some(agent_did.as_str())
        );
        assert!(
            !wake["admission_signature"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "wake request must be signed by the registered runtime principal"
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
        assert_eq!(wake["behavior_id"].as_str(), Some("general"));
        assert_eq!(
            wake["content"].as_str(),
            Some(BACKGROUND_COMPLETION_WAKE_PROMPT)
        );

        let _ = std::fs::remove_dir_all(&data_path);
    }
}
