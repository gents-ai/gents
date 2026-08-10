//! Authoritative logical-to-physical AgentRequest binding lookup.
//!
//! `request_id` is a human-facing label; `_docID` is the provenance edge.
//! Every caller uses the same limit-two lookup so missing and ambiguous labels
//! cannot silently become half-bound writes.

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};

#[derive(Debug, Deserialize)]
struct RequestDocRow {
    #[serde(rename = "_docID")]
    doc_id: String,
}

pub(crate) async fn resolve_request_doc_id(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<String>> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 2
            ) {{ _docID }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "resolve_agent_request_document_binding")
            .await?;
    let mut matches = rows::<RequestDocRow>(&response, "AgentRequest")?;
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop().map(|row| row.doc_id)),
        count => anyhow::bail!(
            "AgentRequest request_id={request_id} is ambiguous across {count} documents"
        ),
    }
}

pub(crate) async fn require_request_doc_id(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<String> {
    resolve_request_doc_id(node, request_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("AgentRequest request_id={request_id} not found"))
}
