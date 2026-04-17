use anyhow::Result;
use defra_agent::graphql::escape_graphql_string;
use serde_json::Value;

use crate::config_writes::ConfigAccess;
use crate::graphql_input_literal;

use super::common::{query_documents_by_unique_value, select_existing_document};

pub(crate) async fn write_scheduled_task_document(
    access: &ConfigAccess,
    task_id: &str,
    add_doc: &Value,
    update_doc: &Value,
) -> Result<String> {
    let existing = select_existing_document(
        "ScheduledTask",
        "task_id",
        task_id,
        &query_documents_by_unique_value(access, "ScheduledTask", "task_id", task_id, true).await?,
    )?;

    let Some(existing) = existing.as_ref() else {
        return create_scheduled_task_document(access, task_id, add_doc).await;
    };
    if existing.deleted {
        return create_scheduled_task_document(access, task_id, add_doc).await;
    }

    let input_literal = graphql_input_literal(update_doc)?;
    let mutation = format!(
        r#"mutation {{
            update_ScheduledTask(docID: "{doc_id}", input: {input_literal}) {{ _docID }}
        }}"#,
        doc_id = escape_graphql_string(&existing.doc_id),
        input_literal = input_literal,
    );

    let response = access.execute(&mutation).await?;
    match crate::extract_mutation_doc_id(&response, "ScheduledTask") {
        Ok(doc_id) => Ok(doc_id),
        Err(extract_error) => {
            let current = select_matching_scheduled_task_row(access, task_id, update_doc).await?;
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
                    && scheduled_task_row_matches_expected(&row, update_doc)?
                {
                    return Ok(current_doc_id);
                }
                return Err(anyhow::anyhow!(
                    "{}\nScheduledTask post-update row did not converge for task_id {}: {}",
                    extract_error,
                    task_id,
                    row
                ));
            }
            Err(anyhow::anyhow!(
                "{}\nScheduledTask task_id {} has no row after update attempt",
                extract_error,
                task_id
            ))
        }
    }
}

async fn create_scheduled_task_document(
    access: &ConfigAccess,
    task_id: &str,
    add_doc: &Value,
) -> Result<String> {
    let input_literal = graphql_input_literal(add_doc)?;
    let mutation = format!(
        r#"mutation {{
            create_ScheduledTask(input: {input_literal}) {{ _docID }}
        }}"#,
        input_literal = input_literal,
    );
    let response = access.execute(&mutation).await?;
    match crate::extract_mutation_doc_id(&response, "ScheduledTask") {
        Ok(doc_id) => Ok(doc_id),
        Err(extract_error) => {
            let current = select_matching_scheduled_task_row(access, task_id, add_doc).await?;
            if let Some(row) = current {
                let deleted = row
                    .get("_deleted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !deleted && scheduled_task_row_matches_expected(&row, add_doc)? {
                    return row
                        .get("_docID")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "ScheduledTask live row missing _docID after recreate: {}",
                                row
                            )
                        });
                }
                return Err(anyhow::anyhow!(
                    "{}\nScheduledTask post-create row did not converge for task_id {}: {}",
                    extract_error,
                    task_id,
                    row
                ));
            }
            Err(anyhow::anyhow!(
                "{}\nScheduledTask task_id {} has no live row after create attempt",
                extract_error,
                task_id
            ))
        }
    }
}

async fn select_matching_scheduled_task_row(
    access: &ConfigAccess,
    task_id: &str,
    expected: &Value,
) -> Result<Option<Value>> {
    let rows = query_scheduled_task_rows(access, task_id, true).await?;
    let live_rows = rows
        .into_iter()
        .filter(|row| row.get("_deleted").and_then(Value::as_bool) != Some(true))
        .collect::<Vec<_>>();
    if live_rows.len() > 1 {
        anyhow::bail!(
            "multiple live ScheduledTask rows share task_id {} during post-write verification",
            task_id
        );
    }
    if let Some(row) = live_rows.into_iter().next() {
        if scheduled_task_row_matches_expected(&row, expected)? {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

async fn query_scheduled_task_rows(
    access: &ConfigAccess,
    task_id: &str,
    show_deleted: bool,
) -> Result<Vec<Value>> {
    let show_deleted_arg = if show_deleted {
        "showDeleted: true, "
    } else {
        ""
    };
    let query = format!(
        r#"{{
            ScheduledTask(
                {show_deleted_arg}filter: {{ task_id: {{ _eq: "{task_id}" }} }},
                limit: 4
            ) {{
                _docID
                _deleted
                task_id
                agent_did
                behavior_id
                name
                prompt
                interval_secs
                enabled
                next_run_at
                last_run_at
                last_status
                last_error
                run_count
                created_at
                updated_at
            }}
        }}"#,
        show_deleted_arg = show_deleted_arg,
        task_id = escape_graphql_string(task_id),
    );
    let response = access.execute(&query).await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("ScheduledTask"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub(crate) fn scheduled_task_row_matches_expected(row: &Value, expected: &Value) -> Result<bool> {
    let expected = expected
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("ScheduledTask expected document must be an object"))?;
    let actual = row
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("ScheduledTask row must be an object"))?;
    Ok(expected
        .iter()
        .all(|(key, value)| actual.get(key).is_some_and(|actual| actual == value)))
}
