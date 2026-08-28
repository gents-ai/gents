//! Apply-owned `ChainKeyBinding` documents. Key material is not stored here.
#![allow(dead_code)]

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};

/// Document-layer view of a `ChainKeyBinding` row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChainKeyBindingDocument {
    pub binding_id: String,
    pub principal_did: String,
    pub address: String,
    pub key_backend: Option<String>,
    pub attestation: Option<String>,
    pub created_at: Option<String>,
    pub revoked_at: Option<String>,
}

const BINDING_FIELDS: &str = r#"
                _docID
                binding_id
                principal_did
                address
                key_backend
                attestation
                created_at
                revoked_at
"#;

pub(crate) async fn list_chain_key_binding_records(
    node: &EmbeddedNode,
    principal_did: &str,
) -> Result<Vec<(String, ChainKeyBindingDocument)>> {
    let escaped_did = escape_graphql_string(principal_did);
    let query = format!(
        r#"{{
            ChainKeyBinding(
                filter: {{ principal_did: {{ _eq: "{escaped_did}" }} }}
            ) {{{BINDING_FIELDS}}}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("list ChainKeyBinding failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "ChainKeyBinding"))
}

pub(crate) async fn load_chain_key_binding_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, ChainKeyBindingDocument)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            ChainKeyBinding(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{{BINDING_FIELDS}}}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query ChainKeyBinding by _docID failed: {:?}", resp.errors);
    }
    Ok(first_row_with_doc_id(resp.data.as_ref(), "ChainKeyBinding"))
}
