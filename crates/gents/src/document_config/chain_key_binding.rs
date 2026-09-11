//! Apply-owned `ChainKeyBinding` documents. Key material is not stored here.

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

/// Document-layer view of a `ChainKeyBinding` row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ChainKeyBindingDocument {
    pub binding_id: String,
    pub agent_did: String,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub key_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub attestation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub revoked_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl ChainKeyBindingDocument {
    /// Intrinsic authoring checks. The signing owner verifies the DID
    /// attestation, revocation and actual key material at execution time.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.binding_id.trim().is_empty(),
            "ChainKeyBinding requires binding_id"
        );
        anyhow::ensure!(
            !self.agent_did.trim().is_empty(),
            "ChainKeyBinding requires agent_did"
        );
        let address = self.address.trim();
        anyhow::ensure!(
            address.len() == 42
                && address.starts_with("0x")
                && address[2..].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "ChainKeyBinding has an invalid Ethereum address"
        );
        anyhow::ensure!(
            self.key_backend.as_deref() == Some(crate::eth::KEY_BACKEND_KEYRING),
            "ChainKeyBinding has unsupported key_backend"
        );
        anyhow::ensure!(
            self.attestation
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            "ChainKeyBinding has no principal attestation"
        );
        anyhow::ensure!(
            self.created_at
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            "ChainKeyBinding has no created_at timestamp"
        );
        Ok(())
    }
}

fn binding_fields() -> Result<String> {
    Ok(
        crate::config_client::config_projection(crate::Collection::ChainKeyBinding, None)?
            .0
            .join(" "),
    )
}

pub fn list_chain_key_bindings_query(agent_did: &str) -> Result<String> {
    anyhow::ensure!(!agent_did.trim().is_empty(), "chain key owner is required");
    let owner = escape_graphql_string(agent_did);
    Ok(format!(
        "{{ ChainKeyBinding(filter: {{agent_did: {{_eq: \"{owner}\"}}}}) {{ _docID {} }} }}",
        binding_fields()?
    ))
}

pub fn chain_key_binding_by_id_query(agent_did: &str, binding_id: &str) -> Result<String> {
    anyhow::ensure!(
        !agent_did.trim().is_empty() && !binding_id.trim().is_empty(),
        "chain key owner and binding ID are required"
    );
    let owner = escape_graphql_string(agent_did);
    let binding = escape_graphql_string(binding_id);
    Ok(format!(
        "{{ ChainKeyBinding(filter: {{agent_did: {{_eq: \"{owner}\"}}, binding_id: {{_eq: \"{binding}\"}}}}, limit: 2) {{ _docID {} }} }}",
        binding_fields()?
    ))
}

pub async fn list_chain_key_binding_records(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<(String, ChainKeyBindingDocument)>> {
    let response = node
        .execute(&list_chain_key_bindings_query(agent_did)?)
        .await;
    anyhow::ensure!(
        !response.has_errors(),
        "list ChainKeyBinding failed: {:?}",
        response.errors
    );
    super::serde_helpers::try_rows_with_doc_id(response.data.as_ref(), "ChainKeyBinding")
}

pub async fn load_chain_key_binding_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, ChainKeyBindingDocument)>> {
    let id = escape_graphql_string(doc_id);
    let query = format!(
        "{{ ChainKeyBinding(filter: {{_docID: {{_eq: \"{id}\"}}}}) {{ _docID {} }} }}",
        binding_fields()?
    );
    let response = node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "read ChainKeyBinding failed: {:?}",
        response.errors
    );
    let mut rows =
        super::serde_helpers::try_rows_with_doc_id(response.data.as_ref(), "ChainKeyBinding")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "duplicate ChainKeyBinding physical identity"
    );
    Ok(rows.pop())
}

/// Shared update policy for the attested creation identity and revocation
/// tombstone. An omitted, null, or blank proposal cannot reactivate a binding.
/// Explicit nonblank revocations retain the existing update behavior.
pub fn preserve_chain_key_binding_update_fields(update: &mut serde_json::Value) -> Result<()> {
    let fields = update
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("chain key binding update must be an object"))?;
    fields.remove("created_at");
    if fields.get("revoked_at").is_none_or(|value| {
        value.is_null() || value.as_str().is_some_and(|value| value.trim().is_empty())
    }) {
        fields.remove("revoked_at");
    }
    Ok(())
}

