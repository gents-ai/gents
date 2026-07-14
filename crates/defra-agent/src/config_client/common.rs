use anyhow::Result;
use serde_json::Value;

use super::ExistingDocumentRef;

pub(super) async fn query_documents_by_unique_value(
    txn: &super::ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    unique_value: &str,
    show_deleted: bool,
) -> Result<Vec<ExistingDocumentRef>> {
    use crate::graphql::escape_graphql_string;

    let show_deleted_arg = if show_deleted {
        "showDeleted: true, "
    } else {
        ""
    };
    let query = format!(
        r#"{{
            {collection_name}(
                {show_deleted_arg}filter: {{ {unique_field}: {{ _eq: "{unique_value}" }} }},
                limit: 16
            ) {{
                _docID
                _deleted
            }}
        }}"#,
        collection_name = collection_name,
        show_deleted_arg = show_deleted_arg,
        unique_field = unique_field,
        unique_value = escape_graphql_string(unique_value),
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    rows.into_iter()
        .map(|row| {
            Ok(ExistingDocumentRef {
                doc_id: row
                    .get("_docID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{collection_name} lookup row missing _docID for {unique_field}={unique_value}: {row}"
                        )
                    })?
                    .to_string(),
                deleted: row.get("_deleted").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

pub(super) fn select_existing_document(
    collection_name: &str,
    unique_field: &str,
    unique_value: &str,
    rows: &[ExistingDocumentRef],
) -> Result<Option<ExistingDocumentRef>> {
    let live_rows = rows.iter().filter(|row| !row.deleted).collect::<Vec<_>>();
    if live_rows.len() > 1 {
        anyhow::bail!(
            "multiple live {collection_name} documents share {unique_field}={unique_value}"
        );
    }
    if let Some(row) = live_rows.first() {
        return Ok(Some((*row).clone()));
    }

    let deleted_rows = rows.iter().filter(|row| row.deleted).collect::<Vec<_>>();
    if deleted_rows.len() > 1 {
        anyhow::bail!(
            "multiple deleted {collection_name} tombstones share {unique_field}={unique_value}"
        );
    }

    Ok(deleted_rows.first().map(|row| (*row).clone()))
}
