use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

/// Sparse runtime observations for the canonical Trigger. Setting a status
/// clears a previous error unless a replacement error is supplied. Fire counts
/// are observational bookkeeping, never delivery/idempotence authority.
#[derive(Debug, Default, Clone)]
pub(crate) struct TriggerRuntimeUpdate {
    pub(crate) next_run_at: Option<String>,
    pub(crate) last_attempt_at: Option<String>,
    pub(crate) last_fired_source_doc_id: Option<String>,
    pub(crate) last_status: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) fire_count_delta: Option<i64>,
}

/// Read the scheduling cursor of an exact owner-scoped trigger. The reusable
/// Schedule contains cadence only; another trigger may reference the same cadence.
pub(crate) async fn load_trigger_next_run_at(
    node: &EmbeddedNode,
    agent_did: &str,
    trigger_id: &str,
) -> Result<Option<String>> {
    anyhow::ensure!(
        !agent_did.is_empty(),
        "trigger observation owner is required"
    );
    let query = format!(
        r#"{{ Trigger(filter: {{ agent_did: {{ _eq: "{}" }}, trigger_id: {{ _eq: "{}" }} }}, limit: 2) {{ next_run_at }} }}"#,
        escape_graphql_string(agent_did),
        escape_graphql_string(trigger_id)
    );
    let response = node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "query Trigger cursor failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("Trigger"))
        .and_then(|rows| rows.as_array());
    anyhow::ensure!(
        rows.map_or(0, Vec::len) <= 1,
        "ambiguous scoped trigger observation"
    );
    Ok(rows
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("next_run_at"))
        .and_then(|value| value.as_str())
        .map(str::to_owned))
}

