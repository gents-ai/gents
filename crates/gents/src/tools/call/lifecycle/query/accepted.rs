use anyhow::{Context, Result};
#[cfg(test)]
use defra_node::Node as EmbeddedNode;
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};

use crate::request::streaming::AcceptedToolCall;
#[cfg(test)]
use crate::store::config_client::ConfigAccess;
use crate::store::config_client::ConfigApplyTxn;
use crate::store::graphql::escape_graphql_string;
use gents_protocol::output::rows::{decode_transcript_message_row, AGENT_MESSAGE_FIELDS};

use super::ToolCallLifecycle;

impl ToolCallLifecycle {
    pub(in crate::tools::call::lifecycle) async fn load_accepted_binding_in_txn(
        txn: &ConfigApplyTxn<'_>,
        tool_doc_id: &str,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
        require_complete: bool,
    ) -> Result<AcceptedToolCall> {
        use rapidhash::{HashSetExt, RapidHashSet};
        let scope =
            gents_protocol::session::session_scope_filter(node_did, session_id, requester_did);
        let mut seen = RapidHashSet::new();
        let mut chain = Vec::new();
        let mut cursor = tool_doc_id.to_owned();
        loop {
            anyhow::ensure!(
                seen.insert(cursor.clone()),
                "cyclic tool invocation ancestry"
            );
            let response = txn
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ {scope}, _docID: {{ _eq: "{}" }} }}, limit: 2) {{
                _docID request_doc_id tool_call_id tool_name message_sequence await_mode
                spawned_by_tool_call_doc_id plugin_parent_tool_call_doc_id plugin_effect_ordinal
            }} }}"#,
                    escape_graphql_string(&cursor)
                ))
                .await?;
            let rows = response["data"]["AgentToolCall"]
                .as_array()
                .context("invocation lookup omitted rows")?;
            anyhow::ensure!(rows.len() == 1, "tool invocation is missing or ambiguous");
            let row = &rows[0];
            let spawned = row["spawned_by_tool_call_doc_id"].as_str();
            let effect = super::super::PluginEffectBinding::decode(
                row["plugin_parent_tool_call_doc_id"].as_str(),
                row["plugin_effect_ordinal"]
                    .as_u64()
                    .map(u32::try_from)
                    .transpose()?,
                spawned.is_some(),
                row["tool_call_id"]
                    .as_str()
                    .context("invocation lacks ID")?,
            )?;
            let Some(parent) = effect
                .as_ref()
                .map(|effect| effect.parent_tool_call_doc_id.as_str())
                .or(spawned)
            else {
                break;
            };
            cursor = parent.to_owned();
            chain.push(row.clone());
        }
        let mut accepted = Self::load_direct_binding_in_txn(
            txn,
            &cursor,
            node_did,
            session_id,
            requester_did,
            require_complete || !chain.is_empty(),
        )
        .await?;
        for row in chain.into_iter().rev() {
            anyhow::ensure!(
                row["request_doc_id"].as_str() == Some(&accepted.request_doc_id)
                    && row["message_sequence"].as_u64()
                        == Some(u64::from(accepted.message_sequence)),
                "tool ancestry crosses its accepted request or sequence"
            );
            let mode = gents_protocol::tool_call::AwaitMode::from_persisted(
                row["await_mode"].as_str().unwrap_or(""),
            )
            .context("derived invocation has invalid await mode")?;
            let doc = row["_docID"]
                .as_str()
                .context("derived invocation lacks document")?;
            let id = row["tool_call_id"]
                .as_str()
                .context("derived invocation lacks ID")?;
            let name = row["tool_name"]
                .as_str()
                .context("derived invocation lacks name")?;
            if row["spawned_by_tool_call_doc_id"].is_string() {
                anyhow::ensure!(
                    accepted.tool_name == crate::tools::native::SPAWN_PROCESS_TOOL_NAME
                        && mode == gents_protocol::tool_call::AwaitMode::Background,
                    "spawned invocation lacks its spawn_process owner"
                );
            } else {
                let rows = txn
                    .cursor_rows(
                        "AgentOutputSegment",
                        gents_protocol::output::rows::AGENT_OUTPUT_SEGMENT_FIELDS,
                        &gents_protocol::output::rows::request_output_segments_filter(
                            &accepted.request_doc_id,
                        ),
                    )
                    .await?;
                accepted.arguments = super::super::plugin_effect::arguments_from_rows(
                    &rows,
                    node_did,
                    session_id,
                    requester_did,
                    &accepted.request_doc_id,
                    doc,
                    &accepted.execution_generation,
                    id,
                    name,
                )?
                .0;
            }
            accepted.tool_call_doc_id = doc.to_owned();
            accepted.id = id.to_owned();
            accepted.call_id = None;
            accepted.tool_name = name.to_owned();
            accepted.await_mode = mode;
        }
        Ok(accepted)
    }

    /// The accepted dispatch binding of a direct call, read as the owning
    /// Agent, which alone sees its Agent-owned call rows.
    #[cfg(test)]
    pub(crate) async fn load_accepted_for_dispatch_as(
        node: &std::sync::Arc<EmbeddedNode>,
        actor: identity::Did,
        tool_doc_id: &str,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
    ) -> Result<AcceptedToolCall> {
        Self::load_direct_binding(
            node,
            actor,
            tool_doc_id,
            node_did,
            session_id,
            requester_did,
            true,
        )
        .await
    }

    #[cfg(test)]
    async fn load_direct_binding(
        node: &std::sync::Arc<EmbeddedNode>,
        actor: identity::Did,
        tool_doc_id: &str,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
        require_complete: bool,
    ) -> Result<AcceptedToolCall> {
        ConfigAccess::transact_local_readonly(
            node,
            Some(actor),
            "tool_call.load_direct_admission",
            |txn| {
                Box::pin(Self::load_direct_binding_in_txn(
                    txn,
                    tool_doc_id,
                    node_did,
                    session_id,
                    requester_did,
                    require_complete,
                ))
            },
        )
        .await
    }

    pub(super) async fn load_direct_binding_in_txn(
        txn: &ConfigApplyTxn<'_>,
        tool_doc_id: &str,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
        require_complete: bool,
    ) -> Result<AcceptedToolCall> {
        let scope =
            gents_protocol::session::session_scope_filter(node_did, session_id, requester_did);
        let tool_id = escape_graphql_string(tool_doc_id);
        let response = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ {scope}, _docID: {{ _eq: "{tool_id}" }} }}, limit: 2) {{
                    _docID request_doc_id tool_call_id tool_name message_sequence lifecycle_state
                    await_mode spawned_by_tool_call_doc_id plugin_parent_tool_call_doc_id plugin_effect_ordinal
                }} }}"#
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
        anyhow::ensure!(
            tool["spawned_by_tool_call_doc_id"].is_null()
                && tool["plugin_parent_tool_call_doc_id"].is_null()
                && tool["plugin_effect_ordinal"].is_null(),
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
        let await_mode = gents_protocol::tool_call::AwaitMode::from_persisted(
            tool["await_mode"].as_str().unwrap_or("foreground"),
        )
        .context("accepted tool has an invalid await mode")?;
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
            node_did,
            requester_did,
        )
        .await?;
        anyhow::ensure!(
            message.role == MessageRole::Assistant,
            "direct tool binding requires assistant publication"
        );
        anyhow::ensure!(
            message.outcome == OutputOutcome::Complete
                || (!require_complete
                    && matches!(
                        tool["lifecycle_state"].as_str(),
                        Some("completed" | "failed" | "timedOut" | "cancelled")
                    )),
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
                } if tool_call_doc_id == tool_doc_id && id == native_id && block_name == name => {
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
        let request_doc = escape_graphql_string(request_doc_id);
        let owner = txn
            .execute_local_response(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc}" }} }}, limit: 1) {{ agent_did }} }}"#
            ))
            .await?;
        let agent = owner
            .data
            .as_ref()
            .and_then(|data| data["AgentRequest"].as_array())
            .and_then(|rows| rows.first())
            .context("tool owner request is missing or ambiguous")?["agent_did"]
            .as_str()
            .context("accepted tool request is missing its agent DID")?;
        let actor = identity::Did::new(agent.to_owned())?;
        Ok(AcceptedToolCall {
            actor,
            agent_signer: txn.managed_agent_signer(),
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
    }
}
