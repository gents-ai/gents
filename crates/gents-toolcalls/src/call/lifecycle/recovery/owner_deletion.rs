//! Whether the Trigger that started a background request, or the Task it
//! names, is deleted.
//!
//! Both are read strictly as the Node, with deleted documents shown. A
//! document the request names is deleted when the read returns it deleted or
//! does not return it at all: on a Vera home a deleted document's reads are
//! denied once its delete commits, so the Node no longer sees it. A read whose
//! access check did not answer, or failed, is an error and decides nothing, so
//! recovery tries again later.

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use defra_node::QueryResponse;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigClientError;
use gents_store::graphql::escape_graphql_string;
use serde::Deserialize;

use super::non_empty;

pub(super) async fn owner_task_deleted(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    parent: &AgentRequestRow,
) -> Result<bool> {
    let Some(trigger_id) = non_empty(parent.caused_by_trigger_id.as_deref()) else {
        return Ok(false);
    };
    let node_did = escape_graphql_string(node_did);
    let trigger_id = escape_graphql_string(trigger_id);
    let operation = "tool_call.recovery.owner_trigger";
    let read = gents_store::graphql::graphql_for_decision(
        node,
        &format!(
            r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{node_did}" }}, trigger_id: {{ _eq: "{trigger_id}" }} }}, showDeleted: true) {{ _deleted task_id }} }}"#
        ),
        operation,
    )
    .await;
    let task_id = match trigger_state(read, operation)? {
        TriggerState::Deleted => return Ok(true),
        TriggerState::Live { task_id } => task_id,
    };
    let Some(task_id) = non_empty(task_id.as_deref()) else {
        return Ok(false);
    };
    let task_id = escape_graphql_string(task_id);
    let operation = "tool_call.recovery.owner_task";
    let read = gents_store::graphql::graphql_for_decision(
        node,
        &format!(
            r#"{{ Task(filter: {{ node_did: {{ _eq: "{node_did}" }}, task_id: {{ _eq: "{task_id}" }} }}, showDeleted: true) {{ _deleted }} }}"#
        ),
        operation,
    )
    .await;
    task_deleted(read, operation)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum TriggerState {
    Deleted,
    Live { task_id: Option<String> },
}

/// The Trigger is live while the read returns a row of it not deleted.
pub(super) fn trigger_state(
    read: Result<QueryResponse, ConfigClientError>,
    operation: &str,
) -> Result<TriggerState> {
    let rows = deletion_rows(read, operation, "Trigger")?;
    Ok(match rows.into_iter().find(|row| !row.deleted) {
        Some(row) => TriggerState::Live {
            task_id: row.task_id,
        },
        None => TriggerState::Deleted,
    })
}

/// The Task is deleted when the read returns no row of it not deleted.
pub(super) fn task_deleted(
    read: Result<QueryResponse, ConfigClientError>,
    operation: &str,
) -> Result<bool> {
    Ok(deletion_rows(read, operation, "Task")?
        .iter()
        .all(|row| row.deleted))
}

#[derive(Debug, Deserialize)]
struct DeletionRow {
    #[serde(rename = "_deleted", default)]
    deleted: bool,
    #[serde(default)]
    task_id: Option<String>,
}

/// The rows of a complete read. A read that failed, or whose answer has no
/// list for `collection`, is an error: neither shows the document is gone.
fn deletion_rows(
    read: Result<QueryResponse, ConfigClientError>,
    operation: &str,
    collection: &str,
) -> Result<Vec<DeletionRow>> {
    let response = read.with_context(|| format!("{operation}: read {collection}"))?;
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .filter(|value| value.is_array())
        .cloned()
        .with_context(|| format!("{operation}: the answer has no {collection} list"))?;
    serde_json::from_value(value).with_context(|| format!("decode {collection} deletion rows"))
}

#[cfg(test)]
#[path = "owner_deletion_tests.rs"]
mod tests;
