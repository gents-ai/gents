//! Apply-owned `EthTool` documents.
#![allow(dead_code)]

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::serde_helpers::{
    deserialize_optional_string_vec, first_row_with_doc_id, rows_with_doc_id,
};

/// Document-layer view of an `EthTool` row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EthToolDocument {
    pub tool_id: String,
    pub agent_did: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    pub chain_id: Option<i64>,
    pub rpc_url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_vec")]
    pub query_methods: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_string_vec")]
    pub calls: Option<Vec<String>>,
    #[serde(default)]
    pub key_binding_id: Option<String>,
    pub created_at: Option<String>,
}

const TOOL_FIELDS: &str = r#"
                _docID
                tool_id
                agent_did
                display_name
                enabled
                chain_id
                rpc_url
                query_methods
                calls
                key_binding_id
                created_at
"#;

pub(crate) async fn list_eth_tool_records(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<(String, EthToolDocument)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            EthTool(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}
            ) {{{TOOL_FIELDS}}}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("list EthTool failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "EthTool"))
}

pub(crate) async fn load_eth_tool_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, EthToolDocument)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            EthTool(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{{TOOL_FIELDS}}}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query EthTool by _docID failed: {:?}", resp.errors);
    }
    Ok(first_row_with_doc_id(resp.data.as_ref(), "EthTool"))
}
