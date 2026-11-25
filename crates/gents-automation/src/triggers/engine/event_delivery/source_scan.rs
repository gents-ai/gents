use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use serde_json::Value;
use std::sync::Arc;

use gents_store::graphql::{cursor_after_arg, cursor_page_data};

const SOURCE_SCAN_PAGE_SIZE: usize = 1_000;

pub(crate) async fn load_doc_ids(
    node: &Arc<EmbeddedNode>,
    collection: &str,
    actor: &str,
) -> Result<Vec<String>> {
    load_source_rows(node, collection, None, None, actor)
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
    node: &Arc<EmbeddedNode>,
    collection: &str,
    field: Option<&str>,
    filter: Option<&str>,
    actor: &str,
) -> Result<Vec<Value>> {
    gents_store::graphql::validate_collection_identifier(collection)?;
    if let Some(field) = field {
        gents_store::graphql::validate_graphql_name(field)?;
    }
    let base = filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .unwrap_or("{}");
    gents_store::graphql::validate_graphql_filter_fragment(base)?;
    let mut cursor: Option<String> = None;
    let mut last_id = String::new();
    let mut documents = Vec::new();
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(actor)?,
    );
    loop {
        let after = cursor_after_arg(cursor.as_deref());
        let response = access.execute(
            &format!(
                "{{ _cursor {{ {collection}(first: {SOURCE_SCAN_PAGE_SIZE}{after}, filter: {base}, order: [{{ _docID: ASC }}]) {{ _docID {} }} _pageInfo {{ hasNext endCursor }} }} }}",
                field.unwrap_or_default(),
            ),
        )
        .await?;
        let (page, next) = cursor_page_data::<Value>(response.get("data"), collection)
            .context("event source scan omitted its result")?;
        for row in &page {
            let id = document_id(row)?;
            anyhow::ensure!(
                id > last_id.as_str(),
                "event source scan did not advance its document cursor"
            );
            last_id = id.to_owned();
        }
        documents.extend(page);
        match next {
            None => return Ok(documents),
            Some(next) => cursor = Some(next),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use gents_store::config_client::ConfigAccess;
    use gents_store::graphql::escape_graphql_string;
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
                            ids.push(gents_store::graphql::created_doc_id(&response, "RescanSource")?);
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
