use anyhow::Result;
use defra_node::Node as EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};

use super::serde_helpers::first_row_with_doc_id;

/// DefraDB DID identity for the runtime node. One active instance is an
/// operating convention; runtime enforcement is deferred to #1435. No host identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Node {
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    /// The desktop's portrait for the node, by number. Cosmetic; a node
    /// without one is drawn with the portrait its name picks.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub default_agent_did: Option<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
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
    #[schemars(transform = crate::documents::accepts_null)]
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
                avatar
                default_agent_did
                enabled
                created_at
                created_by
                max_request_hop
                tags
            }}
        }}"#
    );

    let resp = graphql_with_transaction_retry(node, &query, "query Node").await?;

    Ok(first_row_with_doc_id(resp.data.as_ref(), "Node"))
}
