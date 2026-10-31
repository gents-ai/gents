use gents::store::config_client::{ConfigAccess, GraphqlEndpoint};
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::manifest::{StageToolSequenceExpectation, ToolCallExpectation};
use super::stages::StageResult;

pub(super) async fn canonical_stage_tool_rows(
    graphql: &GraphqlEndpoint,
    request_id: &str,
) -> Result<Vec<Value>> {
    let access = ConfigAccess::Graphql(graphql.clone());
    let timeline =
        gents::observe::run_timeline_fetch::load_run_timeline_rows(&access, request_id).await?;
    let request_doc_id = timeline
        .request
        .doc_id
        .as_deref()
        .context("pack stage request has no physical document ID")?;
    timeline
        .tool_calls
        .iter()
        .filter(|tool| tool.request_doc_id.as_deref() == Some(request_doc_id))
        .map(|tool| {
            let mut row = serde_json::to_value(tool)?;
            row["_docID"] = json!(tool.doc_id);
            row["request_doc_id"] = json!(tool.request_doc_id);
            Ok(row)
        })
        .collect()
}

pub(super) async fn verify_stage_tool_sequences(
    graphql: &GraphqlEndpoint,
    stage: &StageResult,
    expectations: &[StageToolSequenceExpectation],
) -> Result<()> {
    for expected in expectations
        .iter()
        .filter(|expected| expected.trigger_id == stage.trigger_id)
    {
        let rows = canonical_stage_tool_rows(graphql, &stage.request_id).await?;
        verify_stage_tool_sequence_rows(stage, expected, &rows)?;
    }
    Ok(())
}

pub(super) fn verify_stage_tool_sequence_rows(
    stage: &StageResult,
    expected: &StageToolSequenceExpectation,
    rows: &[Value],
) -> Result<()> {
    let sequence_name_and_args = rows
        .iter()
        .map(|row| {
            let sequence = row
                .get("message_sequence")
                .and_then(Value::as_i64)
                .with_context(|| {
                    format!(
                        "trigger {} request {} has a tool call without message_sequence",
                        stage.trigger_id, stage.request_id
                    )
                })?;
            let name = row
                .get("tool_name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .with_context(|| {
                    format!(
                        "trigger {} request {} has a tool call without tool_name",
                        stage.trigger_id, stage.request_id
                    )
                })?;
            let args = row.get("args").and_then(Value::as_str).with_context(|| {
                format!(
                    "trigger {} request {} has a tool call without args",
                    stage.trigger_id, stage.request_id
                )
            })?;
            Ok((sequence, name, args))
        })
        .collect::<Result<Vec<_>>>()?;
    let boundary_sequence = sequence_name_and_args
        .iter()
        .filter(|(_, name, _)| *name == expected.boundary_tool_name)
        .map(|(sequence, _, _)| *sequence)
        .min()
        .with_context(|| {
            format!(
                "trigger {} request {} never called boundary tool {}",
                stage.trigger_id, stage.request_id, expected.boundary_tool_name
            )
        })?;
    let calls_before_boundary = sequence_name_and_args
        .iter()
        .filter(|(sequence, _, _)| *sequence < boundary_sequence)
        .count();
    if calls_before_boundary > expected.max_calls_before_boundary {
        bail!(
            "trigger {} request {} made {} tool calls before {}; maximum is {}",
            stage.trigger_id,
            stage.request_id,
            calls_before_boundary,
            expected.boundary_tool_name,
            expected.max_calls_before_boundary
        );
    }
    let mut calls_by_message = BTreeMap::new();
    for (sequence, _, _) in sequence_name_and_args
        .iter()
        .filter(|(sequence, _, _)| *sequence < boundary_sequence)
    {
        *calls_by_message.entry(sequence).or_insert(0usize) += 1;
    }
    if let Some((sequence, count)) = calls_by_message
        .into_iter()
        .find(|(_, count)| *count > expected.max_calls_per_message_before_boundary)
    {
        bail!(
            "trigger {} request {} made {} tool calls in pre-boundary message {}; maximum is {}",
            stage.trigger_id,
            stage.request_id,
            count,
            sequence,
            expected.max_calls_per_message_before_boundary
        );
    }
    // The provider may repeat a byte-identical deterministic write while
    // checking its own work; count identical write arguments once. A
    // conflicting retry still fails closed.
    let boundary_attempts = sequence_name_and_args
        .iter()
        .filter(|(_, name, _)| *name == expected.boundary_tool_name)
        .count();
    let boundary_calls = sequence_name_and_args
        .iter()
        .filter(|(_, name, _)| *name == expected.boundary_tool_name)
        .map(|(_, _, args)| *args)
        .collect::<BTreeSet<_>>()
        .len();
    if boundary_calls != expected.exact_boundary_calls {
        bail!(
            "trigger {} request {} made {} distinct {} calls across {} attempts; expected exactly {} distinct calls",
            stage.trigger_id,
            stage.request_id,
            boundary_calls,
            expected.boundary_tool_name,
            boundary_attempts,
            expected.exact_boundary_calls
        );
    }
    let disallowed = sequence_name_and_args
        .iter()
        .filter(|(sequence, name, _)| {
            *sequence >= boundary_sequence
                && !expected
                    .allowed_at_or_after_boundary
                    .iter()
                    .any(|allowed| allowed == *name)
        })
        .map(|(_, name, _)| *name)
        .collect::<Vec<_>>();
    if !disallowed.is_empty() {
        bail!(
            "trigger {} request {} called disallowed tools at or after {} began: {}",
            stage.trigger_id,
            stage.request_id,
            expected.boundary_tool_name,
            disallowed.join(", ")
        );
    }
    Ok(())
}

