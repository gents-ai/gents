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
    use defra_agent::graphql::escape_graphql_string;

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

/// Mint a fresh document identity for a recreate over a tombstone.
///
/// Deletion is terminal in DefraDB and docIDs are content-addressed, so
/// recreating a row whose manifest content is IDENTICAL to the tombstoned one
/// would regenerate the tombstoned docID — and a create onto a tombstoned
/// docID does not produce a live row (#700). Stamping the apply time gives the
/// new incarnation distinct content, and with it a new identity.
pub(crate) fn mint_recreate_identity(add_doc: &serde_json::Value) -> serde_json::Value {
    let mut doc = add_doc.clone();
    if let Some(map) = doc.as_object_mut() {
        let now = chrono::Utc::now().to_rfc3339();
        map.insert(
            "updated_at".to_string(),
            serde_json::Value::String(now.clone()),
        );
        map.entry("created_at".to_string())
            .or_insert(serde_json::Value::String(now));
    }
    doc
}
