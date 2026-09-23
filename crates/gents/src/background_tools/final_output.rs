//! Exact child terminal selection; neither response rows nor latest-message
//! fallbacks participate in parent result delivery.
use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::{
    MessagePublication, MessageRole, ReconstructionError, TerminalOutput,
};

use super::ChildEdge;
use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use gents_protocol::row::AgentRequestRow;

pub(crate) async fn load_child_final_response(
    node: &EmbeddedNode,
    edge: &ChildEdge,
) -> Result<Option<String>> {
    ConfigAccess::transact_local(node, None, "background.child_terminal_output", |txn| {
        Box::pin(async move {
            // The already-authorized descendant edge pins the physical child.
            // Resolving its logical label again could select a different row.
            let id = escape_graphql_string(&edge.child_request_doc_id);
            let response = txn.execute_local_response(&format!(r#"{{
                AgentRequest(filter: {{ _docID: {{ _eq: "{id}" }} }}) {{
                    _docID request_id agent_did requester_did session_id lifecycle_state terminal_output
                }}
            }}"#)).await?;
            let Some(row) = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")? else {
                return Ok(None);
            };
            anyhow::ensure!(
                row.doc_id.as_deref() == Some(edge.child_request_doc_id.as_str())
                    && row.request_id == edge.child_request_id
                    && row.agent_did.as_deref() == Some(edge.child_agent_did.as_str())
                    && row.requester_did == edge.child_requester_did
                    && row.session_id.as_deref() == Some(edge.child_session_id.as_str()),
                "child terminal output crossed the authorized descendant edge"
            );
            if !row.lifecycle_state.context("child request missing lifecycle_state")?.is_terminal() {
                return Ok(None);
            }
            let Some(selection) = row.terminal_output else {
                // Terminal state may arrive before its selection/dependencies.
                return Ok(None);
            };
            let TerminalOutput::Message { message_doc_id } = selection else {
                // Explicit absence is resolved, unlike a missing selection.
                return Ok(Some(String::new()));
            };
            let header_id = escape_graphql_string(&message_doc_id);
            let available = txn.execute_local_response(&format!(
                r#"{{ AgentMessage(filter: {{ _docID: {{ _eq: "{header_id}" }} }}) {{ _docID }} }}"#
            )).await?;
            let facts = available.data.as_ref().and_then(|v| v.get("AgentMessage"))
                .and_then(serde_json::Value::as_array).context("child header query omitted rows")?;
            if facts.is_empty() {
                return Ok(None);
            }
            anyhow::ensure!(facts.len() == 1, "ambiguous physical child terminal header");
            let (header, message) = match crate::session::load_canonical_message_in_txn(
                txn, &message_doc_id, &edge.child_agent_did, edge.child_requester_did.as_deref()
            ).await {
                Ok(value) => value,
                Err(error) if error.downcast_ref::<ReconstructionError>()
                    .is_some_and(ReconstructionError::is_incomplete) => return Ok(None),
                Err(error) => return Err(error),
            };
            anyhow::ensure!(
                header.request_doc_id.as_deref() == Some(edge.child_request_doc_id.as_str())
                    && header.session_id == edge.child_session_id
                    && header.role == MessageRole::Assistant
                    && matches!(header.publication, MessagePublication::RequestExecution { .. }
                        | MessagePublication::RequestRecovery { .. }),
                "selected child terminal header is not an owned assistant publication"
            );
            Ok(Some(super::render_assistant_message_text(&message)?))
        })
    }).await
}
