//! Apply-owned `EthTool` documents.
use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::serde_helpers::deserialize_optional_string_vec;

/// Document-layer view of an `EthTool` row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct EthToolDocument {
    pub tool_id: String,
    pub agent_did: String,
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
    /// Per-RPC HTTP timeout; unset retains the current 30s default.
    /// The enclosing tool-call deadline can shorten it.
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
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl EthToolDocument {
    /// Validate authored declarations even when the document is disabled.
    /// An empty surface needs no connection; advertised tools do.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.tool_id.trim().is_empty(), "EthTool requires tool_id");
        anyhow::ensure!(
            !self.agent_did.trim().is_empty(),
            "EthTool requires agent_did"
        );
        if let Some(chain_id) = self.chain_id {
            anyhow::ensure!(chain_id > 0, "EthTool chain_id must be positive");
        }
        if let Some(rpc_url) = &self.rpc_url {
            anyhow::ensure!(
                !rpc_url.trim().is_empty(),
                "EthTool rpc_url must not be blank"
            );
        }
        if let Some(binding_id) = &self.key_binding_id {
            anyhow::ensure!(
                !binding_id.trim().is_empty(),
                "EthTool key_binding_id must not be blank"
            );
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
        if !methods.is_empty() || !calls.is_empty() {
            anyhow::ensure!(
                self.rpc_url.is_some() && self.chain_id.is_some(),
                "EthTool {} advertises tools but has no rpc_url or chain_id",
                self.tool_id
            );
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

pub async fn list_eth_tools(node: &EmbeddedNode, agent_did: &str) -> Result<Vec<EthToolDocument>> {
    Ok(list_eth_tool_records(node, agent_did)
        .await?
        .into_iter()
        .map(|(_, tool)| tool)
        .collect())
}

pub(crate) async fn list_eth_tool_records(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<(String, EthToolDocument)>> {
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "Ethereum tool owner is required"
    );
    let owner = escape_graphql_string(agent_did);
    let query = format!(
        "{{ EthTool(filter: {{agent_did: {{_eq: \"{owner}\"}}}}) {{_docID {}}} }}",
        tool_fields()?
    );
    let response = node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "list EthTool failed: {:?}",
        response.errors
    );
    super::serde_helpers::try_rows_with_doc_id(response.data.as_ref(), "EthTool")
}

pub fn eth_tool_by_id_query(agent_did: &str, tool_id: &str) -> Result<String> {
    anyhow::ensure!(
        !agent_did.trim().is_empty() && !tool_id.trim().is_empty(),
        "Ethereum owner and tool ID are required"
    );
    let owner = escape_graphql_string(agent_did);
    let id = escape_graphql_string(tool_id);
    Ok(format!("{{ EthTool(filter: {{agent_did: {{_eq: \"{owner}\"}}, tool_id: {{_eq: \"{id}\"}}}}, limit: 2) {{_docID {}}} }}", tool_fields()?))
}

#[cfg(test)]
#[path = "eth_validation_tests.rs"]
mod validation_tests;
