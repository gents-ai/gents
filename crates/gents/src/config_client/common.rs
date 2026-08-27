use anyhow::Result;
use serde_json::Value;
use std::sync::atomic::{AtomicI64, Ordering};

use super::ExistingDocumentRef;

pub(super) async fn query_documents_by_unique_value(
    txn: &super::ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    unique_value: &str,
    show_deleted: bool,
) -> Result<Vec<ExistingDocumentRef>> {
    if show_deleted {
        // Never let a long tombstone history consume the query limit before a
        // live row is observed. Live rows decide update-vs-create, so resolve
        // them in a dedicated query first. Only when there is no live row do
        // we need a single tombstone as evidence that the add branch must mint
        // a new content-addressed identity.
        let live = query_documents_by_unique_value_once(
            txn,
            collection_name,
            unique_field,
            unique_value,
            false,
            2,
        )
        .await?;
        if !live.is_empty() {
            return Ok(live);
        }

        return query_documents_by_unique_value_once(
            txn,
            collection_name,
            unique_field,
            unique_value,
            true,
            1,
        )
        .await;
    }

    query_documents_by_unique_value_once(txn, collection_name, unique_field, unique_value, false, 2)
        .await
}

async fn query_documents_by_unique_value_once(
    txn: &super::ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    unique_value: &str,
    show_deleted: bool,
    limit: usize,
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
                limit: {limit}
            ) {{
                _docID
                _deleted
            }}
        }}"#,
        collection_name = collection_name,
        show_deleted_arg = show_deleted_arg,
        unique_field = unique_field,
        unique_value = escape_graphql_string(unique_value),
        limit = limit,
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

    // Tombstones are history, not an ambiguity: every successful recreation
    // deliberately adds another one. Any tombstone is sufficient to select a
    // freshly-stamped create branch when no live row exists.
    Ok(rows.iter().find(|row| row.deleted).cloned())
}

// Process-local monotonicity closes the practical collision window for rapid
// retries, concurrent apply workers, and wall-clock rollback while the process
// is alive. A process restart cannot make a system clock globally monotonic;
// DefraDB's unique/live checks still reject a collision safely, and a retry
// later in that process mints from a new observation. The i64 nanosecond
// representation reaches its chrono boundary in 2262.
static LAST_RECREATE_IDENTITY_NANOS: AtomicI64 = AtomicI64::new(i64::MIN);

/// Mint the timestamp carried only by document add/recreate branches.
pub fn mint_recreate_identity_timestamp() -> String {
    let now = chrono::Utc::now()
        .timestamp_nanos_opt()
        .expect("current time must fit chrono's nanosecond timestamp range");
    let mut observed = LAST_RECREATE_IDENTITY_NANOS.load(Ordering::Relaxed);
    loop {
        let candidate = now.max(observed.saturating_add(1));
        match LAST_RECREATE_IDENTITY_NANOS.compare_exchange_weak(
            observed,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                return chrono::DateTime::<chrono::Utc>::from_timestamp_nanos(candidate)
                    .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
            }
            Err(actual) => observed = actual,
        }
    }
}

/// Mint a fresh document identity for an apply-owned create.
///
/// Deletion is terminal in DefraDB and docIDs are content-addressed, so
/// recreating a row whose manifest content is IDENTICAL to the tombstoned one
/// would regenerate the tombstoned docID. Every apply-controlled collection
/// carries `updated_at`, so stamping the add branch gives each incarnation a
/// distinct identity without changing a live row's update payload.
pub fn mint_recreate_identity(add_doc: &serde_json::Value) -> serde_json::Value {
    let mut doc = add_doc.clone();
    if let Some(map) = doc.as_object_mut() {
        map.insert(
            "updated_at".to_string(),
            serde_json::Value::String(mint_recreate_identity_timestamp()),
        );
    }
    doc
}

/// DefraDB cannot type an empty GraphQL list literal for nillable array
/// columns. Create inputs omit those fields; update inputs use `null` to clear
/// them. Every config writer that starts from JSON shares these transforms.
pub(crate) fn sanitize_create_input(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, value| {
            !value.is_null() && !matches!(value, Value::Array(items) if items.is_empty())
        });
    }
    value
}

pub(crate) fn sanitize_update_input(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        for value in object.values_mut() {
            if matches!(value, Value::Array(items) if items.is_empty()) {
                *value = Value::Null;
            }
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defra_json_inputs_never_emit_empty_array_literals() {
        let input = json!({ "keep": ["value"], "empty": [], "nil": null });
        assert_eq!(sanitize_create_input(&input), json!({ "keep": ["value"] }));
        assert_eq!(
            sanitize_update_input(&input),
            json!({ "keep": ["value"], "empty": null, "nil": null })
        );
    }

    #[test]
    fn recreate_identity_preserves_content_and_stamps_updated_at() {
        let original = json!({
            "selection_id": "tools-a",
            "created_at": "2026-01-01T00:00:00Z"
        });

        let minted = mint_recreate_identity(&original);

        assert_eq!(minted.get("selection_id"), original.get("selection_id"));
        assert_eq!(minted.get("created_at"), original.get("created_at"));
        assert!(minted.get("updated_at").and_then(Value::as_str).is_some());
        assert!(original.get("updated_at").is_none());
    }

    #[test]
    fn recreate_identity_timestamps_are_distinct_and_monotonic() {
        let first = mint_recreate_identity_timestamp();
        let second = mint_recreate_identity_timestamp();
        let first = chrono::DateTime::parse_from_rfc3339(&first).unwrap();
        let second = chrono::DateTime::parse_from_rfc3339(&second).unwrap();

        assert!(second > first);
    }

    #[test]
    fn multiple_tombstones_are_valid_recreate_history() {
        let rows = vec![
            ExistingDocumentRef {
                doc_id: "old-a".to_string(),
                deleted: true,
            },
            ExistingDocumentRef {
                doc_id: "old-b".to_string(),
                deleted: true,
            },
        ];

        let selected = select_existing_document("Task", "task_id", "task-a", &rows)
            .unwrap()
            .expect("any tombstone should select recreate");
        assert!(selected.deleted);
    }

    #[test]
    fn multiple_live_rows_remain_a_hard_error() {
        let rows = vec![
            ExistingDocumentRef {
                doc_id: "live-a".to_string(),
                deleted: false,
            },
            ExistingDocumentRef {
                doc_id: "live-b".to_string(),
                deleted: false,
            },
        ];

        let error = select_existing_document("Task", "task_id", "task-a", &rows).unwrap_err();
        assert!(error.to_string().contains("multiple live Task documents"));
    }
}
