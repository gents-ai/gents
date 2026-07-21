use anyhow::Result;
use gents::defra_node::{EmbeddedNode, QueryRequest};
use gents_protocol::transcript::present_persisted_message;
use serde_json::{json, Value};

use super::progress::response_field_is_blank;
use crate::materialized_message_query;

pub(super) async fn query_node_json(node: &EmbeddedNode, query: &str) -> Result<Value> {
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!("GENTS Codex shim query failed: {:?}", response.errors);
    }
    Ok(json!({
        "data": response.data.unwrap_or_else(|| json!({})),
    }))
}

/// Run a write mutation inside a transaction and commit it. A bare
/// auto-committed single mutation (`node.execute`) does NOT emit the DefraDB
/// `Update` event the runtime control watcher reconciles on, but a transaction
/// COMMIT does. Routing skill enable/disable writes through here lets a running
/// agent pick up Codex-driven toggles without a restart — matching the
/// `config skill` CLI path (#340). Mirrors the Local arm of
/// `config_writes::txn::ConfigApplyTxn`.
pub(super) async fn execute_committed(node: &EmbeddedNode, mutation: &str) -> Result<Value> {
    let handle = node
        .runner()
        .begin_txn(false)
        .await
        .map_err(|error| anyhow::anyhow!("begin_txn: {error}"))?;
    let request = QueryRequest::new(mutation);
    let response = node.runner().execute_in_txn(request, &handle).await;
    if response.has_errors() {
        let _ = node.runner().rollback_txn(&handle).await;
        anyhow::bail!("GENTS Codex shim mutation failed: {:?}", response.errors);
    }
    node.runner()
        .commit_txn(&handle)
        .await
        .map_err(|error| anyhow::anyhow!("commit_txn: {error}"))?;
    Ok(json!({
        "data": response.data.unwrap_or_else(|| json!({})),
    }))
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

    let message_response =
        query_node_json(node, &materialized_message_query(session_id, sequence)).await?;
    let Some(message) = message_response
        .pointer("/data/AgentMessage")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
    else {
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

    // A terminal response can legitimately materialize to no visible text.
    // Once the referenced AgentMessage exists, the Codex turn can finish even
    // if there is no final assistant text to stream.
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
