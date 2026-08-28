use anyhow::Result;
use gents::config_client::ConfigApplyTxn;
use gents::defra_node::EmbeddedNode;
use gents::graphql::graphql_with_transaction_retry;
use gents::retry::{
    defradb_conflict_retry_backoff, is_defradb_transaction_conflict_text,
    DEFRA_DB_CONFLICT_MAX_RETRIES,
};
use gents_protocol::transcript::present_persisted_message;
use serde_json::{json, Value};

use super::progress::response_field_is_blank;
use crate::materialized_message_query;

/// Route shim reads and auto-committed writes through the runtime's bounded
/// DefraDB conflict retry so overlapping reconciliation stays transparent to
/// Codex clients.
pub(super) async fn query_node_json(node: &EmbeddedNode, query: &str) -> Result<Value> {
    let response = graphql_with_transaction_retry(node, query, "codex shim store").await?;
    Ok(json!({
        "data": response.data.unwrap_or_else(|| json!({})),
    }))
}

/// Commit a mutation transactionally so DefraDB emits the `Update` event the
/// runtime control watcher consumes. A conflicted cycle commits nothing; the
/// bounded retry therefore preserves exactly one update for a successful call.
pub(super) async fn execute_committed(node: &EmbeddedNode, mutation: &str) -> Result<Value> {
    let mut retry_index = 0;
    loop {
        match execute_committed_once(node, mutation).await {
            Ok(value) => return Ok(value),
            Err(error)
                if retry_index < DEFRA_DB_CONFLICT_MAX_RETRIES
                    && is_defradb_transaction_conflict_text(&format!("{error:#}")) =>
            {
                let backoff = defradb_conflict_retry_backoff(retry_index);
                retry_index += 1;
                tracing::warn!(
                    retry_count = retry_index,
                    max_retries = DEFRA_DB_CONFLICT_MAX_RETRIES,
                    backoff_ms = backoff.as_millis() as u64,
                    error = %error,
                    "retrying Codex shim committed mutation after transaction conflict"
                );
                tokio::time::sleep(backoff).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn execute_committed_once(node: &EmbeddedNode, mutation: &str) -> Result<Value> {
    let txn = ConfigApplyTxn::begin_local(node, None).await?;
    match txn.execute(mutation).await {
        Ok(response) => {
            txn.commit().await?;
            Ok(response)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error.context("GENTS Codex shim mutation failed"))
        }
    }
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
