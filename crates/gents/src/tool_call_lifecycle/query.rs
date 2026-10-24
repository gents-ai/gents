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

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;

use super::{
    AwaitMode, CancelCause, FailureClass, SelectedToolIdentity, ToolCallLifecycle, ToolCallState,
};

fn decode_selected_tool_identity(
    service_id: Option<String>,
    tool_name: Option<String>,
) -> Result<Option<SelectedToolIdentity>> {
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
        _ => anyhow::bail!(
            "AgentToolCall selected tool identity must contain both non-empty \
             selected_service_id and selected_tool_name"
        ),
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
        ConfigAccess::transact_local_readonly(&node, None, "tool_call.load_by_doc_id", |txn| {
            let node = node.clone();
            Box::pin(async move {
                Self::load_by_doc_id_in_txn(txn, node, doc_id, node_did, session_id, requester_did)
                    .await
            })
        })
        .await
    }

    pub(crate) async fn load_by_doc_id_in_txn(
        txn: &ConfigApplyTxn<'_>,
        node: Arc<EmbeddedNode>,
        doc_id: &str,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
    ) -> Result<Option<Self>> {
        let physical = escape_graphql_string(doc_id);
        let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
        Self::load_filtered_in_txn(txn, node, &format!("{scope},_docID:{{_eq:\"{physical}\"}}"))
            .await
    }

    async fn load_filtered(node: Arc<EmbeddedNode>, filter: String) -> Result<Option<Self>> {
        ConfigAccess::transact_local_readonly(&node, None, "tool_call.load", |txn| {
            let node = node.clone();
            let filter = &filter;
            Box::pin(async move { Self::load_filtered_in_txn(txn, node, filter).await })
        })
        .await
    }

    async fn load_filtered_in_txn(
        txn: &ConfigApplyTxn<'_>,
        node: Arc<EmbeddedNode>,
        filter: &str,
    ) -> Result<Option<Self>> {
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

        let resp = txn.execute_local_response(&query).await?;
        if resp.has_errors() {
            return Err(anyhow!(
                "load AgentToolCall query failed: {:?}",
                resp.errors
            ));
        }

        anyhow::ensure!(
            resp.data
                .as_ref()
                .and_then(|data| data.get("AgentToolCall"))
                .is_some(),
            "lifecycle query omitted AgentToolCall rows"
        );
        let mut rows: Vec<ToolCallRow> = crate::graphql::rows(&resp, "AgentToolCall")?;
        anyhow::ensure!(rows.len() <= 1, "ambiguous tool-call lifecycle identity");
        let Some(row) = rows.pop() else {
            return Ok(None);
        };

        let owner = row
            .node_did
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("AgentToolCall is missing node_did"))?;
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
        let accepted = Self::load_direct_binding_in_txn(
            txn,
            admission_doc_id,
            owner,
            &row.session_id,
            row.requester_did.as_deref(),
            false,
        )
        .await?;

        if let Some(effect) = plugin_effect.as_ref() {
            anyhow::ensure!(
                effect.parent_tool_call_doc_id != row.doc_id
                    && row.request_doc_id.as_deref() == Some(accepted.request_doc_id.as_str())
                    && row.message_sequence == accepted.message_sequence,
                "plugin effect has incoherent accepted-parent provenance"
            );
        } else if let Some(parent_doc_id) = spawned_by_tool_call_doc_id.as_deref() {
            anyhow::ensure!(
                parent_doc_id != row.doc_id
                    && row.request_doc_id.as_deref() == Some(accepted.request_doc_id.as_str())
                    && row.message_sequence == accepted.message_sequence
                    && accepted.tool_name == crate::toolset::SPAWN_PROCESS_TOOL_NAME,
                "spawned lifecycle has incoherent accepted-parent provenance"
            );
        } else {
            anyhow::ensure!(
                row.request_doc_id.as_deref() == Some(accepted.request_doc_id.as_str())
                    && row.message_sequence == accepted.message_sequence
                    && row.tool_call_id == accepted.id
                    && row.tool_name == accepted.tool_name,
                "tool lifecycle changed while resolving its publication; retry the scoped read"
            );
        }

        let state = row
            .lifecycle_state
            .as_deref()
            .and_then(ToolCallState::from_persisted)
            .ok_or_else(|| anyhow!("AgentToolCall is missing a valid lifecycle_state"))?;

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
            .ok_or_else(|| anyhow!("AgentToolCall is missing a valid deadline_at"))?;

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
            .ok_or_else(|| anyhow!("AgentToolCall is missing a valid await_mode"))?;

        if spawned_by_tool_call_doc_id.is_some() {
            anyhow::ensure!(
                await_mode == AwaitMode::Background,
                "spawned lifecycle must be background work"
            );
        }
        if plugin_effect.is_some() {
            anyhow::ensure!(
                await_mode == AwaitMode::Foreground,
                "plugin effect must remain foreground work"
            );
        }
        let arguments = if let Some(effect) = &plugin_effect {
            let binding = super::delivery::ToolOutputBinding {
                node: node.clone(),
                tool_call_doc_id: row.doc_id.clone(),
                request_doc_id: accepted.request_doc_id.clone(),
                session_id: row.session_id.clone(),
                node_did: owner.to_owned(),
                requester_did: row.requester_did.clone(),
            };
            let mut parent_binding = binding.clone();
            parent_binding.tool_call_doc_id = effect.parent_tool_call_doc_id.clone();
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
            Some(
                super::plugin_effect::arguments_in_txn(
                    txn,
                    &binding,
                    &row.doc_id,
                    &accepted.execution_generation,
                    &row.tool_call_id,
                    &row.tool_name,
                )
                .await?
                .0,
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
        let request_response = txn.execute_local_response(&format!(
            r#"{{ AgentRequest(filter: {{ {scope}, _docID: {{ _eq: "{request_doc}" }} }}, limit: 2) {{ request_id }} }}"#
        )).await?;
        anyhow::ensure!(
            !request_response.has_errors(),
            "tool owner request lookup failed: {:?}",
            request_response.errors
        );
        let requests = request_response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow!("tool owner request lookup omitted rows"))?;
        anyhow::ensure!(
            requests.len() == 1,
            "tool owner request is missing or ambiguous"
        );
        let request_id = requests[0]["request_id"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("tool owner request is missing logical request_id"))?
            .to_owned();
        anyhow::ensure!(
            row.request_id.as_deref().is_none_or(|id| id == request_id),
            "tool logical request ID conflicts with physical owner"
        );
        let node_did = row
            .node_did
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("AgentToolCall is missing node_did"))?;

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
            file_capture: None,
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
    async fn accepted_tool_reads_retain_the_callers_snapshot_through_terminal_commit() {
        use crate::tool_call_lifecycle::admission_fixture::{
            published_admission_with_owner, PublishedAdmissionOptions,
        };
        let (fixture, _request) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "inventory-accepted-snapshot".into(),
            real_identity: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = fixture.node;
        let tool = fixture.tool;
        let doc_id = tool.doc_id.clone().unwrap();
        let session_id = tool.session_id.clone();
        let did = fixture.node_did;
        let requester = tool.requester_did.clone();
        let tool = Arc::new(tokio::sync::Mutex::new(tool));
        ConfigAccess::transact_local_readonly(&node, None, "test.tool.accepted_snapshot", |txn| {
            let node = node.clone();
            let tool = tool.clone();
            let (doc_id, session_id, did, requester) = (&doc_id, &session_id, &did, &requester);
            Box::pin(async move {
                let before = ToolCallLifecycle::load_by_doc_id_in_txn(
                    txn,
                    node.clone(),
                    doc_id,
                    did,
                    session_id,
                    requester.as_deref(),
                )
                .await?
                .expect("accepted physical tool");
                assert_eq!(before.state, ToolCallState::Running);
                tokio::spawn(async move { tool.lock().await.complete("finished").await }).await??;
                let after = ToolCallLifecycle::load_by_doc_id_in_txn(
                    txn,
                    node,
                    doc_id,
                    did,
                    session_id,
                    requester.as_deref(),
                )
                .await?
                .expect("same accepted physical tool");
                assert_eq!(after.state, ToolCallState::Running);
                assert_eq!(after.accepted_header_doc_id, before.accepted_header_doc_id);
                assert_eq!(after.execution_generation, before.execution_generation);
                assert_eq!(after.arguments, before.arguments);
                Ok(())
            })
        })
        .await
        .unwrap();
        let fresh = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &doc_id,
            &did,
            &session_id,
            requester.as_deref(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(fresh.state, ToolCallState::Completed);
        node.shutdown().await;
        tokio::fs::remove_dir_all(fixture.path).await.unwrap();
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
