use anyhow::Result;
use defra_node::Node as EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};

use super::references::ConfigReferences;
use super::serde_helpers::{
    default_enabled, deserialize_default_on_null, deserialize_enabled, first_row_with_doc_id,
    is_enabled, rows_with_doc_id,
};

mod persistence;

pub use persistence::write_agent_document;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Agent {
    pub agent_did: String,
    pub creation_nonce: String,
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub context_id: Option<String>,
    pub inference_profile_id: String,
    #[serde(
        default = "default_enabled",
        deserialize_with = "deserialize_enabled",
        skip_serializing_if = "is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub enabled: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    #[schemars(transform = crate::documents::accepts_null)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
}

impl Agent {
    pub fn identity_binding(&self) -> gents_protocol::agent_identity::AgentIdentityBinding {
        gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: self.agent_did.clone(),
            creation_nonce: self.creation_nonce.clone(),
        }
    }

    pub fn reference_violations(&self, refs: &ConfigReferences) -> Vec<String> {
        self.validate_references(refs)
            .err()
            .map(|error| vec![error.to_string()])
            .unwrap_or_default()
    }

    pub fn validate_references(&self, refs: &ConfigReferences) -> Result<()> {
        refs.validate_document(crate::Collection::Agent, &serde_json::to_value(self)?)?;
        refs.validate()
    }
}

pub async fn load_agent(node: &EmbeddedNode, agent_did: &str) -> Result<Option<Agent>> {
    Ok(load_agent_record(node, agent_did)
        .await?
        .map(|(_, behavior)| behavior))
}

pub(crate) async fn load_agent_record(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Option<(String, Agent)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            Agent(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                limit: 1
            ) {{
                _docID
                agent_did
                creation_nonce
                node_did
                display_name
                description
                context_id
                inference_profile_id
                enabled
                tags
                created_at
            }}
        }}"#
    );

    let resp = graphql_with_transaction_retry(node, &query, "query Agent").await?;

    Ok(first_row_with_doc_id(resp.data.as_ref(), "Agent"))
}

pub async fn list_agents(node: &EmbeddedNode, node_did: &str) -> Result<Vec<Agent>> {
    Ok(list_agent_records(node, node_did)
        .await?
        .into_iter()
        .map(|(_, behavior)| behavior)
        .collect())
}

pub(crate) async fn list_agent_records(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Vec<(String, Agent)>> {
    let escaped_node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            Agent(
                filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                agent_did
                creation_nonce
                node_did
                display_name
                description
                context_id
                inference_profile_id
                enabled
                tags
                created_at
            }}
        }}"#
    );

    let resp = graphql_with_transaction_retry(node, &query, "list Agent").await?;

    Ok(rows_with_doc_id(resp.data.as_ref(), "Agent"))
}

