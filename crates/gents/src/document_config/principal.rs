use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

use super::error::DocumentConfigError;
use super::serde_helpers::first_row_with_doc_id;

type Result<T, E = DocumentConfigError> = std::result::Result<T, E>;

/// DefraDB DID identity for the runtime node. One active instance is an
/// operating convention; runtime enforcement is deferred to #1435. No host identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Node {
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub default_agent_id: Option<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_by: Option<String>,
    /// Optional admission bound on a request's causal hop; absent disables the cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_request_hop: Option<u32>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

pub async fn load_node(node: &EmbeddedNode, node_did: &str) -> Result<Option<Node>> {
    Ok(load_node_record(node, node_did)
        .await?
        .map(|(_, principal)| principal))
}

pub(crate) async fn load_node_record(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Option<(String, Node)>> {
    let escaped_node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            Node(
                filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }},
                limit: 1
            ) {{
                _docID
                node_did
                display_name
                default_agent_id
                enabled
                created_at
                created_by
                max_request_hop
            }}
        }}"#
    );

    let resp = graphql_with_transaction_retry(node, &query, "query Node").await?;

    Ok(first_row_with_doc_id(resp.data.as_ref(), "Node"))
}

/// Raw fixture writer for tests that stage a principal before its agents.
/// Production principals publish through the desired-state plan, which
/// validates the default behavior with the documents it names.
#[cfg(test)]
pub(crate) async fn upsert_node(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    display_name: Option<&str>,
    default_agent_id: Option<&str>,
    enabled: bool,
    max_request_hop: Option<u32>,
) -> anyhow::Result<()> {
    use super::serde_helpers::{default_display_name_for_did, normalize_optional_string};
    let escaped_node_did = escape_graphql_string(node_did);
    let fallback_display_name = default_display_name_for_did(node_did);
    let display_name =
        normalize_optional_string(display_name).unwrap_or(fallback_display_name.as_str());
    let escaped_display_name = escape_graphql_string(display_name);
    let default_agent_id = normalize_optional_string(default_agent_id)
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .unwrap_or_else(|| "null".to_string());
    let max_request_hop = max_request_hop.map_or_else(|| "null".to_string(), |hop| hop.to_string());
    let escaped_created_by = escape_graphql_string(node_did);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            upsert_Node(
                filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }},
                add: {{
                    node_did: "{escaped_node_did}",
                    display_name: "{escaped_display_name}",
                    default_agent_id: {default_agent_id},
                    enabled: {enabled},
                    max_request_hop: {max_request_hop},
                    created_at: "{created_at}",
                    created_by: "{escaped_created_by}"
                }},
                update: {{
                    display_name: "{escaped_display_name}",
                    default_agent_id: {default_agent_id},
                    enabled: {enabled},
                    max_request_hop: {max_request_hop}
                }}
            ) {{ _docID }}
        }}"#
    );

    crate::config_client::ConfigAccess::write_local(node, "document.upsert_node", &mutation)
        .await?;
    Ok(())
}
