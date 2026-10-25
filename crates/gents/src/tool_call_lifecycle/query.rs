mod accepted;
mod result;
pub use result::{
    load_tool_call_arguments, load_tool_call_file_changes, load_tool_call_presentation,
    load_tool_call_result, load_tool_file_change_observation, render_tool_result,
    CanonicalToolCallPresentation, ToolFileChangeRead,
};
pub(crate) use result::{
    load_tool_call_arguments_in_txn, load_tool_call_read, load_tool_call_read_in_txn,
};

use std::sync::Arc;

type Result<T, E = ToolCallQueryError> = std::result::Result<T, E>;
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use super::{
    AwaitMode, CancelCause, FailureClass, SelectedToolIdentity, ToolCallLifecycle, ToolCallState,
};

#[derive(Debug, thiserror::Error)]
pub enum ToolCallQueryError {
    #[error(
        "AgentToolCall selected tool identity must contain both non-empty selected_service_id and selected_tool_name"
    )]
    SelectedToolIdentityIncomplete,
    #[error("load AgentToolCall query failed: {errors}")]
    LoadQueryFailed { errors: String },
    #[error("lifecycle query omitted AgentToolCall rows")]
    LifecycleQueryOmittedRows,
    #[error("ambiguous tool-call lifecycle identity")]
    AmbiguousLifecycleIdentity,
    #[error("AgentToolCall is missing node_did")]
    MissingNodeDid,
    #[error("plugin effect has incoherent accepted-parent provenance")]
    PluginEffectIncoherentParent,
    #[error("spawned lifecycle has incoherent accepted-parent provenance")]
    SpawnedIncoherentParent,
    #[error("tool lifecycle changed while resolving its publication; retry the scoped read")]
    ChangedWhileResolving,
    #[error("AgentToolCall is missing a valid lifecycle_state")]
    MissingLifecycleState,
    #[error("AgentToolCall is missing a valid deadline_at")]
    MissingDeadline,
    #[error("AgentToolCall is missing a valid await_mode")]
    MissingAwaitMode,
    #[error("spawned lifecycle must be background work")]
    SpawnedNotBackground,
    #[error("plugin effect must remain foreground work")]
    PluginEffectNotForeground,
    #[error("tool owner request lookup failed: {errors}")]
    RequestLookupFailed { errors: String },
    #[error("tool owner request lookup omitted rows")]
    RequestLookupOmittedRows,
    #[error("tool owner request is missing or ambiguous")]
    RequestMissingOrAmbiguous,
    #[error("tool owner request is missing logical request_id")]
    RequestMissingLogicalId,
    #[error("tool logical request ID conflicts with physical owner")]
    LogicalRequestIdConflict,
    #[error("direct admission lookup omitted tool rows")]
    DirectAdmissionOmittedToolRows,
    #[error("direct admission tool is missing or ambiguous")]
    DirectAdmissionToolMissingOrAmbiguous,
    #[error("spawned process admission belongs to its accepted meta-call owner")]
    SpawnedProcessAdmissionNotDirect,
    #[error("accepted tool lacks request document identity")]
    AcceptedToolLacksRequestDoc,
    #[error("accepted tool lacks native ID")]
    AcceptedToolLacksNativeId,
    #[error("accepted tool lacks name")]
    AcceptedToolLacksName,
    #[error("accepted tool lacks valid message sequence")]
    AcceptedToolLacksMessageSequence,
    #[error("accepted tool has an invalid await mode")]
    AcceptedToolInvalidAwaitMode,
    #[error("direct admission lookup omitted header rows")]
    DirectAdmissionOmittedHeaderRows,
    #[error("accepted header is missing or ambiguous")]
    AcceptedHeaderMissingOrAmbiguous,
    #[error("direct tool binding requires assistant publication")]
    DirectBindingNotAssistant,
    #[error("partial diagnostic tool bindings must already be terminal and cannot dispatch")]
    PartialDiagnosticNotTerminal,
    #[error("direct dispatch requires provider execution publication")]
    DirectDispatchNotExecution,
    #[error("accepted header does not uniquely bind this physical tool")]
    AcceptedHeaderNotUniqueBinding,
    #[error(transparent)]
    Lifecycle(#[from] super::ToolCallLifecycleError),
    #[error(transparent)]
    Access(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    CanonicalRows(#[from] crate::session::canonical_rows::CanonicalRowsError),
    #[error(transparent)]
    SessionOutput(#[from] crate::session::SessionOutputError),
    #[error(transparent)]
    Graphql(#[from] crate::graphql::GraphqlResponseError),
}

impl crate::config_client::TransactionFailure for ToolCallQueryError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Graphql(error) => error.config_client_error(),
            Self::Lifecycle(error) => error.config_client_error(),
            Self::SessionOutput(error) => error.config_client_error(),
            _ => None,
        }
    }
}

fn decode_selected_tool_identity(
    service_id: Option<String>,
    tool_name: Option<String>,
) -> Result<Option<SelectedToolIdentity>, ToolCallQueryError> {
    match (service_id, tool_name) {
        (None, None) => Ok(None),
        (Some(service_id), Some(tool_name))
            if !service_id.trim().is_empty() && !tool_name.trim().is_empty() =>
        {
            Ok(Some(SelectedToolIdentity {
                service_id,
                tool_name,
            }))
        }
        _ => Err(ToolCallQueryError::SelectedToolIdentityIncomplete),
    }
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    session_id: String,
    tool_call_id: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    request_doc_id: Option<String>,
    #[serde(default)]
    node_did: Option<String>,
    #[serde(default)]
    requester_did: Option<String>,
    message_sequence: u32,
    tool_name: String,
    lifecycle_state: Option<String>,
    started_at: Option<String>,
    #[serde(default)]
    deadline_at: Option<String>,
    tool_failure_class: Option<String>,
    cancel_cause: Option<String>,
    selected_service_id: Option<String>,
    selected_tool_name: Option<String>,
    await_mode: Option<String>,
    #[serde(default)]
    spawned_by_tool_call_doc_id: Option<String>,
    plugin_parent_tool_call_doc_id: Option<String>,
    plugin_effect_ordinal: Option<u32>,
    #[serde(default)]
    job_pin: Option<gents_protocol::job::JobPin>,
}

impl ToolCallLifecycle {
    pub async fn load(
        node: Arc<EmbeddedNode>,
        session_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<Self>> {
        let escaped_session_id = escape_graphql_string(session_id);
        let escaped_tool_call_id = escape_graphql_string(tool_call_id);
        Self::load_filtered(node, format!("session_id:{{_eq:\"{escaped_session_id}\"}},tool_call_id:{{_eq:\"{escaped_tool_call_id}\"}}")).await
    }

    pub async fn load_by_doc_id(
        node: Arc<EmbeddedNode>,
        doc_id: &str,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
    ) -> Result<Option<Self>> {
        let physical = escape_graphql_string(doc_id);
        let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
        Self::load_filtered(node, format!("{scope},_docID:{{_eq:\"{physical}\"}}")).await
    }

    async fn load_filtered(node: Arc<EmbeddedNode>, filter: String) -> Result<Option<Self>> {
        let query = format!(
            r#"{{AgentToolCall(filter:{{{filter}}},limit:2){{
                    _docID
                    session_id
                    tool_call_id
                    request_id
                    request_doc_id
                    node_did
                    requester_did
                    message_sequence
                    tool_name
                    lifecycle_state
                    started_at
                    deadline_at
                    tool_failure_class
                    cancel_cause
                    selected_service_id
                    selected_tool_name
                    await_mode
                    spawned_by_tool_call_doc_id
                    plugin_parent_tool_call_doc_id
                    plugin_effect_ordinal
                    job_pin
        }}}}"#
        );

        let resp =
            crate::graphql::graphql_with_transaction_retry(&node, &query, "tool_call.load").await?;
        if resp.has_errors() {
            return Err(ToolCallQueryError::LoadQueryFailed {
                errors: format!("{:?}", resp.errors),
            });
        }

        if resp
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .is_none()
        {
            return Err(ToolCallQueryError::LifecycleQueryOmittedRows);
        }
        let mut rows: Vec<ToolCallRow> = crate::graphql::rows(&resp, "AgentToolCall")?;
        if rows.len() > 1 {
            return Err(ToolCallQueryError::AmbiguousLifecycleIdentity);
        }
        let Some(row) = rows.pop() else {
            return Ok(None);
        };

        let owner = row
            .node_did
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or(ToolCallQueryError::MissingNodeDid)?;
        let spawned_by_tool_call_doc_id = row
            .spawned_by_tool_call_doc_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned);
        let plugin_effect = super::PluginEffectBinding::decode(
            row.plugin_parent_tool_call_doc_id.as_deref(),
            row.plugin_effect_ordinal,
            spawned_by_tool_call_doc_id.is_some(),
            &row.tool_call_id,
        )?;
        let admission_doc_id = plugin_effect
            .as_ref()
            .map(|effect| effect.parent_tool_call_doc_id.as_str())
            .or(spawned_by_tool_call_doc_id.as_deref())
            .unwrap_or(&row.doc_id);
        let accepted = Self::load_direct_binding(
            &node,
            admission_doc_id,
            owner,
            &row.session_id,
            row.requester_did.as_deref(),
            false,
        )
        .await?;

        if let Some(effect) = plugin_effect.as_ref() {
            if !(effect.parent_tool_call_doc_id != row.doc_id
                && row.request_doc_id.as_deref() == Some(accepted.request_doc_id.as_str())
                && row.message_sequence == accepted.message_sequence)
            {
                return Err(ToolCallQueryError::PluginEffectIncoherentParent);
            }
        } else if let Some(parent_doc_id) = spawned_by_tool_call_doc_id.as_deref() {
            if !(parent_doc_id != row.doc_id
                && row.request_doc_id.as_deref() == Some(accepted.request_doc_id.as_str())
                && row.message_sequence == accepted.message_sequence
                && accepted.tool_name == crate::toolset::SPAWN_PROCESS_TOOL_NAME)
            {
                return Err(ToolCallQueryError::SpawnedIncoherentParent);
            }
        } else if !(row.request_doc_id.as_deref() == Some(accepted.request_doc_id.as_str())
            && row.message_sequence == accepted.message_sequence
            && row.tool_call_id == accepted.id
            && row.tool_name == accepted.tool_name)
        {
            return Err(ToolCallQueryError::ChangedWhileResolving);
        }

        let state = row
            .lifecycle_state
            .as_deref()
            .and_then(ToolCallState::from_persisted)
            .ok_or(ToolCallQueryError::MissingLifecycleState)?;

        let started_at = row
            .started_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let deadline_at = row
            .deadline_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .ok_or(ToolCallQueryError::MissingDeadline)?;

        let failure_class = row
            .tool_failure_class
            .as_deref()
            .and_then(FailureClass::from_persisted);

        let cancel_cause = row
            .cancel_cause
            .as_deref()
            .and_then(CancelCause::from_persisted);

        let await_mode = row
            .await_mode
            .as_deref()
            .and_then(AwaitMode::from_persisted)
            .ok_or(ToolCallQueryError::MissingAwaitMode)?;

        if spawned_by_tool_call_doc_id.is_some() && await_mode != AwaitMode::Background {
            return Err(ToolCallQueryError::SpawnedNotBackground);
        }
        if plugin_effect.is_some() && await_mode != AwaitMode::Foreground {
            return Err(ToolCallQueryError::PluginEffectNotForeground);
        }
        let arguments = if plugin_effect.is_some() {
            let binding = super::delivery::ToolOutputBinding {
                node: node.clone(),
                tool_call_doc_id: row.doc_id.clone(),
                request_doc_id: accepted.request_doc_id.clone(),
                session_id: row.session_id.clone(),
                node_did: owner.to_owned(),
                requester_did: row.requester_did.clone(),
            };
            Some(
                crate::config_client::ConfigAccess::transact_local_readonly(
                    &node,
                    None,
                    "tool_call.load_plugin_arguments",
                    |txn| {
                        Box::pin(async {
                            let mut parent_binding = binding.clone();
                            parent_binding.tool_call_doc_id = plugin_effect
                                .as_ref()
                                .unwrap()
                                .parent_tool_call_doc_id
                                .clone();
                            super::plugin_effect::validate_parent(
                                txn,
                                &parent_binding,
                                &accepted.accepted_header_doc_id,
                                &accepted.execution_generation,
                                row.message_sequence,
                                deadline_at,
                                false,
                            )
                            .await?;
                            super::plugin_effect::arguments_in_txn(
                                txn,
                                &binding,
                                &row.doc_id,
                                &accepted.execution_generation,
                                &row.tool_call_id,
                                &row.tool_name,
                            )
                            .await
                            .map(|(reference, _)| reference)
                        })
                    },
                )
                .await?,
            )
        } else if spawned_by_tool_call_doc_id.is_none() {
            Some(accepted.arguments.clone())
        } else {
            None
        };
        let selected_tool_identity =
            decode_selected_tool_identity(row.selected_service_id, row.selected_tool_name)?;

        let scope = crate::session::session_scope_filter(
            owner,
            &row.session_id,
            row.requester_did.as_deref(),
        );
        let request_doc = escape_graphql_string(&accepted.request_doc_id);
        let request_response = crate::graphql::graphql_with_transaction_retry(&node, &format!(
            r#"{{ AgentRequest(filter: {{ {scope}, _docID: {{ _eq: "{request_doc}" }} }}, limit: 2) {{ request_id }} }}"#
        ), "tool_call.load_request").await?;
        if request_response.has_errors() {
            return Err(ToolCallQueryError::RequestLookupFailed {
                errors: format!("{:?}", request_response.errors),
            });
        }
        let requests = request_response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(serde_json::Value::as_array)
            .ok_or(ToolCallQueryError::RequestLookupOmittedRows)?;
        if requests.len() != 1 {
            return Err(ToolCallQueryError::RequestMissingOrAmbiguous);
        }
        let request_id = requests[0]["request_id"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or(ToolCallQueryError::RequestMissingLogicalId)?
            .to_owned();
        if !row.request_id.as_deref().is_none_or(|id| id == request_id) {
            return Err(ToolCallQueryError::LogicalRequestIdConflict);
        }
        let node_did = row
            .node_did
            .filter(|value| !value.trim().is_empty())
            .ok_or(ToolCallQueryError::MissingNodeDid)?;

        Ok(Some(Self {
            node,
            request_id,
            request_doc_id: row.request_doc_id.filter(|value| !value.trim().is_empty()),
            session_id: row.session_id,
            node_did,
            requester_did: row.requester_did,
            tool_call_id: row.tool_call_id,
            call_id: if spawned_by_tool_call_doc_id.is_none() && plugin_effect.is_none() {
                accepted.call_id.clone()
            } else {
                None
            },
            message_sequence: row.message_sequence,
            kind: super::ToolRowKind::of_row(&row.tool_name, row.job_pin.is_some()),
            tool_name: row.tool_name,
            accepted_header_doc_id: Some(accepted.accepted_header_doc_id),
            arguments,
            execution_generation: Some(accepted.execution_generation),
            plugin_receipt: None,
            plugin_writes: None,
            file_changes: None,
            spawned_by_tool_call_doc_id,
            plugin_effect,
            doc_id: Some(row.doc_id),
            deadline_at,
            state,
            started_at,
            failure_class,
            cancel_cause,
            selected_tool_identity,
            await_mode,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
    use crate::streaming::DefraStreamWriter;
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    use std::time::Duration;

    async fn claimed_request(
        node: &Arc<EmbeddedNode>,
        request_id: &str,
        session_id: &str,
        node_did: &str,
        requester_did: Option<&str>,
    ) -> RequestLifecycle {
        let now = crate::graphql::escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let request_id = crate::graphql::escape_graphql_string(request_id);
        let session_id = crate::graphql::escape_graphql_string(session_id);
        let node_did = crate::graphql::escape_graphql_string(node_did);
        let requester_did_field = crate::session::requester_did_create_field(requester_did);
        let created = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", purpose: "normal", node_did: "{node_did}", agent_id: "general", session_id: "{session_id}", retry_parent_request: "", retry_root_request: "{request_id}", superseded_by_request: "", content: "query fixture", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, request_hop: 0, {requester_did_field} }}) {{ _docID }} }}"#)).await;
        assert!(!created.has_errors(), "{:#?}", created.errors);
        let row = crate::graphql::graphql_with_transaction_retry(node, &format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        ), "test.tool_call.load_request").await.unwrap();
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
    async fn load_preserves_requester_route_and_selected_tool_identity() {
        let data_path = std::env::temp_dir().join(format!(
            "agent-tool-call-query-route-{}",
            uuid::Uuid::new_v4()
        ));
        let node = Arc::new(
            crate::test_support::node_builder()
                .data_path(&data_path)
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("runtime schemas");

        let node_did = "did:test:host";
        let mut request = claimed_request(
            &node,
            "request-routed",
            "session-routed",
            node_did,
            Some("did:test:coordinator"),
        )
        .await;
        let writer = DefraStreamWriter::new(node.clone(), node_did, Duration::from_millis(1));
        request
            .begin_owned_execution(&writer)
            .await
            .expect("owned execution");
        writer
            .start_provider_attempt(
                &request.request().doc_id,
                0,
                0,
                "inference.1".parse().unwrap(),
            )
            .await;
        let arguments = serde_json::json!({
            "service_id": "metrics-prod",
            "tool_name": "query_metrics",
            "arguments": {},
        });
        let message = Message::Assistant {
            id: Some("routed-provider-message".into()),
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: "tool-call-routed".into(),
                call_id: Some("routed-provider-call".into()),
                function: ToolFunction::new("call_tool".into(), arguments.clone()),
                signature: None,
                additional_params: None,
            })],
        };
        let mut published = writer
            .publish_native_turn(&request, 0, 0, &message)
            .await
            .expect("canonical publication accepted call_tool");
        let accepted = published
            .accepted_tools
            .pop()
            .expect("accepted native tool call");
        let deadline = request
            .claimed_deadline_at()
            .expect("claimed request deadline");
        let selected = crate::meta_tools::selected_tool_identity(
            "call_tool",
            &serde_json::to_string(&arguments).expect("arguments text"),
        )
        .expect("call_tool carries a selected tool identity");
        let mut lifecycle = ToolCallLifecycle::from_accepted(
            node.clone(),
            node_did.to_string(),
            Some("did:test:coordinator".to_string()),
            accepted,
            deadline,
            AwaitMode::Foreground,
        )
        .expect("accepted lifecycle")
        .with_selected_tool_identity(Some(selected));
        lifecycle.start_running().await.expect("dispatch tool call");

        let loaded = ToolCallLifecycle::load(node.clone(), "session-routed", "tool-call-routed")
            .await
            .expect("load tool call")
            .expect("persisted tool call");

        assert_eq!(
            loaded.requester_did.as_deref(),
            Some("did:test:coordinator")
        );
        let selected = loaded.selected_tool_identity.expect("selected identity");
        assert_eq!(selected.service_id, "metrics-prod");
        assert_eq!(selected.tool_name, "query_metrics");
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(&data_path);
    }
}
