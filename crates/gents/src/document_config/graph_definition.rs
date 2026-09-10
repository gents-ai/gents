use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};
use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphDefinition {
    pub graph_id: String,
    pub agent_did: String,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

/// Runtime-owned graph publication state, not part of authored pack configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphDefinitionObservation {
    pub graph_id: String,
    pub agent_did: String,
    pub active_revision_digest: Option<String>,
    pub generation: Option<i64>,
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