/// Update only runtime observations on the exact owner-scoped trigger.
/// Deleted triggers are harmless; concurrent fire-count updates may undercount.
pub(crate) async fn update_trigger_runtime_fields(
    node: &EmbeddedNode,
    agent_did: &str,
    trigger_id: &str,
    updates: TriggerRuntimeUpdate,
) -> Result<()> {
    let escaped_owner = escape_graphql_string(agent_did);
    anyhow::ensure!(
        !agent_did.is_empty(),
        "trigger observation owner is required"
    );
    // Short-circuit: nothing to write.
    if updates.next_run_at.is_none()
        && updates.last_attempt_at.is_none()
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
                Trigger(
                    filter: {{ agent_did: {{ _eq: "{escaped_owner}" }}, trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                    limit: 2
                ) {{
                    fire_count
                }}
            }}"#
        );
        let resp = node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!(
                "query Trigger fire_count for runtime update failed: {:?}",
                resp.errors
            );
        }
        let rows = resp
            .data
            .as_ref()
            .and_then(|data| data.get("Trigger"))
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        anyhow::ensure!(rows.len() <= 1, "ambiguous scoped trigger observation");
        if rows.is_empty() {
            // Trigger doc disappeared; nothing to update.
            tracing::info!(
                trigger_id,
                "Trigger doc missing during runtime update; skipping"
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
    if let Some(value) = &updates.next_run_at {
        entries.push(format!("next_run_at: \"{}\"", escape_graphql_string(value)));
    }
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
    if updates.last_error.is_none() && updates.last_status.is_some() {
        entries.push("last_error: null".into());
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
            update_Trigger(
                filter: {{ agent_did: {{ _eq: "{escaped_owner}" }}, trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                input: {input_literal}
            ) {{ _docID }}
        }}"#
    );

    crate::config_client::ConfigAccess::write_local(
        node,
        "document.update_trigger_runtime",
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EventSource {
    pub agent_did: String,
    pub event_source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    pub source_collection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Absent/null means created, never all events. Explicit values use the
    /// existing event engine vocabulary and are validated before installation.
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub event_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub correlation_field: Option<String>,
    /// Absent processes each document independently. A group requires a
    /// correlation_field; that same correlation remains available to templates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub group: Option<EventGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub workspace_authority: Option<crate::toolset::WorkspaceAuthority>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub updated_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EventGroup {
    /// Fixed count or count supplied by each source document, never both.
    /// Absence retains existing timeout-driven grouping, subject to validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub expected_count: Option<EventGroupCount>,
    /// Time to wait for the group, not the task execution deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub timeout_secs: Option<i64>,
    /// Minimum group size accepted on timeout; unset uses the existing default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub min_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(untagged)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EventGroupCount {
    Fixed(i64),
    SourceField { source_field: String },
}

impl EventSource {
    /// Shared matching configuration validation for admission and runtime loads.
    pub fn validate(&self) -> anyhow::Result<()> {
        crate::graphql::validate_collection_identifier(&self.source_collection)?;
        anyhow::ensure!(
            self.event_kind.as_deref().unwrap_or("created") == "created",
            "event source supports only the created event kind"
        );
        if let Some(filter) = self
            .filter
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            crate::graphql::validate_graphql_filter_fragment(filter)?;
        }
        if let Some(field) = self
            .correlation_field
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            crate::graphql::validate_graphql_name(field)?;
        }
        self.validate_group()
    }

    /// Validate group completion conditions before admission or runtime projection.
    /// Missing grouping processes each document independently; explicit malformed
    /// counts and timeouts are errors, never defaults.
    pub fn validate_group(&self) -> anyhow::Result<()> {
        use crate::runtime_snapshot::MAX_EVENT_TRIGGER_GROUP_DOCS;
        let correlation_field = self
            .correlation_field
            .as_deref()
            .filter(|field| !field.trim().is_empty());
        let Some(group) = self.group.as_ref() else {
            // per_document: no grouping configuration is allowed.
            return Ok(());
        };

        if correlation_field.is_none() {
            anyhow::bail!("per_group source requires correlation_field");
        }

        let expected_count = match &group.expected_count {
            Some(crate::document_config::EventGroupCount::Fixed(count)) => {
                let count = usize::try_from(*count)
                    .map_err(|_| anyhow::anyhow!("expected_count must be a positive count"))?;
                if !(1..=MAX_EVENT_TRIGGER_GROUP_DOCS).contains(&count) {
                    anyhow::bail!(
                        "expected_count {count} exceeds the group bound \
                     {MAX_EVENT_TRIGGER_GROUP_DOCS}"
                    );
                }
                Some(count)
            }
            Some(crate::document_config::EventGroupCount::SourceField { source_field }) => {
                if crate::graphql::validate_graphql_name(source_field).is_err() {
                    anyhow::bail!("count field is not a GraphQL name");
                }
                None
            }
            None => None,
        };
        let expected_count_field = match &group.expected_count {
            Some(crate::document_config::EventGroupCount::SourceField { source_field }) => {
                Some(source_field.clone())
            }
            _ => None,
        };

        let group_timeout_secs = group
            .timeout_secs
            .map(|value| {
                anyhow::ensure!(value > 0, "group timeout_secs must be positive");
                u64::try_from(value)
                    .map_err(|_| anyhow::anyhow!("group timeout_secs exceeds the supported range"))
            })
            .transpose()?;
        let group_min_count = group
            .min_count
            .map(|value| {
                anyhow::ensure!(value > 0, "group min_count must be positive");
                usize::try_from(value)
                    .map_err(|_| anyhow::anyhow!("group min_count exceeds the supported range"))
            })
            .transpose()?;
        if let Some(min_count) = group_min_count {
            if min_count == 0 || min_count > MAX_EVENT_TRIGGER_GROUP_DOCS {
                anyhow::bail!(
                    "min_count {min_count} must be within 1..={MAX_EVENT_TRIGGER_GROUP_DOCS}"
                );
            }
            if group_timeout_secs.is_none() {
                anyhow::bail!("min_count requires a positive group timeout_secs");
            }
        }
        if let (Some(expected), Some(min_count)) = (expected_count, group_min_count) {
            if min_count > expected {
                anyhow::bail!("min_count {min_count} exceeds expected_count {expected}");
            }
        }
        match (expected_count, expected_count_field.as_ref()) {
            (Some(_), None) | (None, Some(_)) => {}
            (None, None) => {
                if group_timeout_secs.is_none() {
                    anyhow::bail!(
                        "per_group without a fixed or source-field count requires \
                     a positive timeout_secs"
                    );
                }
            }
            (Some(_), Some(_)) => {
                anyhow::bail!("expected_count cannot combine a fixed count and a source field");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "event_group_validation_tests.rs"]
mod group_validation_tests;
