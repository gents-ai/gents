use defra_node::EmbeddedNode;
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::session::canonical_rows::{decode_transcript_message_row, AGENT_MESSAGE_FIELDS};
use crate::streaming::AcceptedToolCall;

use super::{ToolCallLifecycle, ToolCallQueryError};

type Result<T, E = ToolCallQueryError> = std::result::Result<T, E>;

impl ToolCallLifecycle {
    #[cfg(test)]
    pub(crate) async fn load_accepted_for_dispatch(
        node: &std::sync::Arc<EmbeddedNode>,
        tool_doc_id: &str,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
    ) -> Result<AcceptedToolCall> {
        Self::load_direct_binding(node, tool_doc_id, node_did, session_id, requester_did, true)
            .await
    }

    pub(super) async fn load_direct_binding(
        node: &std::sync::Arc<EmbeddedNode>,
        tool_doc_id: &str,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
        require_complete: bool,
    ) -> Result<AcceptedToolCall> {
        ConfigAccess::transact_local_readonly(
            node,
            None,
            "tool_call.load_direct_admission",
            |txn| {
                Box::pin(async move {
                    let scope =
                        crate::session::session_scope_filter(node_did, session_id, requester_did);
                    let tool_id = escape_graphql_string(tool_doc_id);
                    let response = txn
                        .execute(&format!(
                            r#"{{ AgentToolCall(filter: {{
                    {scope}, _docID: {{ _eq: "{tool_id}" }}
                }}, limit: 2) {{ _docID request_doc_id tool_call_id tool_name
                    message_sequence lifecycle_state await_mode spawned_by_tool_call_doc_id plugin_parent_tool_call_doc_id plugin_effect_ordinal }} }}"#
                        ))
                        .await?;
                    let rows = response["data"]["AgentToolCall"]
                        .as_array()
                        .ok_or(ToolCallQueryError::DirectAdmissionOmittedToolRows)?;
                    if rows.len() != 1 {
                        return Err(ToolCallQueryError::DirectAdmissionToolMissingOrAmbiguous);
                    }
                    let tool = &rows[0];
                    if !(tool["spawned_by_tool_call_doc_id"].is_null() && tool["plugin_parent_tool_call_doc_id"].is_null() && tool["plugin_effect_ordinal"].is_null()) {
                        return Err(ToolCallQueryError::SpawnedProcessAdmissionNotDirect);
                    }
                    let request_doc_id = tool["request_doc_id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .ok_or(ToolCallQueryError::AcceptedToolLacksRequestDoc)?;
                    let native_id = tool["tool_call_id"]
                        .as_str()
                        .ok_or(ToolCallQueryError::AcceptedToolLacksNativeId)?;
                    let name = tool["tool_name"]
                        .as_str()
                        .ok_or(ToolCallQueryError::AcceptedToolLacksName)?;
                    let sequence = tool["message_sequence"]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or(ToolCallQueryError::AcceptedToolLacksMessageSequence)?;
                    let await_mode = crate::tool_call_lifecycle::AwaitMode::from_persisted(
                        tool["await_mode"].as_str().unwrap_or("foreground"),
                    )
                    .ok_or(ToolCallQueryError::AcceptedToolInvalidAwaitMode)?;
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
                        .ok_or(ToolCallQueryError::DirectAdmissionOmittedHeaderRows)?;
                    if headers.len() != 1 {
                        return Err(ToolCallQueryError::AcceptedHeaderMissingOrAmbiguous);
                    }
                    let header = decode_transcript_message_row(&headers[0])?;
                    let (message, _) = crate::session::load_canonical_message_in_txn(
                        txn,
                        &header.doc_id,
                        node_did,
                        requester_did,
                    )
                    .await?;
                    if message.role != MessageRole::Assistant {
                        return Err(ToolCallQueryError::DirectBindingNotAssistant);
                    }
                    if !(message.outcome == OutputOutcome::Complete
                        || (!require_complete
                            && matches!(
                                tool["lifecycle_state"].as_str(),
                                Some("completed" | "failed" | "timedOut" | "cancelled")
                            )))
                    {
                        return Err(ToolCallQueryError::PartialDiagnosticNotTerminal);
                    }
                    let MessagePublication::RequestExecution {
                        execution_generation,
                    } = &message.publication
                    else {
                        return Err(ToolCallQueryError::DirectDispatchNotExecution);
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
                    if matches.len() != 1 {
                        return Err(ToolCallQueryError::AcceptedHeaderNotUniqueBinding);
                    }
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
                        await_mode,
                    })
                })
            },
        )
        .await
    }
}
