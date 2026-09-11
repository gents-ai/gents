use std::sync::Arc;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::graphql::graphql_with_transaction_retry;
use gents_protocol::transcript::present_persisted_message;
use serde_json::{json, Value};

use super::progress::response_field_is_blank;

/// Route shim reads and auto-committed writes through the runtime's bounded
/// DefraDB conflict retry so overlapping reconciliation stays transparent to
/// Codex clients.
pub(super) async fn query_node_json(node: &EmbeddedNode, query: &str) -> Result<Value> {
    let response = graphql_with_transaction_retry(node, query, "codex shim store").await?;
    Ok(json!({
        "data": response.data.unwrap_or_else(|| json!({})),
    }))
}

/// Route a mutation through the canonical committed-write owner.
pub(super) async fn write_committed(
    node: &Arc<EmbeddedNode>,
    operation: &'static str,
    mutation: &str,
) -> Result<Value> {
    ConfigAccess::Local(node.clone())
        .write(operation, mutation)
        .await
        .context("GENTS Codex shim mutation failed")
}

pub(super) async fn hydrate_materialized_response_content(
    node: &EmbeddedNode,
    response: &mut Value,
) -> Result<bool> {
    let content_blank = response_field_is_blank(response, "content");
    let reasoning_blank = response_field_is_blank(response, "reasoning");
    if !content_blank && !reasoning_blank {
        return Ok(true);
    }

    let Some(sequence) = response_materialized_sequence(response) else {
        return Ok(!content_blank || !reasoning_blank);
    };
    let Some(session_id) = response.get("session_id").and_then(Value::as_str) else {
        return Ok(!content_blank || !reasoning_blank);
    };

    let owner = response
        .get("agent_did")
        .and_then(Value::as_str)
        .context("materialized response omitted principal owner")?;
    let requester = match response.get("requester_did") {
        Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.as_str()),
        _ => anyhow::bail!("materialized response omitted exact requester scope"),
    };
    let request_doc = response
        .get("request_doc_id")
        .and_then(Value::as_str)
        .context("materialized response omitted physical request")?;
    let scope = gents::session::session_scope_filter(owner, session_id, requester);
    let physical = gents::graphql::escape_graphql_string(request_doc);
    let message_response = query_node_json(node, &format!(
        r#"{{AgentMessage(filter:{{{scope},request_doc_id:{{_eq:"{physical}"}},sequence:{{_eq:{sequence}}}}}){{role content reasoning sequence}}}}"#
    )).await?;
    let messages = message_response
        .pointer("/data/AgentMessage")
        .and_then(Value::as_array)
        .context("materialized message query omitted rows")?;
    anyhow::ensure!(
        messages.len() <= 1,
        "ambiguous materialized message identity"
    );
    let Some(message) = messages.first() else {
        return Ok(false);
    };
    let Some(role) = message.get("role").and_then(Value::as_str) else {
        return Ok(false);
    };
    let Some(content) = message.get("content").and_then(Value::as_str) else {
        return Ok(false);
    };

    let presentation = present_persisted_message(role, content);
    let Some(object) = response.as_object_mut() else {
        return Ok(false);
    };

    if content_blank && !presentation.body_markdown.trim().is_empty() {
        object.insert(
            "content".to_string(),
            Value::String(presentation.body_markdown),
        );
    }
    if reasoning_blank {
        if let Some(reasoning) = message
            .get("reasoning")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .or(presentation.reasoning_markdown)
            .filter(|value| !value.trim().is_empty())
        {
            object.insert("reasoning".to_string(), Value::String(reasoning));
        }
    }

    Ok(true)
}

fn response_materialized_sequence(response: &Value) -> Option<i64> {
    response
        .get("materialized_message_sequence")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
}
