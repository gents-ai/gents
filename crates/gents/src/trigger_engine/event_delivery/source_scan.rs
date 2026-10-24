use anyhow::Result;
use defra_node::EmbeddedNode;
use serde_json::Value;

use super::super::TriggerEngineError;
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};

const SOURCE_SCAN_PAGE_SIZE: usize = 1_000;

pub(crate) async fn load_doc_ids(node: &EmbeddedNode, collection: &str) -> Result<Vec<String>> {
    load_source_rows(node, collection, None, None)
        .await?
        .into_iter()
        .map(|row| {
            document_id(&row)
                .map(str::to_owned)
                .map_err(anyhow::Error::from)
        })
        .collect()
}

pub(crate) async fn load_doc_field(
    node: &EmbeddedNode,
    collection: &str,
    field: &str,
) -> Result<Vec<(String, String)>> {
    let mut fields = Vec::new();
    for row in load_source_rows(node, collection, Some(field), None).await? {
        if let Some(value) = row.get(field).and_then(Value::as_str) {
            fields.push((document_id(&row)?.to_owned(), value.to_owned()));
        }
    }
    Ok(fields)
}

fn document_id(row: &Value) -> Result<&str, TriggerEngineError> {
    row.get("_docID")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or(TriggerEngineError::ScanDocumentIdMissing)
}

pub(crate) async fn load_source_rows(
    node: &EmbeddedNode,
    collection: &str,
    field: Option<&str>,
    filter: Option<&str>,
) -> Result<Vec<Value>> {
    crate::graphql::validate_collection_identifier(collection)?;
    if let Some(field) = field {
        crate::graphql::validate_graphql_name(field)?;
    }
    let base = filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .unwrap_or("{}");
    crate::graphql::validate_graphql_filter_fragment(base)?;
    let mut after = String::new();
    let mut documents = Vec::new();
    loop {
        let filter = if after.is_empty() {
            base.to_owned()
        } else {
            format!(
                "{{_and:[{base},{{_docID:{{_gt:\"{}\"}}}}]}}",
                escape_graphql_string(&after)
            )
        };
        let response = graphql_with_transaction_retry(
            node,
            &format!(
                "{{ {collection}(filter: {filter}, order: {{ _docID: ASC }}, limit: {SOURCE_SCAN_PAGE_SIZE}) {{ _docID {} }} }}",
                field.unwrap_or_default(),
            ),
            "event.source_scan",
        )
        .await?;
        let page = rows::<Value>(&response, collection)?;
        for row in &page {
            let id = document_id(row)?;
            anyhow::ensure!(
                id > after.as_str(),
                "event source scan did not advance its document cursor"
            );
            after = id.to_owned();
        }
        let complete = page.len() < SOURCE_SCAN_PAGE_SIZE;
        documents.extend(page);
        if complete {
            return Ok(documents);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config_client::ConfigAccess;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    pub(crate) async fn create_source_documents(node: &Arc<EmbeddedNode>) -> BTreeSet<String> {
        let access = ConfigAccess::local(node.clone());
        access
            .add_schema("type RescanSource { correlation: String }")
            .await
            .unwrap();
        let mut inserted = BTreeSet::new();
        for first in (0..10_001).step_by(250) {
            let ids = access
                .transact_write("test.source_scan.fixture", |txn| {
                    Box::pin(async move {
                        let mut ids = Vec::new();
                        for index in first..(first + 250).min(10_001) {
                            let response = txn
                                .execute(&format!(
                                    "mutation {{ create_RescanSource(input: {{ correlation: \"{}\" }}) {{ _docID }} }}",
                                    escape_graphql_string(&format!("group-{index}"))
                                ))
                                .await?;
                            ids.push(crate::graphql::created_doc_id(&response, "RescanSource")?);
                        }
                        Ok(ids)
                    })
                })
                .await
                .unwrap();
            inserted.extend(ids);
        }
        inserted
    }

    #[tokio::test]
    async fn source_scans_reach_documents_beyond_the_former_cap() {
        let directory = tempfile::tempdir().unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(directory.path().join("db"))
                .build()
                .await
                .unwrap(),
        );
        let inserted = create_source_documents(&node).await;
        assert_eq!(inserted.len(), 10_001);
        let ids = load_doc_ids(&node, "RescanSource").await.unwrap();
        assert_eq!(ids.len(), inserted.len());
        assert_eq!(ids.into_iter().collect::<BTreeSet<_>>(), inserted);
        let fields = load_doc_field(&node, "RescanSource", "correlation")
            .await
            .unwrap();
        assert_eq!(fields.len(), inserted.len());
        assert_eq!(
            fields
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<BTreeSet<_>>(),
            inserted
        );
        assert_eq!(
            fields
                .into_iter()
                .map(|(_, field)| field)
                .collect::<BTreeSet<_>>(),
            (0..10_001).map(|index| format!("group-{index}")).collect()
        );
        node.shutdown().await;
    }
}