pub(super) async fn verify_tool_call_expectations(
    graphql: &GraphqlEndpoint,
    stages: &[StageResult],
    expectations: &[ToolCallExpectation],
) -> Result<()> {
    for expected in expectations {
        let matching_stages = stages
            .iter()
            .filter(|stage| stage.trigger_id == expected.trigger_id)
            .collect::<Vec<_>>();
        if matching_stages.is_empty() {
            return Err(anyhow::anyhow!(
                "tool call expectation references unknown trigger {}",
                expected.trigger_id
            ));
        }
        let mut all_rows = Vec::new();
        let mut matched = false;
        for stage in matching_stages {
            let rows = canonical_stage_tool_rows(graphql, &stage.request_id).await?;
            matched |= rows.iter().any(|row| tool_call_matches(row, expected));
            all_rows.extend(rows);
        }
        if !matched {
            bail!(
                "no completed {} call for {} matched action={:?} file={:?} symbol={:?} result_contains={:?}; rows={all_rows:?}",
                expected.tool_name,
                expected.trigger_id,
                expected.action,
                expected.file,
                expected.symbol,
                expected.result_contains
            );
        }
    }
    Ok(())
}

fn tool_call_matches(row: &Value, expected: &ToolCallExpectation) -> bool {
    if row.get("tool_name").and_then(Value::as_str) != Some(expected.tool_name.as_str()) {
        return false;
    }
    let completed = row.get("lifecycle_state").and_then(Value::as_str) == Some("completed");
    if !completed {
        return false;
    }
    let parsed = row
        .get("args")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    for (field, expected_value) in [
        ("action", expected.action.as_deref()),
        ("file", expected.file.as_deref()),
        ("symbol", expected.symbol.as_deref()),
    ] {
        let Some(expected_value) = expected_value else {
            continue;
        };
        let Some(actual) = parsed
            .as_ref()
            .and_then(|value| value.get(field))
            .and_then(Value::as_str)
        else {
            return false;
        };
        let matches = if field == "file" {
            gents::tools::native::result_path_matches(expected_value, actual)
        } else {
            actual == expected_value
        };
        if !matches {
            return false;
        }
    }
    let result = row.get("result").and_then(Value::as_str).unwrap_or("");
    if result_looks_failed(result) {
        return false;
    }
    expected
        .result_contains
        .iter()
        .all(|needle| result.contains(needle))
}

fn result_looks_failed(result: &str) -> bool {
    gents::tools::native::result_looks_failed(result)
}
