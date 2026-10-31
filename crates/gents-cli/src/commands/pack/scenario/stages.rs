use gents::config_client::GraphqlEndpoint;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::manifest::{
    StageToolSequenceExpectation, TriggerRequestCountSource, WorkspaceReceiptPathExpectation,
};
use super::results::graphql_rows;
use super::tool_calls::verify_stage_tool_sequences;
use crate::graphql_access::post_graphql;
use gents::graphql::escape_graphql_string;
use gents_protocol::client_protocol::RequestLifecycleState;

#[derive(Debug, Clone)]
pub(super) struct StageResult {
    pub(super) trigger_id: String,
    pub(super) request_id: String,
    pub(super) lifecycle_state: RequestLifecycleState,
    pub(super) caused_by_source_doc_id: Option<String>,
}

async fn sourced_trigger_request_count(
    graphql: &GraphqlEndpoint,
    source: &TriggerRequestCountSource,
    correlation: &str,
) -> Result<Option<usize>> {
    let correlation = escape_graphql_string(correlation);
    if let (Some(match_field), Some(match_value)) = (
        source
            .match_field
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        source
            .match_value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ) {
        let query = format!(
            r#"{{ {collection}(filter: {{ {correlation_field}: {{ _eq: "{correlation}" }} }}) {{ {match_field} }} }}"#,
            collection = source.collection,
            correlation_field = source.correlation_field,
            match_field = match_field,
        );
        let rows = graphql_rows(graphql, &source.collection, &query).await?;
        if rows.is_empty() {
            return Ok(None);
        }
        let count = rows
            .iter()
            .filter(|row| {
                row.get(match_field).and_then(Value::as_str).map(str::trim) == Some(match_value)
            })
            .count();
        return Ok(Some(count));
    }
    let query = format!(
        r#"{{ {collection}(filter: {{ {correlation_field}: {{ _eq: "{correlation}" }} }}) {{ {expected_count_field} }} }}"#,
        collection = source.collection,
        correlation_field = source.correlation_field,
        expected_count_field = source.expected_count_field,
    );
    let rows = graphql_rows(graphql, &source.collection, &query).await?;
    let mut expected = None;
    for row in rows {
        let value = row.get(&source.expected_count_field).with_context(|| {
            format!(
                "{} row omitted expected count field {}",
                source.collection, source.expected_count_field
            )
        })?;
        let count =
            gents::graphql::canonical_positive_count(value, gents::MAX_EVENT_TRIGGER_GROUP_DOCS)
                .with_context(|| {
                    format!(
                        "{}.{} must be a canonical positive integer <= {}",
                        source.collection,
                        source.expected_count_field,
                        gents::MAX_EVENT_TRIGGER_GROUP_DOCS
                    )
                })?;
        if expected.is_some_and(|prior| prior != count) {
            bail!(
                "{}.{} is inconsistent for correlation {correlation}",
                source.collection,
                source.expected_count_field
            );
        }
        expected = Some(count);
    }
    Ok(expected)
}

async fn verify_workspace_receipt_paths(
    graphql: &GraphqlEndpoint,
    expected: &WorkspaceReceiptPathExpectation,
    correlation: &str,
) -> Result<()> {
    let escaped = escape_graphql_string(correlation);
    let unit_query = format!(
        r#"{{ {collection}(filter: {{ {correlation_field}: {{ _eq: "{escaped}" }} }}) {{ {work_unit_id_field} {owned_paths_field} }} }}"#,
        collection = expected.work_unit_collection,
        correlation_field = expected.correlation_field,
        work_unit_id_field = expected.work_unit_id_field,
        owned_paths_field = expected.owned_paths_field,
    );
    let unit_rows = graphql_rows(graphql, &expected.work_unit_collection, &unit_query).await?;
    let receipt_query = format!(
        r#"{{ WorkspaceReceipt(filter: {{ caused_by_correlation: {{ _eq: "{escaped}" }}, kind: {{ _eq: "writer" }} }}) {{ work_unit_id changed_files }} }}"#
    );
    let receipt_rows = graphql_rows(graphql, "WorkspaceReceipt", &receipt_query).await?;
    validate_workspace_receipt_path_rows(expected, &unit_rows, &receipt_rows)
}

