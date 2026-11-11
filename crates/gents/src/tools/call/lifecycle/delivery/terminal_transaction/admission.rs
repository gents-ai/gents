use super::super::background_receipt::{
    ensure_background_receipt_before_bridge_close, BackgroundReceiptKey,
};
use super::Terminalization;
use crate::store::graphql::escape_graphql_string;
use crate::tools::call::lifecycle::ToolCallState;
use anyhow::{Context, Result};
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};

pub(super) async fn validate_accepted_invocation(
    terminalization: Terminalization<'_, '_>,
) -> Result<()> {
    let Terminalization {
        txn,
        tool_doc_id,
        request_doc_id,
        accepted_header_doc_id,
        node_did,
        requester_did,
        session_id,
        tool_call_id,
        call_id,
        message_sequence,
        arguments,
        spawned_by_tool_call_doc_id,
        plugin_effect,
        tool_name,
        deadline_at,
        request,
        agent,
        session,
        requester_filter,
        ..
    } = terminalization;
    let (accepted, _) = crate::session::load_canonical_message_in_txn(
        txn,
        accepted_header_doc_id,
        node_did,
        requester_did,
    )
    .await?;
    if let Some(effect) = plugin_effect {
        anyhow::ensure!(call_id.is_none(), "invalid plugin effect terminal identity");
        crate::tools::call::lifecycle::plugin_effect::validate_parent(
            txn,
            &effect.parent,
            accepted_header_doc_id,
            &effect.generation,
            message_sequence,
            deadline_at,
            false,
        )
        .await?;
        let (reference, _) = crate::tools::call::lifecycle::plugin_effect::arguments_in_txn(
            txn,
            &effect.parent,
            tool_doc_id,
            &effect.generation,
            tool_call_id,
            tool_name,
        )
        .await?;
        anyhow::ensure!(
            arguments == Some(&reference),
            "plugin effect terminal arguments changed"
        );
    } else if let Some(parent_doc_id) = spawned_by_tool_call_doc_id {
        let parent = escape_graphql_string(parent_doc_id);
        let parent_rows = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{parent}" }}, request_doc_id: {{ _eq: "{request}" }},
            session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }}{requester_filter}
        }}, limit: 2) {{ _docID tool_call_id tool_name message_sequence lifecycle_state
            spawned_by_tool_call_doc_id }} }}"#
            ))
            .await?;
        let parents = parent_rows["data"]["AgentToolCall"]
            .as_array()
            .context("spawned terminalization parent lookup omitted rows")?;
        anyhow::ensure!(
            parents.len() == 1,
            "spawned terminalization parent is missing or ambiguous"
        );
        let parent = &parents[0];
        let parent_id = parent["tool_call_id"]
            .as_str()
            .context("spawned parent lacks native ID")?;
        let parent_name = parent["tool_name"]
            .as_str()
            .context("spawned parent lacks tool name")?;
        anyhow::ensure!(
            parent["spawned_by_tool_call_doc_id"].is_null()
                && parent_name == crate::tools::native::SPAWN_PROCESS_TOOL_NAME
                && parent["message_sequence"].as_u64() == Some(u64::from(message_sequence))
                && accepted.session_id == session_id
                && accepted.request_doc_id.as_deref() == Some(request_doc_id)
                && accepted.role == MessageRole::Assistant
                && accepted.outcome == OutputOutcome::Complete
                && accepted.sequence == message_sequence
                && matches!(accepted.publication, MessagePublication::RequestExecution { .. })
                && accepted.blocks.iter().any(|block| matches!(block,
                    MessageBlock::ToolCall { tool_call_doc_id, id, name, .. }
                    if tool_call_doc_id == parent_doc_id && id == parent_id && name == parent_name)),
            "spawned terminalization has no exact accepted spawn_process parent"
        );
    } else {
        let arguments =
            arguments.context("canonical direct terminalization requires accepted arguments")?;
        anyhow::ensure!(
            accepted.session_id == session_id
                && accepted.request_doc_id.as_deref() == Some(request_doc_id)
                && accepted.role == MessageRole::Assistant
                && accepted.outcome == OutputOutcome::Complete
                && accepted.sequence == message_sequence
                && matches!(
                    accepted.publication,
                    MessagePublication::RequestExecution { .. }
                )
                && accepted.blocks.iter().any(|block| matches!(block,
                    MessageBlock::ToolCall { tool_call_doc_id, id, call_id: accepted_call_id,
                        name, arguments: accepted_arguments, .. }
                    if tool_call_doc_id == tool_doc_id && id == tool_call_id
                        && accepted_call_id.as_deref() == call_id && name == tool_name
                        && accepted_arguments == arguments)),
            "accepted tool header does not bind this exact physical invocation"
        );
    }
    Ok(())
}

