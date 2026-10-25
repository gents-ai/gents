type Result<T, E = DocumentConfigError> = std::result::Result<T, E>;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

use super::error::DocumentConfigError;
use super::serde_helpers::deserialize_optional_string_vec;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EthToolDocument {
    pub tool_id: String,
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub chain_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub rpc_url: Option<String>,
    // Per-RPC HTTP timeout; the enclosing tool-call deadline can shorten it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub rpc_timeout_secs: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_vec")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub query_methods: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_string_vec")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub calls: Option<Vec<String>>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub key_binding_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl EthToolDocument {
    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        if self.tool_id.trim().is_empty() {
            return Err(DocumentConfigError::EthToolIdBlank);
        }
        if self.node_did.trim().is_empty() {
            return Err(DocumentConfigError::EthToolNodeBlank);
        }
        if let Some(chain_id) = self.chain_id {
            if chain_id <= 0 {
                return Err(DocumentConfigError::EthToolChainIdNotPositive);
            }
        }
        if let Some(rpc_url) = &self.rpc_url {
            if rpc_url.trim().is_empty() {
                return Err(DocumentConfigError::EthToolRpcUrlBlank);
            }
        }
        if let Some(binding_id) = &self.key_binding_id {
            if binding_id.trim().is_empty() {
                return Err(DocumentConfigError::EthToolKeyBindingBlank);
            }
        }
        crate::eth::HttpEthRpc::configured_timeout(self.rpc_timeout_secs)?;
        let methods =
            crate::eth::validate_query_methods(self.query_methods.as_deref().unwrap_or(&[]))?;
        let calls = self.calls.as_deref().unwrap_or(&[]);
        crate::eth::validate_eth_call_declarations(
            calls,
            self.key_binding_id.as_deref(),
            self.chain_id.map(|value| value as u64),
        )?;
        if (!methods.is_empty() || !calls.is_empty())
            && (self.rpc_url.is_none() || self.chain_id.is_none())
        {
            return Err(DocumentConfigError::EthToolAdvertisesWithoutRpc {
                tool_id: self.tool_id.clone(),
            });
        }
        Ok(())
    }
}

fn tool_fields() -> Result<String> {
    Ok(
        crate::config_client::config_projection(crate::Collection::EthTool, None)?
            .0
            .join(" "),
    )
}

pub async fn list_eth_tools(node: &EmbeddedNode, node_did: &str) -> Result<Vec<EthToolDocument>> {
    Ok(list_eth_tool_records(node, node_did)
        .await?
        .into_iter()
        .map(|(_, tool)| tool)
        .collect())
}

pub(crate) async fn list_eth_tool_records(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Vec<(String, EthToolDocument)>> {
    if node_did.trim().is_empty() {
        return Err(DocumentConfigError::EthToolOwnerRequired);
    }
    let owner = escape_graphql_string(node_did);
    let query = format!(
        "{{ EthTool(filter: {{node_did: {{_eq: \"{owner}\"}}}}) {{_docID {}}} }}",
        tool_fields()?
    );
    let response = graphql_with_transaction_retry(node, &query, "list EthTool").await?;
    super::serde_helpers::try_rows_with_doc_id(response.data.as_ref(), "EthTool")
}

pub fn eth_tool_by_id_query(node_did: &str, tool_id: &str) -> Result<String> {
    if node_did.trim().is_empty() || tool_id.trim().is_empty() {
        return Err(DocumentConfigError::EthToolOwnerAndIdRequired);
    }
    let owner = escape_graphql_string(node_did);
    let id = escape_graphql_string(tool_id);
    Ok(format!(
        "{{ EthTool(filter: {{node_did: {{_eq: \"{owner}\"}}, tool_id: {{_eq: \"{id}\"}}}}, limit: 2) {{_docID {}}} }}",
        tool_fields()?
    ))
}

#[cfg(test)]
#[path = "eth_validation_tests.rs"]
mod validation_tests;
