//! Exact terminal selection of the request a session-message row caused;
//! neither response rows nor latest-message fallbacks participate.
use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::{
    MessagePublication, MessageRole, ReconstructionError, TerminalOutput,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::tool_call_lifecycle::CausedRequestTerminal;

/// The durable terminal of one exact caused request, or `None` while it is
/// live or its terminal output selection has not replicated yet.
pub async fn load_caused_request_terminal(
    node: &EmbeddedNode,
    caused_request_doc_id: &str,
) -> Result<Option<CausedRequestTerminal>> {
    ConfigAccess::transact_local(node, None, "background.caused_request_terminal", |txn| {
        Box::pin(async move {
            let id = escape_graphql_string(caused_request_doc_id);
            let response = txn.execute_local_response(&format!(r#"{{
                AgentRequest(filter: {{ _docID: {{ _eq: "{id}" }} }}) {{
                    _docID request_id node_did requester_did session_id lifecycle_state
                    failure_reason terminal_output
                }}
            }}"#)).await?;
            let Some(row) = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")? else {
                return Ok(None);
            };
            let state = row.lifecycle_state.context("caused request missing lifecycle_state")?;
            let terminal = match state {
                RequestLifecycleState::Failed => CausedRequestTerminal::Failed {
                    reason: row
                        .failure_reason
                        .as_deref()
                        .map(str::trim)
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or("the started request failed")
                        .to_owned(),
                },
                RequestLifecycleState::Dead => CausedRequestTerminal::Dead,
                RequestLifecycleState::Interrupted => CausedRequestTerminal::Interrupted,
                RequestLifecycleState::Superseded => CausedRequestTerminal::Superseded,
                RequestLifecycleState::Completed => {
                    let Some(selection) = row.terminal_output else {
                        // Terminal state may arrive before its selection.
                        return Ok(None);
                    };
                    let TerminalOutput::Message { message_doc_id } = selection else {
                        return Ok(Some(CausedRequestTerminal::Completed { output: String::new() }));
                    };
                    let node_did = row.node_did.as_deref().context("caused request missing node_did")?;
                    let session_id = row.session_id.as_deref().context("caused request missing session_id")?;
                    let header_id = escape_graphql_string(&message_doc_id);
                    let available = txn.execute_local_response(&format!(
                        r#"{{ AgentMessage(filter: {{ _docID: {{ _eq: "{header_id}" }} }}) {{ _docID }} }}"#
                    )).await?;
                    let facts = available.data.as_ref().and_then(|v| v.get("AgentMessage"))
                        .and_then(serde_json::Value::as_array).context("caused terminal header query omitted rows")?;
                    if facts.is_empty() {
                        return Ok(None);
                    }
                    anyhow::ensure!(facts.len() == 1, "ambiguous physical caused terminal header");
                    let (header, message) = match crate::session::load_canonical_message_in_txn(
                        txn, &message_doc_id, node_did, row.requester_did.as_deref()
                    ).await {
                        Ok(value) => value,
                        Err(error) if error.downcast_ref::<ReconstructionError>()
                            .is_some_and(ReconstructionError::is_incomplete) => return Ok(None),
                        Err(error) => return Err(error),
                    };
                    anyhow::ensure!(
                        header.request_doc_id.as_deref() == Some(caused_request_doc_id)
                            && header.session_id == session_id
                            && header.role == MessageRole::Assistant
                            && matches!(header.publication, MessagePublication::RequestExecution { .. }
                                | MessagePublication::RequestRecovery { .. }),
                        "selected caused terminal header is not an owned assistant publication"
                    );
                    CausedRequestTerminal::Completed {
                        output: super::render_assistant_message_text(&message)?,
                    }
                }
                _ => return Ok(None),
            };
            Ok(Some(terminal))
        })
    }).await
}
