use anyhow::{anyhow, Result};

use super::error::DocumentConfigError;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

pub(crate) const TRIGGER_CURSOR_BATCH_SIZE: usize = 64;

#[derive(Debug, Default, Clone)]
pub(crate) struct TriggerRuntimeUpdate {
    pub(crate) next_run_at: Option<String>,
    pub(crate) last_attempt_at: Option<String>,
    pub(crate) last_fired_source_doc_id: Option<String>,
    pub(crate) last_status: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) fire_count_delta: Option<i64>,
}

fn render_trigger_runtime_mutation(
    escaped_owner: &str,
    escaped_trigger_id: &str,
    entries: &[String],
) -> String {
    let input_literal = format!("{{ {} }}", entries.join(", "));
    format!(
        r#"mutation {{
            update_Trigger(
                filter: {{ node_did: {{ _eq: "{escaped_owner}" }}, trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                input: {input_literal}
            ) {{ _docID }}
        }}"#
    )
}

pub(crate) async fn load_trigger_next_run_at_batch(
    node: &EmbeddedNode,
    scopes: &[(String, String)],
) -> Vec<Result<Option<String>>> {
    let mut results = std::iter::repeat_with(|| None)
        .take(scopes.len())
        .collect::<Vec<Option<Result<Option<String>>>>>();

    for (chunk_index, chunk) in scopes.chunks(TRIGGER_CURSOR_BATCH_SIZE).enumerate() {
        let chunk_start = chunk_index * TRIGGER_CURSOR_BATCH_SIZE;
        let mut selections = Vec::new();
        let mut aliases = Vec::new();
        for (index, (node_did, trigger_id)) in chunk.iter().enumerate() {
            let result_index = chunk_start + index;
            if node_did.is_empty() {
                results[result_index] = Some(Err(anyhow!("trigger observation owner is required")));
                continue;
            }
            let alias = format!("trigger_{index}");
            selections.push(format!(
                r#"{alias}: Trigger(filter: {{ node_did: {{ _eq: "{}" }}, trigger_id: {{ _eq: "{}" }} }}, limit: 2) {{ next_run_at }}"#,
                escape_graphql_string(node_did),
                escape_graphql_string(trigger_id),
            ));
            aliases.push((result_index, alias));
        }
        if selections.is_empty() {
            continue;
        }
        let query = format!("{{ {} }}", selections.join("\n"));
        #[cfg(test)]
        record_trigger_cursor_batch_query();
        let response = match crate::graphql::graphql_response_with_transaction_retry(
            node,
            &query,
            "query Trigger cursor batch",
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                for (result_index, _) in aliases {
                    results[result_index] =
                        Some(Err(anyhow!("query Trigger cursor failed: {error:#}")));
                }
                continue;
            }
        };
        let known_aliases = aliases
            .iter()
            .map(|(_, alias)| alias.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let global_errors = response
            .errors
            .iter()
            .filter(|error| {
                error
                    .path
                    .as_ref()
                    .and_then(|path| path.first())
                    .is_none_or(|alias| !known_aliases.contains(alias.as_str()))
            })
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>();

        for (result_index, alias) in aliases {
            let alias_errors = response
                .errors
                .iter()
                .filter(|error| {
                    error
                        .path
                        .as_ref()
                        .and_then(|path| path.first())
                        .is_some_and(|path_alias| path_alias == &alias)
                })
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>();
            if !alias_errors.is_empty() || !global_errors.is_empty() {
                let messages = alias_errors
                    .into_iter()
                    .chain(global_errors.iter().copied())
                    .collect::<Vec<_>>()
                    .join("; ");
                results[result_index] = Some(Err(anyhow!(
                    "query Trigger cursor for {alias} failed: {messages}"
                )));
                continue;
            }

            let rows = response
                .data
                .as_ref()
                .and_then(|data| data.get(alias.as_str()))
                .and_then(serde_json::Value::as_array);
            if let Some(rows) = rows {
                if rows.len() > 1 {
                    results[result_index] =
                        Some(Err(anyhow!("ambiguous scoped trigger observation")));
                    continue;
                }
            }
            results[result_index] = Some(Ok(rows
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("next_run_at"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)));
        }
    }

    results
        .into_iter()
        .map(|result| result.unwrap_or_else(|| Err(anyhow!("trigger cursor result was omitted"))))
        .collect()
}

