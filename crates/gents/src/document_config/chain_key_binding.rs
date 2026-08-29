//! Apply-owned `ChainKeyBinding` documents. Key material is not stored here.

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::{escape_graphql_string, graphql_mutation_with_transaction_retry};

use super::graphql_fields;

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

pub async fn list_chain_key_binding_records(
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

pub async fn load_chain_key_binding_by_doc_id(
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

pub(crate) async fn load_chain_key_binding(
    node: &EmbeddedNode,
    binding_id: &str,
) -> Result<Option<ChainKeyBindingDocument>> {
    let escaped = escape_graphql_string(binding_id);
    let query = format!(
        r#"{{
            ChainKeyBinding(filter: {{ binding_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{{BINDING_FIELDS}}}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query ChainKeyBinding by binding_id failed: {:?}",
            response.errors
        );
    }
    Ok(
        first_row_with_doc_id(response.data.as_ref(), "ChainKeyBinding")
            .map(|(_, binding)| binding),
    )
}

pub fn list_chain_key_bindings_query(principal_did: &str) -> String {
    let escaped_did = escape_graphql_string(principal_did);
    format!(
        r#"{{
            ChainKeyBinding(
                filter: {{ principal_did: {{ _eq: "{escaped_did}" }} }}
            ) {{{BINDING_FIELDS}}}
        }}"#
    )
}

pub fn chain_key_binding_by_id_query(binding_id: &str) -> String {
    let escaped = escape_graphql_string(binding_id);
    format!(
        r#"{{
            ChainKeyBinding(filter: {{ binding_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{{BINDING_FIELDS}}}
        }}"#
    )
}

pub fn upsert_chain_key_binding_mutation(doc: &ChainKeyBindingDocument) -> String {
    let escaped_binding_id = escape_graphql_string(&doc.binding_id);
    let escaped_principal = escape_graphql_string(&doc.principal_did);
    let escaped_address = escape_graphql_string(&doc.address);
    let backend = graphql_fields::graphql_string_field("key_backend", doc.key_backend.as_deref())
        .unwrap_or_else(|| r#"key_backend: """#.to_string());
    let attestation =
        graphql_fields::graphql_string_field("attestation", doc.attestation.as_deref())
            .unwrap_or_else(|| r#"attestation: """#.to_string());
    let created_at = graphql_fields::graphql_string_field("created_at", doc.created_at.as_deref())
        .unwrap_or_else(|| r#"created_at: """#.to_string());
    let revoked_add =
        graphql_fields::graphql_nullable_string_field("revoked_at", doc.revoked_at.as_deref());
    let revoked_update =
        graphql_fields::graphql_string_field("revoked_at", doc.revoked_at.as_deref())
            .map(|field| format!("{field},"))
            .unwrap_or_default();
    format!(
        r#"mutation {{
            upsert_ChainKeyBinding(
                filter: {{ binding_id: {{ _eq: "{escaped_binding_id}" }} }},
                add: {{
                    binding_id: "{escaped_binding_id}",
                    principal_did: "{escaped_principal}",
                    address: "{escaped_address}",
                    {backend},
                    {attestation},
                    {created_at},
                    {revoked_add}
                }},
                update: {{
                    principal_did: "{escaped_principal}",
                    address: "{escaped_address}",
                    {backend},
                    {attestation},
                    {revoked_update}
                }}
            ) {{ _docID }}
        }}"#
    )
}

pub fn create_chain_key_binding_mutation(doc: &ChainKeyBindingDocument) -> String {
    let escaped_binding_id = escape_graphql_string(&doc.binding_id);
    let escaped_principal = escape_graphql_string(&doc.principal_did);
    let escaped_address = escape_graphql_string(&doc.address);
    let backend = graphql_fields::graphql_string_field("key_backend", doc.key_backend.as_deref())
        .unwrap_or_else(|| r#"key_backend: """#.to_string());
    let attestation =
        graphql_fields::graphql_string_field("attestation", doc.attestation.as_deref())
            .unwrap_or_else(|| r#"attestation: """#.to_string());
    let created_at = graphql_fields::graphql_string_field("created_at", doc.created_at.as_deref())
        .unwrap_or_else(|| r#"created_at: """#.to_string());
    format!(
        r#"mutation {{
            create_ChainKeyBinding(input: {{
                binding_id: "{escaped_binding_id}",
                principal_did: "{escaped_principal}",
                address: "{escaped_address}",
                {backend},
                {attestation},
                {created_at},
                revoked_at: null
            }}) {{ _docID }}
        }}"#
    )
}

pub fn delete_chain_key_binding_mutation(binding_id: &str) -> String {
    let escaped_binding_id = escape_graphql_string(binding_id);
    format!(
        r#"mutation {{
            delete_ChainKeyBinding(filter: {{ binding_id: {{ _eq: "{escaped_binding_id}" }} }}) {{ _docID }}
        }}"#
    )
}

pub async fn upsert_chain_key_binding(
    node: &EmbeddedNode,
    doc: &ChainKeyBindingDocument,
) -> Result<()> {
    let mutation = upsert_chain_key_binding_mutation(doc);
    graphql_mutation_with_transaction_retry(node, &mutation, "upsert ChainKeyBinding").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(revoked_at: Option<&str>) -> ChainKeyBindingDocument {
        ChainKeyBindingDocument {
            binding_id: "bind-1".to_string(),
            principal_did: "did:key:zAlice".to_string(),
            address: "0x1111111111111111111111111111111111111111".to_string(),
            key_backend: Some("keyring".to_string()),
            attestation: Some("0xsig".to_string()),
            created_at: Some("2026-08-28T00:00:00Z".to_string()),
            revoked_at: revoked_at.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn active_upsert_never_clears_a_live_revocation() {
        let mutation = upsert_chain_key_binding_mutation(&binding(None));
        assert_eq!(mutation.matches("revoked_at: null").count(), 1);
    }

    #[test]
    fn revoked_upsert_writes_the_tombstone_on_add_and_update() {
        let mutation = upsert_chain_key_binding_mutation(&binding(Some("t1")));
        assert_eq!(mutation.matches("revoked_at: \"t1\"").count(), 2);
    }
}
