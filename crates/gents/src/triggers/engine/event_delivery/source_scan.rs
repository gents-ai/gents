use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde_json::Value;

use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};

const SOURCE_SCAN_PAGE_SIZE: usize = 1_000;

pub(crate) async fn load_doc_ids(node: &EmbeddedNode, collection: &str) -> Result<Vec<String>> {
    load_source_rows(node, collection, None, None)
        .await?
        .into_iter()
        .map(|row| document_id(&row).map(str::to_owned))
        .collect()
}

fn document_id(row: &Value) -> Result<&str> {
    row.get("_docID")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .context("event source scan omitted a document ID")
}

pub(crate) async fn load_source_rows(
    node: &EmbeddedNode,
    collection: &str,
    field: Option<&str>,
    filter: Option<&str>,
) -> Result<Vec<Value>> {
    crate::store::graphql::validate_collection_identifier(collection)?;
    if let Some(field) = field {
        crate::store::graphql::validate_graphql_name(field)?;
    }
    let base = filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .unwrap_or("{}");
    crate::store::graphql::validate_graphql_filter_fragment(base)?;
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
    use crate::store::config_client::ConfigAccess;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    pub(crate) async fn create_source_documents(node: &Arc<EmbeddedNode>) -> BTreeSet<String> {
        let access = ConfigAccess::local(node.clone());
        access
            .add_collection("type RescanSource { correlation: String }")
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
                            ids.push(crate::store::graphql::created_doc_id(&response, "RescanSource")?);
                        }
                        Ok::<_, anyhow::Error>(ids)
                    })
                })
                .await
                .unwrap();
            inserted.extend(ids);
        }
        inserted
    }
}
