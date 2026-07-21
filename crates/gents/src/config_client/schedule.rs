use crate::graphql::escape_graphql_string;
use anyhow::Result;
use serde_json::Value;

use gents_protocol::graphql::graphql_input_literal;

use super::common::{
    mint_recreate_identity, query_documents_by_unique_value, select_existing_document,
};

/// Apply-path writer for the `Schedule` collection.
///
/// CRITICAL: `Schedule` is the boundary between apply-owned desired state
/// and runtime-owned live state. This writer must ONLY read, write, or
/// compare apply-owned fields: `schedule_id`, `task_id`, `interval_secs`,
/// `cron`, `timezone`, `missed_run_policy`, `enabled`, `concurrency`,
/// `created_at`, `updated_at`.
///
/// Runtime-owned fields — `next_run_at`, `last_attempt_at`, `last_status`,
/// `last_error`, `fire_count` — are the scheduler's responsibility. They are
/// never projected into mutation input, never pulled into the verification
/// SELECT, and never participate in `row_matches_expected`. This preserves
/// the contract that reapplying a manifest does not reset live scheduler
/// state.
pub async fn write_schedule_document(
    txn: &super::ConfigApplyTxn<'_>,
    schedule_id: &str,
    add_doc: &Value,
    update_doc: &Value,
) -> Result<String> {
    let existing = select_existing_document(
        "Schedule",
        "schedule_id",
        schedule_id,
        &query_documents_by_unique_value(txn, "Schedule", "schedule_id", schedule_id, true).await?,
    )?;

    let Some(existing) = existing.as_ref() else {
        return create_schedule_document(txn, schedule_id, add_doc).await;
    };
    if existing.deleted {
        let add_doc = mint_recreate_identity(add_doc);
        return create_schedule_document(txn, schedule_id, &add_doc).await;
    }

    let input_literal = graphql_input_literal(update_doc)?;
    let mutation = format!(
        r#"mutation {{
            update_Schedule(docID: "{doc_id}", input: {input_literal}) {{ _docID }}
        }}"#,
        doc_id = escape_graphql_string(&existing.doc_id),
        input_literal = input_literal,
    );

    let response = txn.execute(&mutation).await?;
    match gents_protocol::graphql::extract_mutation_doc_id(&response, "Schedule") {
        Ok(doc_id) => Ok(doc_id),
        Err(extract_error) => {
            let current = select_matching_schedule_row(txn, schedule_id, update_doc).await?;
            if let Some(row) = current {
                let current_doc_id = row
                    .get("_docID")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let deleted = row
                    .get("_deleted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !deleted
                    && current_doc_id == existing.doc_id
                    && schedule_row_matches_expected(&row, update_doc)?
                {
                    return Ok(current_doc_id);
                }
                return Err(anyhow::anyhow!(
                    "{}\nSchedule post-update row did not converge for schedule_id {}: {}",
                    extract_error,
                    schedule_id,
                    row
                ));
            }
            Err(anyhow::anyhow!(
                "{}\nSchedule schedule_id {} has no row after update attempt",
                extract_error,
                schedule_id
            ))
        }
    }
}

async fn create_schedule_document(
    txn: &super::ConfigApplyTxn<'_>,
    schedule_id: &str,
    add_doc: &Value,
) -> Result<String> {
    let input_literal = graphql_input_literal(add_doc)?;
    let mutation = format!(
        r#"mutation {{
            create_Schedule(input: {input_literal}) {{ _docID }}
        }}"#,
        input_literal = input_literal,
    );
    let response = txn.execute(&mutation).await?;
    match gents_protocol::graphql::extract_mutation_doc_id(&response, "Schedule") {
        Ok(doc_id) => Ok(doc_id),
        Err(extract_error) => {
            let current = select_matching_schedule_row(txn, schedule_id, add_doc).await?;
            if let Some(row) = current {
                let deleted = row
                    .get("_deleted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !deleted && schedule_row_matches_expected(&row, add_doc)? {
                    return row
                        .get("_docID")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Schedule live row missing _docID after recreate: {}",
                                row
                            )
                        });
                }
                return Err(anyhow::anyhow!(
                    "{}\nSchedule post-create row did not converge for schedule_id {}: {}",
                    extract_error,
                    schedule_id,
                    row
                ));
            }
            Err(anyhow::anyhow!(
                "{}\nSchedule schedule_id {} has no live row after create attempt",
                extract_error,
                schedule_id
            ))
        }
    }
}

async fn select_matching_schedule_row(
    txn: &super::ConfigApplyTxn<'_>,
    schedule_id: &str,
    expected: &Value,
) -> Result<Option<Value>> {
    let rows = query_schedule_rows(txn, schedule_id, true).await?;
    let live_rows = rows
        .into_iter()
        .filter(|row| row.get("_deleted").and_then(Value::as_bool) != Some(true))
        .collect::<Vec<_>>();
    if live_rows.len() > 1 {
        anyhow::bail!(
            "multiple live Schedule rows share schedule_id {} during post-write verification",
            schedule_id
        );
    }
    if let Some(row) = live_rows.into_iter().next() {
        if schedule_row_matches_expected(&row, expected)? {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

async fn query_schedule_rows(
    txn: &super::ConfigApplyTxn<'_>,
    schedule_id: &str,
    show_deleted: bool,
) -> Result<Vec<Value>> {
    let show_deleted_arg = if show_deleted {
        "showDeleted: true, "
    } else {
        ""
    };
    // Only apply-owned fields are selected. Runtime-owned fields
    // (next_run_at, last_attempt_at, last_status, last_error, fire_count)
    // are intentionally omitted so the apply path cannot see or compare
    // them — they belong to the scheduler.
    let query = format!(
        r#"{{
            Schedule(
                {show_deleted_arg}filter: {{ schedule_id: {{ _eq: "{schedule_id}" }} }},
                limit: 4
            ) {{
                _docID
                _deleted
                schedule_id
                task_id
                interval_secs
                cron
                timezone
                missed_run_policy
                enabled
                concurrency
                created_at
                updated_at
            }}
        }}"#,
        show_deleted_arg = show_deleted_arg,
        schedule_id = escape_graphql_string(schedule_id),
    );
    let response = txn.execute(&query).await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("Schedule"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Compares only the keys present in `expected`. Because the mutation input
/// contains only apply-owned keys, runtime-owned fields in the live row are
/// never inspected here — even if present and non-null, they can never cause
/// convergence to fail, preserving runtime ownership.
pub fn schedule_row_matches_expected(row: &Value, expected: &Value) -> Result<bool> {
    let expected = expected
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Schedule expected document must be an object"))?;
    let actual = row
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Schedule row must be an object"))?;
    Ok(expected
        .iter()
        .all(|(key, value)| actual.get(key).is_some_and(|actual| actual == value)))
}
