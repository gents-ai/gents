use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::serde_helpers::{
    default_display_name_for_did, first_row_with_doc_id, normalize_optional_string,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPrincipal {
    pub agent_did: String,
    pub display_name: Option<String>,
    pub default_behavior_id: Option<String>,
    pub enabled: bool,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
}

pub async fn load_agent_principal(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Option<AgentPrincipal>> {
    Ok(load_agent_principal_record(node, agent_did)
        .await?
        .map(|(_, principal)| principal))
}

pub(crate) async fn load_agent_principal_record(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Option<(String, AgentPrincipal)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                limit: 1
            ) {{
                _docID
                agent_did
                display_name
                default_behavior_id
                enabled
                created_at
                created_by
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query AgentPrincipal failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "AgentPrincipal"))
}

pub(crate) async fn load_agent_principal_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, AgentPrincipal)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentPrincipal(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                agent_did
                display_name
                default_behavior_id
                enabled
                created_at
                created_by
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query AgentPrincipal by _docID failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "AgentPrincipal"))
}

pub async fn upsert_agent_principal(
    node: &EmbeddedNode,
    agent_did: &str,
    display_name: Option<&str>,
    default_behavior_id: Option<&str>,
    enabled: bool,
) -> Result<()> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let fallback_display_name = default_display_name_for_did(agent_did);
    let display_name =
        normalize_optional_string(display_name).unwrap_or(fallback_display_name.as_str());
    let escaped_display_name = escape_graphql_string(display_name);
    let escaped_default_behavior_id =
        escape_graphql_string(normalize_optional_string(default_behavior_id).unwrap_or_default());
    let escaped_created_by = escape_graphql_string(agent_did);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            upsert_AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                add: {{
                    agent_did: "{escaped_agent_did}",
                    display_name: "{escaped_display_name}",
                    default_behavior_id: "{escaped_default_behavior_id}",
                    enabled: {enabled},
                    created_at: "{created_at}",
                    created_by: "{escaped_created_by}"
                }},
                update: {{
                    display_name: "{escaped_display_name}",
                    default_behavior_id: "{escaped_default_behavior_id}",
                    enabled: {enabled}
                }}
            ) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("upsert AgentPrincipal failed: {:?}", resp.errors);
    }
    Ok(())
}
