//! Direct coordinator admission lookup. Remote hosts must use delegated input;
//! they must never obtain the parent's mixed output through this reader.

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::session::canonical_rows::{decode_transcript_message_row, AGENT_MESSAGE_FIELDS};
use crate::streaming::AcceptedToolCall;

use super::ToolCallLifecycle;

impl ToolCallLifecycle {
    /// Reconstruct the immutable admission binding of a local direct tool.
    /// This is not dispatch authorization: the start transition rechecks the
    /// live request and pending row atomically under the mutation gate.
    pub(crate) async fn load_accepted_for_dispatch(
        node: &EmbeddedNode,
        tool_doc_id: &str,
        agent_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
    ) -> Result<AcceptedToolCall> {
        Self::load_direct_binding(
            node,
            tool_doc_id,
            agent_did,
            session_id,
            requester_did,
            true,
        )
        .await
    }

    pub(super) async fn load_direct_binding(
        node: &EmbeddedNode,
        tool_doc_id: &str,
        agent_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
        require_complete: bool,
    ) -> Result<AcceptedToolCall> {
        ConfigAccess::transact_local(node, None, "tool_call.load_direct_admission", |txn| {
            Box::pin(async move {
                let scope =
                    crate::session::session_scope_filter(agent_did, session_id, requester_did);
                let tool_id = escape_graphql_string(tool_doc_id);
                let response = txn
                    .execute(&format!(
                        r#"{{ AgentToolCall(filter: {{
                    {scope}, _docID: {{ _eq: "{tool_id}" }}
                }}, limit: 2) {{ _docID request_doc_id tool_call_id tool_name
                    message_sequence lifecycle_state delegated_input spawned_by_tool_call_doc_id }} }}"#
                    ))
                    .await?;
                let rows = response["data"]["AgentToolCall"]
                    .as_array()
                    .context("direct admission lookup omitted tool rows")?;
                anyhow::ensure!(
                    rows.len() == 1,
                    "direct admission tool is missing or ambiguous"
                );
                let tool = &rows[0];
                // A remote spawn bridge also carries the host's bounded
                // delegated input on the coordinator's own physical row.
                // Its author may still reconstruct the accepted parent
                // header; a different node must use delegated input instead
                // and must not read the parent's mixed output.
                if !tool["delegated_input"].is_null() {
                    anyhow::ensure!(
                        node.node_identity_did() == Some(agent_did),
                        "foreign node cannot reconstruct delegated parent output"
                    );
                }
                anyhow::ensure!(
                    tool["spawned_by_tool_call_doc_id"].is_null(),
                    "spawned process admission belongs to its accepted meta-call owner"
                );
                let request_doc_id = tool["request_doc_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .context("accepted tool lacks request document identity")?;
                let native_id = tool["tool_call_id"]
                    .as_str()
                    .context("accepted tool lacks native ID")?;
                let name = tool["tool_name"]
                    .as_str()
                    .context("accepted tool lacks name")?;
                let sequence = tool["message_sequence"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .context("accepted tool lacks valid message sequence")?;
                let request = escape_graphql_string(request_doc_id);
                let response = txn
                    .execute(&format!(
                        r#"{{ AgentMessage(filter: {{
                    {scope}, request_doc_id: {{ _eq: "{request}" }}, sequence: {{ _eq: {sequence} }}
                }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
                    ))
                    .await?;
                let headers = response["data"]["AgentMessage"]
                    .as_array()
                    .context("direct admission lookup omitted header rows")?;
                anyhow::ensure!(
                    headers.len() == 1,
                    "accepted header is missing or ambiguous"
                );
                let header = decode_transcript_message_row(&headers[0])?;
                let (message, _) = crate::session::load_canonical_message_in_txn(
                    txn,
                    &header.doc_id,
                    agent_did,
                    requester_did,
                )
                .await?;
                anyhow::ensure!(
                    message.role == MessageRole::Assistant,
                    "direct tool binding requires assistant publication"
                );
                anyhow::ensure!(message.outcome == OutputOutcome::Complete
                    || (!require_complete && matches!(tool["lifecycle_state"].as_str(),
                        Some("completed" | "failed" | "timedOut" | "cancelled"))),
                    "partial diagnostic tool bindings must already be terminal and cannot dispatch"
                );
                let MessagePublication::RequestExecution {
                    execution_generation,
                } = &message.publication
                else {
                    anyhow::bail!("direct dispatch requires provider execution publication");
                };
                let matches = message
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        MessageBlock::ToolCall {
                            tool_call_doc_id,
                            id,
                            call_id,
                            name: block_name,
                            arguments,
                            ..
                        } if tool_call_doc_id == tool_doc_id
                            && id == native_id
                            && block_name == name =>
                        {
                            Some((call_id, arguments))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                anyhow::ensure!(
                    matches.len() == 1,
                    "accepted header does not uniquely bind this physical tool"
                );
                let (call_id, arguments) = matches[0];
                Ok(AcceptedToolCall {
                    tool_call_doc_id: tool_doc_id.to_owned(),
                    request_doc_id: request_doc_id.to_owned(),
                    session_id: session_id.to_owned(),
                    accepted_header_doc_id: header.doc_id,
                    message_sequence: sequence,
                    id: native_id.to_owned(),
                    call_id: call_id.clone(),
                    tool_name: name.to_owned(),
                    execution_generation: execution_generation.clone(),
                    arguments: arguments.clone(),
                    delegated_input: None,
                    spawn_admission: None,
                })
            })
        })
        .await
    }
}