#[cfg(test)]
tokio::task_local! {
    static TRIGGER_CURSOR_BATCH_QUERIES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

#[cfg(test)]
fn record_trigger_cursor_batch_query() {
    let _ = TRIGGER_CURSOR_BATCH_QUERIES
        .try_with(|queries| queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
}

#[cfg(test)]
pub(crate) async fn count_trigger_cursor_batch_queries<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = TRIGGER_CURSOR_BATCH_QUERIES
        .scope(queries.clone(), future)
        .await;
    (output, queries.load(std::sync::atomic::Ordering::Relaxed))
}

pub(crate) async fn update_trigger_runtime_fields(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    trigger_id: &str,
    updates: TriggerRuntimeUpdate,
) -> Result<()> {
    let escaped_owner = escape_graphql_string(node_did);
    anyhow::ensure!(
        !node_did.is_empty(),
        "trigger observation owner is required"
    );
    if updates.next_run_at.is_none()
        && updates.last_attempt_at.is_none()
        && updates.last_fired_source_doc_id.is_none()
        && updates.last_status.is_none()
        && updates.last_error.is_none()
        && updates.fire_count_delta.is_none()
    {
        return Ok(());
    }

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
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let Some(fire_count_delta) = updates.fire_count_delta else {
        let mutation =
            render_trigger_runtime_mutation(&escaped_owner, &escaped_trigger_id, &entries);
        crate::config_client::ConfigAccess::write_local(
            node,
            "document.update_trigger_runtime",
            &mutation,
        )
        .await?;
        return Ok(());
    };

    // Read and write in one transaction. Conflict retries rerun this whole
    // read-modify-write against a fresh snapshot, so concurrent committed
    // deltas cannot overwrite one another.
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "document.update_trigger_runtime",
        |txn| {
            let escaped_owner = escaped_owner.clone();
            let escaped_trigger_id = escaped_trigger_id.clone();
            let mut entries = entries.clone();
            Box::pin(async move {
                let query = format!(
                    r#"{{
                        Trigger(
                            filter: {{ node_did: {{ _eq: "{escaped_owner}" }}, trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                            limit: 2
                        ) {{ fire_count }}
                    }}"#
                );
                let response = txn.execute(&query).await?;
                let rows = response
                    .get("data")
                    .and_then(|data| data.get("Trigger"))
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                anyhow::ensure!(rows.len() <= 1, "ambiguous scoped trigger observation");
                let Some(row) = rows.first() else {
                    tracing::info!(
                        trigger_id,
                        "Trigger doc missing during runtime update; skipping"
                    );
                    return Ok(());
                };
                let current_fire_count = row
                    .get("fire_count")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                entries.push(format!(
                    "fire_count: {}",
                    current_fire_count.saturating_add(fire_count_delta)
                ));
                let mutation = render_trigger_runtime_mutation(
                    &escaped_owner,
                    &escaped_trigger_id,
                    &entries,
                );
                txn.execute(&mutation).await?;
                Ok(())
            })
        },
    )
    .await?;

    Ok(())
}

/// Reusable event-source configuration; the existing event engine owns group
/// state independently per typed trigger/callback consumer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EventSource {
    pub node_did: String,
    pub event_source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    pub source_collection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Absent/null means created, never all events.
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub event_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub correlation_field: Option<String>,
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
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// Completion conditions for documents sharing an event correlation.
/// Existing group deduplication, bounds, and durable ownership remain
/// authoritative. Graph compilation additionally requires expected_count >= 2
/// and refuses latest_only concurrency on every graph edge (Lean
/// `GraphPipeline.graphEdgeValid`); shared types do not widen graph semantics.

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
    pub fn validate(&self) -> std::result::Result<(), DocumentConfigError> {
        crate::graphql::validate_collection_identifier(&self.source_collection)?;
        if self.event_kind.as_deref().unwrap_or("created") != "created" {
            return Err(DocumentConfigError::EventSourceEventKind);
        }
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

    pub fn validate_group(&self) -> std::result::Result<(), DocumentConfigError> {
        use crate::runtime_snapshot::MAX_EVENT_TRIGGER_GROUP_DOCS;
        let correlation_field = self
            .correlation_field
            .as_deref()
            .filter(|field| !field.trim().is_empty());
        let Some(group) = self.group.as_ref() else {
            return Ok(());
        };
        if correlation_field.is_none() {
            return Err(DocumentConfigError::PerGroupRequiresCorrelation);
        }
        let expected_count = match &group.expected_count {
            Some(crate::document_config::EventGroupCount::Fixed(count)) => {
                let count = usize::try_from(*count)
                    .map_err(|_| DocumentConfigError::ExpectedCountNotPositive)?;
                if !(1..=MAX_EVENT_TRIGGER_GROUP_DOCS).contains(&count) {
                    return Err(DocumentConfigError::ExpectedCountOutOfBounds { count });
                }
                Some(count)
            }
            Some(crate::document_config::EventGroupCount::SourceField { source_field }) => {
                if crate::graphql::validate_graphql_name(source_field).is_err() {
                    return Err(DocumentConfigError::CountFieldNotName);
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
                if value <= 0 {
                    return Err(DocumentConfigError::GroupTimeoutNotPositive);
                }
                u64::try_from(value).map_err(|_| DocumentConfigError::GroupTimeoutOutOfRange)
            })
            .transpose()?;
        let group_min_count = group
            .min_count
            .map(|value| {
                if value <= 0 {
                    return Err(DocumentConfigError::GroupMinCountNotPositive);
                }
                usize::try_from(value).map_err(|_| DocumentConfigError::GroupMinCountOutOfRange)
            })
            .transpose()?;
        if let Some(min_count) = group_min_count {
            if min_count == 0 || min_count > MAX_EVENT_TRIGGER_GROUP_DOCS {
                return Err(DocumentConfigError::GroupMinCountOutOfBounds { min_count });
            }
            if group_timeout_secs.is_none() {
                return Err(DocumentConfigError::MinCountRequiresTimeout);
            }
        }
        if let (Some(expected), Some(min_count)) = (expected_count, group_min_count) {
            if min_count > expected {
                return Err(DocumentConfigError::MinCountExceedsExpected {
                    min_count,
                    expected,
                });
            }
        }
        match (expected_count, expected_count_field.as_ref()) {
            (Some(_), None) | (None, Some(_)) => {}
            (None, None) => {
                if group_timeout_secs.is_none() {
                    return Err(DocumentConfigError::PerGroupRequiresCount);
                }
            }
            (Some(_), Some(_)) => {
                return Err(DocumentConfigError::ExpectedCountAndSourceField);
            }
        }
        Ok(())
    }
}
