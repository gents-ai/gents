use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};
use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphDefinition {
    pub(crate) graph_id: String,
    pub(crate) owner_did: String,
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) active_revision_digest: Option<String>,
    #[serde(default)]
    pub(crate) generation: Option<i64>,
    pub(crate) created_at: Option<String>,
    pub(crate) updated_at: Option<String>,
}

pub(crate) async fn list_graph_definition_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, GraphDefinition)>> {
    let response = node
        .execute(
            r#"{
                GraphDefinition(order: { graph_id: ASC }) {
                    _docID graph_id owner_did enabled active_revision_digest generation created_at updated_at
                }
            }"#,
        )
        .await;
    if response.has_errors() {
        anyhow::bail!("list GraphDefinition failed: {:?}", response.errors);
    }
    Ok(rows_with_doc_id(response.data.as_ref(), "GraphDefinition"))
}

pub(crate) async fn load_graph_definition_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, GraphDefinition)>> {
    let query = format!(
        r#"{{
            GraphDefinition(
                filter: {{ _docID: {{ _eq: "{}" }} }},
                limit: 1
            ) {{
                _docID graph_id owner_did enabled active_revision_digest generation created_at updated_at
            }}
        }}"#,
        escape_graphql_string(doc_id)
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query GraphDefinition by _docID failed: {:?}",
            response.errors
        );
    }
    Ok(first_row_with_doc_id(
        response.data.as_ref(),
        "GraphDefinition",
    ))
}
