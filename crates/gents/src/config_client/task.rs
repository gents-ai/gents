use crate::graphql::escape_graphql_string;
use anyhow::Result;
use serde_json::Value;

use gents_protocol::graphql::graphql_input_literal;

use super::common::{
    mint_recreate_identity, query_documents_by_unique_value, select_existing_document,
};

/// Apply-path writer for the `Task` collection.
///
/// All fields written by this path are apply-owned: `task_id`, `name`,
/// `description`, `behavior_id`, `prompt_template`, `enabled`,
/// `output_schema_ref`, `created_at`, `updated_at`. `Task` has no
/// runtime-owned fields today; the runtime-owned lifecycle lives on
/// `Schedule` and `Run`.
pub async fn write_task_document(
    txn: &super::ConfigApplyTxn<'_>,
    task_id: &str,
    add_doc: &Value,
    update_doc: &Value,
) -> Result<String> {
    let existing = select_existing_document(
        "Task",
        "task_id",
        task_id,
        &query_documents_by_unique_value(txn, "Task", "task_id", task_id, true).await?,
    )?;

    let Some(existing) = existing.as_ref() else {
        return create_task_document(txn, task_id, add_doc).await;
    };
    if existing.deleted {
        let add_doc = mint_recreate_identity(add_doc);
        return create_task_document(txn, task_id, &add_doc).await;
    }

    let input_literal = graphql_input_literal(update_doc)?;
    let mutation = format!(
        r#"mutation {{
            update_Task(docID: "{doc_id}", input: {input_literal}) {{ _docID }}
        }}"#,
        doc_id = escape_graphql_string(&existing.doc_id),
        input_literal = input_literal,
    );

    let response = txn.execute(&mutation).await?;
    match gents_protocol::graphql::extract_mutation_doc_id(&response, "Task") {
        Ok(doc_id) => Ok(doc_id),
        Err(extract_error) => {
            let current = select_matching_task_row(txn, task_id, update_doc).await?;
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
                    && task_row_matches_expected(&row, update_doc)?
                {
                    return Ok(current_doc_id);
                }
                return Err(anyhow::anyhow!(
                    "{}\nTask post-update row did not converge for task_id {}: {}",
                    extract_error,
                    task_id,
                    row
                ));
            }
            Err(anyhow::anyhow!(
                "{}\nTask task_id {} has no row after update attempt",
                extract_error,
                task_id
            ))
        }
    }
}

async fn create_task_document(
    txn: &super::ConfigApplyTxn<'_>,
    task_id: &str,
    add_doc: &Value,
) -> Result<String> {
    let input_literal = graphql_input_literal(add_doc)?;
    let mutation = format!(
        r#"mutation {{
            create_Task(input: {input_literal}) {{ _docID }}
        }}"#,
        input_literal = input_literal,
    );
    let response = txn.execute(&mutation).await?;
    match gents_protocol::graphql::extract_mutation_doc_id(&response, "Task") {
        Ok(doc_id) => Ok(doc_id),
        Err(extract_error) => {
            let current = select_matching_task_row(txn, task_id, add_doc).await?;
            if let Some(row) = current {
                let deleted = row
                    .get("_deleted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !deleted && task_row_matches_expected(&row, add_doc)? {
                    return row
                        .get("_docID")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            anyhow::anyhow!("Task live row missing _docID after recreate: {}", row)
                        });
                }
                return Err(anyhow::anyhow!(
                    "{}\nTask post-create row did not converge for task_id {}: {}",
                    extract_error,
                    task_id,
                    row
                ));
            }
            Err(anyhow::anyhow!(
                "{}\nTask task_id {} has no live row after create attempt",
                extract_error,
                task_id
            ))
        }
    }
}

async fn select_matching_task_row(
    txn: &super::ConfigApplyTxn<'_>,
    task_id: &str,
    expected: &Value,
) -> Result<Option<Value>> {
    let rows = query_task_rows(txn, task_id, true).await?;
    let live_rows = rows
        .into_iter()
        .filter(|row| row.get("_deleted").and_then(Value::as_bool) != Some(true))
        .collect::<Vec<_>>();
    if live_rows.len() > 1 {
        anyhow::bail!(
            "multiple live Task rows share task_id {} during post-write verification",
            task_id
        );
    }
    if let Some(row) = live_rows.into_iter().next() {
        if task_row_matches_expected(&row, expected)? {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

async fn query_task_rows(
    txn: &super::ConfigApplyTxn<'_>,
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
            Task(
                {show_deleted_arg}filter: {{ task_id: {{ _eq: "{task_id}" }} }},
                limit: 4
            ) {{
                _docID
                _deleted
                task_id
                name
                description
                behavior_id
                prompt_template
                enabled
                output_schema_ref
                created_at
                updated_at
            }}
        }}"#,
        show_deleted_arg = show_deleted_arg,
        task_id = escape_graphql_string(task_id),
    );
    let response = txn.execute(&query).await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("Task"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub fn task_row_matches_expected(row: &Value, expected: &Value) -> Result<bool> {
    let expected = expected
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Task expected document must be an object"))?;
    let actual = row
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Task row must be an object"))?;
    Ok(expected
        .iter()
        .all(|(key, value)| actual.get(key).is_some_and(|actual| actual == value)))
}
