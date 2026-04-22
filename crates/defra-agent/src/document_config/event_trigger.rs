use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};
use crate::graphql::escape_graphql_string;

/// Description of an event-driven trigger for a task.
///
/// Mirrors the `EventTrigger` GraphQL schema in
/// `crates/defra-agent-protocol/schemas/agent/event_trigger.graphql`. Includes
/// both apply-owned fields (`trigger_id`, `task_id`, `source_collection`,
/// `event_kind`, `filter`, `enabled`, `concurrency`, `created_at`,
/// `updated_at`) and runtime-owned fields (`last_attempt_at`,
/// `last_fired_source_doc_id`, `last_status`, `last_error`, `fire_count`)
/// because `DocumentRuntimeView` is a DB-read view.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct EventTrigger {
    pub(crate) trigger_id: String,
    #[serde(default)]
    pub(crate) task_id: Option<String>,
    #[serde(default)]
    pub(crate) source_collection: Option<String>,
    #[serde(default)]
    pub(crate) event_kind: Option<String>,
    #[serde(default)]
    pub(crate) filter: Option<String>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
    #[serde(default)]
    pub(crate) concurrency: Option<String>,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
    #[serde(default)]
    pub(crate) updated_at: Option<String>,
    // runtime-owned:
    #[serde(default)]
    pub(crate) last_attempt_at: Option<String>,
    #[serde(default)]
    pub(crate) last_fired_source_doc_id: Option<String>,
    #[serde(default)]
    pub(crate) last_status: Option<String>,
    #[serde(default)]
    pub(crate) last_error: Option<String>,
    #[serde(default)]
    pub(crate) fire_count: Option<i64>,
}

/// List every `EventTrigger` document in the node, returning
/// `(doc_id, event_trigger)` pairs.
///
/// EventTriggers are addressed by a globally unique `trigger_id` (see
/// `event_trigger.graphql`), so this helper is not scoped by `agent_did`.
#[allow(dead_code)]
pub(crate) async fn list_event_trigger_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, EventTrigger)>> {
    let query = r#"{
            EventTrigger(order: { trigger_id: ASC }) {
                _docID
                trigger_id
                task_id
                source_collection
                event_kind
                filter
                enabled
                concurrency
                created_at
                updated_at
                last_attempt_at
                last_fired_source_doc_id
                last_status
                last_error
                fire_count
            }
        }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list EventTrigger failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "EventTrigger"))
}

/// Load a single `EventTrigger` document by its DefraDB `_docID`.
///
/// Used by the control watcher's update-dispatch path to classify an updated
/// document by collection when only the `_docID` is known.
#[allow(dead_code)]
pub(crate) async fn load_event_trigger_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, EventTrigger)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            EventTrigger(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                trigger_id
                task_id
                source_collection
                event_kind
                filter
                enabled
                concurrency
                created_at
                updated_at
                last_attempt_at
                last_fired_source_doc_id
                last_status
                last_error
                fire_count
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query EventTrigger by _docID failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "EventTrigger"))
}