pub(super) async fn plugin_receipt_field(
    terminalization: Terminalization<'_, '_>,
) -> Result<Option<String>> {
    let Terminalization {
        txn,
        message_sequence,
        plugin_receipt,
        tool,
        request,
        agent,
        session,
        tool_id,
        name,
        requester_filter,
        ..
    } = terminalization;
    let plugin_receipt_field = if let Some(receipt) = plugin_receipt {
        let rows = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{tool}" }},
                request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }},
                node_did: {{ _eq: "{agent}" }}, tool_call_id: {{ _eq: "{tool_id}" }},
                tool_name: {{ _eq: "{name}" }}, message_sequence: {{ _eq: {message_sequence} }}
                {requester_filter} }}, limit: 2) {{ lifecycle_state plugin_execution_receipt }} }}"#
            ))
            .await?;
        let rows = rows["data"]["AgentToolCall"]
            .as_array()
            .context("plugin receipt tool query omitted rows")?;
        anyhow::ensure!(
            rows.len() == 1,
            "plugin receipt lacks exact physical tool invocation"
        );
        let row = &rows[0];
        let state = row["lifecycle_state"]
            .as_str()
            .and_then(ToolCallState::from_persisted)
            .context("plugin receipt tool has invalid lifecycle state")?;
        let existing = if row["plugin_execution_receipt"].is_null() {
            None
        } else {
            Some(serde_json::from_value::<
                gents_protocol::plugin::PluginExecutionReceipt,
            >(row["plugin_execution_receipt"].clone())?)
        };
        match crate::tools::call::lifecycle::plugin_receipt::decide(
            state.is_terminal(),
            existing.as_ref(),
            receipt,
        ) {
            crate::tools::call::lifecycle::plugin_receipt::ReceiptDecision::Commit
            | crate::tools::call::lifecycle::plugin_receipt::ReceiptDecision::Replay => {}
            crate::tools::call::lifecycle::plugin_receipt::ReceiptDecision::Adopt => {
                return Ok(None)
            }
            crate::tools::call::lifecycle::plugin_receipt::ReceiptDecision::Conflict => {
                anyhow::bail!("plugin receipt conflicts with durable evidence")
            }
        }
        ", plugin_execution_receipt: $plugin_receipt".to_owned()
    } else {
        String::new()
    };
    Ok(Some(plugin_receipt_field))
}

pub(super) async fn resolve_native_publication(
    terminalization: Terminalization<'_, '_>,
    publish_native_result: bool,
) -> Result<bool> {
    let Terminalization {
        txn,
        tool_doc_id,
        request_doc_id,
        node_did,
        requester_did,
        session_id,
        tool_call_id,
        call_id,
        spawned_by_tool_call_doc_id,
        plugin_effect,
        agent,
        session,
        requester_filter,
        ..
    } = terminalization;
    let receipt_key = format!("{session_id}:background-receipt:{tool_doc_id}");
    let receipt_key_escaped = escape_graphql_string(&receipt_key);
    let receipt_rows = txn.execute(&format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{receipt_key_escaped}" }}, session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }}{requester_filter} }}, limit: 2) {{ _docID }} }}"#
    )).await?;
    let receipt_rows = receipt_rows["data"]["AgentMessage"]
        .as_array()
        .context("background receipt lookup omitted messages")?;
    anyhow::ensure!(receipt_rows.len() <= 1, "background receipt is ambiguous");
    let publish_native_result =
        publish_native_result && receipt_rows.is_empty() && plugin_effect.is_none();

    if !publish_native_result && spawned_by_tool_call_doc_id.is_none() && plugin_effect.is_none() {
        ensure_background_receipt_before_bridge_close(
            txn,
            BackgroundReceiptKey {
                tool_call_doc_id: tool_doc_id,
                request_doc_id,
                node_did,
                requester_did,
                session_id,
                tool_call_id,
                call_id,
            },
        )
        .await?;
    }
    Ok(publish_native_result)
}

pub(super) async fn ensure_open_plugin_effect_has_no_terminal_output(
    terminalization: Terminalization<'_, '_>,
) -> Result<()> {
    let Terminalization {
        txn,
        plugin_effect,
        tool,
        ..
    } = terminalization;
    if plugin_effect.is_some() {
        let stored = txn
            .execute(&format!(
                r#"{{ AgentToolCall(docID: "{tool}") {{ terminal_output }} }}"#
            ))
            .await?;
        let rows = stored["data"]["AgentToolCall"]
            .as_array()
            .context("plugin terminal presentation lookup omitted rows")?;
        anyhow::ensure!(
            rows.len() == 1 && rows[0]["terminal_output"].is_null(),
            "open plugin effect already carries terminal output"
        );
    }
    Ok(())
}
