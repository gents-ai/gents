use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};
use crate::graphql::escape_graphql_string;

/// Runtime-owned EventTrigger fields the trigger engine writes back after a
/// fire attempt.
///
/// Each field is optional so callers can update a subset — the helper only
/// emits GraphQL input entries for the fields that are `Some`, leaving
/// apply-owned fields (`enabled`, `task_id`, `source_collection`,
/// `event_kind`, `filter`, `concurrency`) untouched. `fire_count_delta`
/// expresses the desired increment (typically `+1` on a successful fire); the
/// helper performs a read-then-write because DefraDB does not currently expose
/// atomic increments. Racing writes may undercount, which is acceptable for
/// PR 2 (fire_count is bookkeeping, not a correctness-critical counter).
#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub(crate) struct EventTriggerRuntimeUpdate {
    pub(crate) last_attempt_at: Option<String>,
    pub(crate) last_fired_source_doc_id: Option<String>,
    pub(crate) last_status: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) fire_count_delta: Option<i64>,
}

/// Update the runtime-owned fields on an `EventTrigger` document identified by
/// its apply-owned `trigger_id`.
///
/// Only writes fields present in `updates`; apply-owned fields (`enabled`,
/// `task_id`, `source_collection`, `event_kind`, `filter`, `concurrency`) are
/// never touched. Returns `Ok` even when the trigger doc is missing — the
/// caller is assumed to have raced a delete from apply, which the reconcile
/// path will resolve.
///
/// `fire_count_delta` triggers a read-then-write: the current `fire_count` is
/// loaded, the delta added, and the new value written. DefraDB does not
/// expose atomic increments today, so racing concurrent updates may
/// undercount; this is acceptable for the EventTrigger `fire_count` field per
/// the event-driven-tasks PR 2 plan.
#[allow(dead_code)]
pub(crate) async fn update_event_trigger_runtime_fields(
    node: &EmbeddedNode,
    trigger_id: &str,
    updates: EventTriggerRuntimeUpdate,
) -> Result<()> {
    // Short-circuit: nothing to write.
    if updates.last_attempt_at.is_none()
        && updates.last_fired_source_doc_id.is_none()
        && updates.last_status.is_none()
        && updates.last_error.is_none()
        && updates.fire_count_delta.is_none()
    {
        return Ok(());
    }

    // Resolve the current fire_count if we need to increment it. Also use this
    // to detect whether the trigger doc still exists (idempotent behavior on
    // a deleted trigger).
    let current_fire_count = if updates.fire_count_delta.is_some() {
        let escaped_trigger_id = escape_graphql_string(trigger_id);
        let query = format!(
            r#"{{
                EventTrigger(
                    filter: {{ trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                    limit: 1
                ) {{
                    fire_count
                }}
            }}"#
        );
        let resp = node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!(
                "query EventTrigger fire_count for runtime update failed: {:?}",
                resp.errors
            );
        }
        let rows = resp
            .data
            .as_ref()
            .and_then(|data| data.get("EventTrigger"))
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            // EventTrigger doc disappeared; nothing to update.
            tracing::info!(
                trigger_id,
                "EventTrigger doc missing during runtime update; skipping"
            );
            return Ok(());
        }
        rows.first()
            .and_then(|row| row.get("fire_count"))
            .and_then(|value| value.as_i64())
            .unwrap_or(0)
    } else {
        0
    };

    // Build the input literal with only the requested fields so apply-owned
    // fields are never overwritten.
    let mut entries: Vec<String> = Vec::new();
    if let Some(v) = updates.last_attempt_at.as_ref() {
        entries.push(format!("last_attempt_at: \"{}\"", escape_graphql_string(v)));
    }
    if let Some(v) = updates.last_fired_source_doc_id.as_ref() {
        entries.push(format!(
            "last_fired_source_doc_id: \"{}\"",
            escape_graphql_string(v)
        ));
    }
    if let Some(v) = updates.last_status.as_ref() {
        entries.push(format!("last_status: \"{}\"", escape_graphql_string(v)));
    }
    if let Some(v) = updates.last_error.as_ref() {
        entries.push(format!("last_error: \"{}\"", escape_graphql_string(v)));
    }
    if let Some(delta) = updates.fire_count_delta {
        let new_fire_count = current_fire_count.saturating_add(delta);
        entries.push(format!("fire_count: {new_fire_count}"));
    }
    let input_literal = format!("{{ {} }}", entries.join(", "));

    let escaped_trigger_id = escape_graphql_string(trigger_id);
    // Use a filter-based mutation so we key on the apply-owned trigger_id and
    // don't need to resolve the _docID separately. DefraDB matches at most one
    // trigger (trigger_id is unique) so this updates the single target doc.
    let mutation = format!(
        r#"mutation {{
            update_EventTrigger(
                filter: {{ trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                input: {input_literal}
            ) {{ _docID }}
        }}"#
    );

    crate::config_client::ConfigAccess::write_local(
        node,
        "document.update_event_trigger_runtime",
        &mutation,
    )
    .await?;

    Ok(())
}

/// Reusable event-source configuration. Trigger owns task selection, enabled
/// state, concurrency, and delivery observations. The existing event engine owns
/// group state independently for each typed trigger/callback consumer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventSource {
    pub agent_did: String,
    pub event_source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub source_collection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Absent/null means created, never all events. Explicit values use the
    /// existing event engine vocabulary and are validated before installation.
    pub event_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_field: Option<String>,
    /// Absent processes each document independently. A group requires a
    /// correlation_field; that same correlation remains available to templates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<EventGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_authority: Option<crate::toolset::WorkspaceAuthority>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

/// Completion conditions for documents sharing an event correlation.
/// Existing group deduplication, bounds, and durable ownership remain authoritative.
/// Shared validation requires correlation and expected_count or timeout_secs.
/// Counts/timeouts must be positive; min_count defaults to 1 and cannot exceed
/// a known expected count. Source-field counts are validated on delivery too.
/// Graph compilation additionally requires expected_count >= 2 and rejects
/// latest_only concurrency; shared types do not widen existing graph semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventGroup {
    /// Fixed count or count supplied by each source document, never both.
    /// Absence retains existing timeout-driven grouping, subject to validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_count: Option<EventGroupCount>,
    /// Time to wait for the group, not the task execution deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
    /// Minimum group size accepted on timeout; unset uses the existing default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum EventGroupCount {
    Fixed(i64),
    SourceField { source_field: String },
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
                correlation_field
                fire_mode
                expected_count
                expected_count_field
                group_timeout_secs
                group_min_count
                workspace_authority
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
                correlation_field
                fire_mode
                expected_count
                expected_count_field
                group_timeout_secs
                group_min_count
                workspace_authority
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
