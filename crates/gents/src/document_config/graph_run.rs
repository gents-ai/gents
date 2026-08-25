use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};
use crate::graphql::escape_graphql_string;

/// Minimal GraphRun projection needed by runtime artifact reconciliation.
/// Execution/result observation owns the full run projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphRunPin {
    pub(crate) run_id: String,
    pub(crate) revision_digest: String,
    pub(crate) owner_did: String,
    pub(crate) status: String,
}

impl GraphRunPin {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "succeeded" | "failed" | "cancelled")
    }
}

pub(crate) async fn list_graph_run_pin_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, GraphRunPin)>> {
    let response = node
        .execute(
            r#"{
                GraphRun(order: { created_at: ASC }) {
                    _docID run_id revision_digest owner_did status
                }
            }"#,
        )
        .await;
    if response.has_errors() {
        anyhow::bail!("list GraphRun pins failed: {:?}", response.errors);
    }
    Ok(rows_with_doc_id(response.data.as_ref(), "GraphRun"))
}

pub(crate) async fn load_graph_run_pin_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, GraphRunPin)>> {
    let query = format!(
        r#"{{
            GraphRun(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                _docID run_id revision_digest owner_did status
            }}
        }}"#,
        escape_graphql_string(doc_id),
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("query GraphRun by _docID failed: {:?}", response.errors);
    }
    Ok(first_row_with_doc_id(response.data.as_ref(), "GraphRun"))
}
