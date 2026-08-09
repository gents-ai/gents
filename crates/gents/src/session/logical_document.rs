/// A complete logical-key lookup resolved to more than one physical DefraDB
/// document.  Callers can downcast `anyhow::Error` to this type when they need
/// to distinguish a replicated/concurrent twin from an ordinary query error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{collection} logical identity conflict for {logical_field}={logical_value}: _docIDs={document_ids:?}"
)]
pub struct LogicalIdConflict {
    pub collection: &'static str,
    pub logical_field: &'static str,
    pub logical_value: String,
    pub document_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LogicalDocumentResolutionError {
    #[error("{collection} logical match for {logical_field}={logical_value} has no _docID")]
    MissingDocumentId {
        collection: &'static str,
        logical_field: &'static str,
        logical_value: String,
    },
    #[error(transparent)]
    Conflict(#[from] LogicalIdConflict),
}

/// Resolve a *complete* logical-key result set to one physical document.
///
/// This is reader hardening, not a lifecycle transition: it does not choose a
/// winner or change which state transitions are legal.  It only prevents scan
/// order (or `limit: 1`) from becoming authority when replication or an older
/// unindexed collection exposes logical twins.
pub fn resolve_exact_logical_match<T>(
    collection: &'static str,
    logical_field: &'static str,
    logical_value: &str,
    rows: Vec<T>,
    doc_id: impl Fn(&T) -> &str,
) -> Result<Option<T>, LogicalDocumentResolutionError> {
    let mut document_ids = rows
        .iter()
        .map(|row| doc_id(row).trim().to_string())
        .collect::<Vec<_>>();
    if document_ids.iter().any(String::is_empty) {
        return Err(LogicalDocumentResolutionError::MissingDocumentId {
            collection,
            logical_field,
            logical_value: logical_value.to_string(),
        });
    }
    if rows.len() > 1 {
        document_ids.sort();
        return Err(LogicalIdConflict {
            collection,
            logical_field,
            logical_value: logical_value.to_string(),
            document_ids,
        }
        .into());
    }
    Ok(rows.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_is_typed_and_order_independent() {
        let error = resolve_exact_logical_match(
            "Example",
            "logical_id",
            "same",
            vec!["doc-z".to_string(), "doc-a".to_string()],
            String::as_str,
        )
        .unwrap_err();
        assert_eq!(
            error,
            LogicalDocumentResolutionError::Conflict(LogicalIdConflict {
                collection: "Example",
                logical_field: "logical_id",
                logical_value: "same".to_string(),
                document_ids: vec!["doc-a".to_string(), "doc-z".to_string()],
            })
        );
    }
}