pub fn upsert_chain_key_binding_mutation(doc: &ChainKeyBindingDocument) -> Result<String> {
    let owner = escape_graphql_string(&doc.agent_did);
    let binding = escape_graphql_string(&doc.binding_id);
    anyhow::ensure!(
        !doc.agent_did.trim().is_empty() && !doc.binding_id.trim().is_empty(),
        "chain key owner and binding ID are required"
    );
    let (_, add) = crate::config_client::config_projection(
        crate::Collection::ChainKeyBinding,
        Some(&serde_json::to_value(doc)?),
    )?;
    let add = add.expect("value projection");
    let mut update = add.clone();
    preserve_chain_key_binding_update_fields(&mut update)?;
    Ok(format!(
        "mutation {{ upsert_ChainKeyBinding(filter: {{agent_did: {{_eq: \"{owner}\"}}, binding_id: {{_eq: \"{binding}\"}}}}, add: {}, update: {}) {{ _docID }} }}",
        gents_protocol::graphql::graphql_input_literal(&add)?,
        gents_protocol::graphql::graphql_input_literal(&update)?
    ))
}

pub fn create_chain_key_binding_mutation(doc: &ChainKeyBindingDocument) -> Result<String> {
    anyhow::ensure!(
        !doc.agent_did.trim().is_empty() && !doc.binding_id.trim().is_empty(),
        "chain key owner and binding ID are required"
    );
    let (_, value) = crate::config_client::config_projection(
        crate::Collection::ChainKeyBinding,
        Some(&serde_json::to_value(doc)?),
    )?;
    Ok(format!(
        "mutation {{ create_ChainKeyBinding(input: {}) {{ _docID }} }}",
        gents_protocol::graphql::graphql_input_literal(&value.expect("value projection"))?
    ))
}

/// Remove only the exact newly created, still-active row after keyring failure.
/// A concurrent revocation is a tombstone, never incomplete-create cleanup.
pub fn delete_chain_key_binding_mutation(doc_id: &str) -> String {
    let id = escape_graphql_string(doc_id);
    format!(
        "mutation {{ delete_ChainKeyBinding(filter: {{_docID: {{_eq: \"{id}\"}}, revoked_at: {{_eq: null}}}}) {{ _docID }} }}"
    )
}

pub async fn upsert_chain_key_binding(
    node: &EmbeddedNode,
    doc: &ChainKeyBindingDocument,
) -> Result<()> {
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "document.upsert_chain_key_binding",
        |txn| {
            Box::pin(async move {
                // Exact scoped read rejects duplicate IDs before a mutation can touch them.
                crate::config_client::read_desired_state_document_in_txn(
                    txn,
                    crate::Collection::ChainKeyBinding,
                    &doc.agent_did,
                    &doc.binding_id,
                )
                .await?;
                txn.execute(&upsert_chain_key_binding_mutation(doc)?)
                    .await?;
                Ok(())
            })
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding(revoked_at: Option<&str>) -> ChainKeyBindingDocument {
        ChainKeyBindingDocument {
            binding_id: "bind-1".into(),
            agent_did: "did:key:zAlice".into(),
            address: "0x1111111111111111111111111111111111111111".into(),
            key_backend: Some("keyring".into()),
            attestation: Some("0xsig".into()),
            created_at: Some("2026-08-28T00:00:00Z".into()),
            revoked_at: revoked_at.map(str::to_string),
            tags: vec!["treasury".into()],
        }
    }
    #[test]
    fn active_upsert_never_clears_a_live_revocation() {
        let mutation = upsert_chain_key_binding_mutation(&binding(None)).unwrap();
        assert_eq!(mutation.matches("revoked_at: null").count(), 1);
        assert!(!mutation.contains("principal_did"));
        assert!(mutation.contains("tags: [\"treasury\"]"));
    }
    #[test]
    fn revoked_upsert_writes_the_tombstone_on_add_and_update() {
        let mutation = upsert_chain_key_binding_mutation(&binding(Some("t1"))).unwrap();
        assert_eq!(mutation.matches("revoked_at: \"t1\"").count(), 2);
    }
}