pub(super) fn validate_workspace_receipt_path_rows(
    expected: &WorkspaceReceiptPathExpectation,
    unit_rows: &[Value],
    receipt_rows: &[Value],
) -> Result<()> {
    let mut ownership = BTreeMap::<String, BTreeSet<String>>::new();
    for row in unit_rows {
        let work_unit_id = row
            .get(&expected.work_unit_id_field)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .with_context(|| {
                format!(
                    "{}.{} must be a non-empty string",
                    expected.work_unit_collection, expected.work_unit_id_field
                )
            })?;
        let encoded = row
            .get(&expected.owned_paths_field)
            .and_then(Value::as_str)
            .with_context(|| {
                format!(
                    "{}.{} must be a JSON string array for {work_unit_id}",
                    expected.work_unit_collection, expected.owned_paths_field
                )
            })?;
        let paths: Vec<String> = serde_json::from_str(encoded).with_context(|| {
            format!(
                "{}.{} is not a JSON string array for {work_unit_id}",
                expected.work_unit_collection, expected.owned_paths_field
            )
        })?;
        let mut exact = BTreeSet::new();
        for path in paths {
            let canonical = !path.is_empty()
                && path.trim() == path
                && !Path::new(&path).is_absolute()
                && !path.contains('\\')
                && path
                    .split('/')
                    .all(|component| !component.is_empty() && !matches!(component, "." | ".."));
            if !canonical || !exact.insert(path.clone()) {
                bail!(
                    "{}.{} contains an invalid, non-canonical, or duplicate path for {work_unit_id}",
                    expected.work_unit_collection,
                    expected.owned_paths_field
                );
            }
        }
        if ownership.insert(work_unit_id.to_string(), exact).is_some() {
            bail!("duplicate work unit ownership declaration for {work_unit_id}");
        }
    }

    for receipt in receipt_rows {
        let work_unit_id = receipt
            .get("work_unit_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .context("writer WorkspaceReceipt.work_unit_id must be a non-empty string")?;
        let allowed = ownership.get(work_unit_id).with_context(|| {
            format!("writer WorkspaceReceipt references unknown work unit {work_unit_id}")
        })?;
        let changed: Vec<String> = match receipt.get("changed_files") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::String(encoded)) => serde_json::from_str(encoded).with_context(|| {
                format!("WorkspaceReceipt.changed_files is not a JSON string array for {work_unit_id}")
            })?,
            Some(_) => bail!(
                "WorkspaceReceipt.changed_files must be null or a JSON string array for {work_unit_id}"
            ),
        };
        for path in changed {
            if !allowed.contains(&path) {
                bail!(
                    "writer WorkspaceReceipt for {work_unit_id} changed out-of-scope path {path}; allowed paths: {}",
                    allowed.iter().cloned().collect::<Vec<_>>().join(", ")
                );
            }
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "each expectation comes from a separate scenario section"
)]
pub(super) async fn await_stages(
    graphql: &GraphqlEndpoint,
    trigger_ids: &[String],
    trigger_request_counts: &BTreeMap<String, usize>,
    trigger_request_count_sources: &BTreeMap<String, TriggerRequestCountSource>,
    stage_tool_sequences: &[StageToolSequenceExpectation],
    workspace_receipt_paths: Option<&WorkspaceReceiptPathExpectation>,
    correlation: &str,
    deadline: Duration,
) -> Result<Vec<StageResult>> {
    let started = Instant::now();
    loop {
        if let Some(expected) = workspace_receipt_paths {
            verify_workspace_receipt_paths(graphql, expected, correlation).await?;
        }
        let mut done: Vec<StageResult> = Vec::new();
        let mut resolved_counts = BTreeMap::new();
        for trigger_id in trigger_ids {
            let expected_count = if let Some(source) = trigger_request_count_sources.get(trigger_id)
            {
                sourced_trigger_request_count(graphql, source, correlation).await?
            } else {
                Some(trigger_request_counts.get(trigger_id).copied().unwrap_or(1))
            };
            resolved_counts.insert(trigger_id, expected_count);
            let query = stage_requests_query(trigger_id, correlation);
            let Ok(resp) = post_graphql(graphql, &query).await else {
                continue;
            };
            let Some(rows) = resp.pointer("/data/AgentRequest").and_then(Value::as_array) else {
                continue;
            };
            if expected_count.is_some_and(|expected_count| rows.len() > expected_count) {
                bail!(
                    "trigger {trigger_id} materialized {} requests for correlation {correlation}; expected {}",
                    rows.len(),
                    expected_count.expect("guard establishes expected count")
                );
            }
            for row in rows {
                let state = row
                    .get("lifecycle_state")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if RequestLifecycleState::is_terminal_str(Some(state)) {
                    let request_id = row
                        .get("request_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if RequestLifecycleState::parse_opt(Some(state))
                        != Some(RequestLifecycleState::Completed)
                    {
                        let reason = row
                            .get("failure_reason")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        bail!("trigger {trigger_id} request {request_id} ended {state}: {reason}");
                    }
                    let stage = StageResult {
                        trigger_id: trigger_id.clone(),
                        request_id: request_id.to_string(),
                        lifecycle_state: RequestLifecycleState::Completed,
                        caused_by_source_doc_id: row
                            .get("caused_by_source_doc_id")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    };
                    verify_stage_tool_sequences(graphql, &stage, stage_tool_sequences).await?;
                    done.push(stage);
                }
            }
        }
        let all_counts_resolved = resolved_counts.values().all(Option::is_some);
        let expected_total = resolved_counts
            .values()
            .filter_map(|count| *count)
            .sum::<usize>();
        if all_counts_resolved && done.len() == expected_total {
            return Ok(done);
        }
        // A trigger that fired and failed to materialize will never retry
        // (created/first-seen marks the source document seen); surface its own
        // last_error instead of waiting out the deadline.
        for trigger_id in trigger_ids {
            let Some(expected_count) = resolved_counts.get(trigger_id).copied().flatten() else {
                continue;
            };
            if done
                .iter()
                .filter(|stage| &stage.trigger_id == trigger_id)
                .count()
                == expected_count
            {
                continue;
            }
            if let Some(error) = trigger_error(graphql, trigger_id).await {
                bail!("trigger {trigger_id} fired but did not materialize: {error}");
            }
        }
        if started.elapsed() >= deadline {
            let seen: Vec<&str> = done.iter().map(|s| s.trigger_id.as_str()).collect();
            bail!(
                "timed out after {}s: reached a terminal state for [{}], expected [{}]",
                deadline.as_secs(),
                seen.join(", "),
                trigger_ids.join(", ")
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

pub(super) fn stage_requests_query(trigger_id: &str, correlation: &str) -> String {
    format!(
        r#"{{ AgentRequest(filter: {{
                    caused_by_trigger_id: {{ _eq: "{}" }},
                    caused_by_correlation: {{ _eq: "{}" }}
                }}) {{ request_id lifecycle_state failure_reason caused_by_source_doc_id }} }}"#,
        escape_graphql_string(trigger_id),
        escape_graphql_string(correlation),
    )
}

async fn trigger_error(graphql: &GraphqlEndpoint, trigger_id: &str) -> Option<String> {
    let query = format!(
        r#"{{ Trigger(filter: {{ trigger_id: {{ _eq: "{}" }} }}) {{ last_status last_error }} }}"#,
        escape_graphql_string(trigger_id)
    );
    let resp = post_graphql(graphql, &query).await.ok()?;
    let row = resp.pointer("/data/Trigger/0")?;
    if row.get("last_status").and_then(Value::as_str) != Some("error") {
        return None;
    }
    Some(
        row.get("last_error")
            .and_then(Value::as_str)
            .unwrap_or("(no last_error recorded)")
            .to_string(),
    )
}
