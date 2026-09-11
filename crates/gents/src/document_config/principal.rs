use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::serde_helpers::{
    default_display_name_for_did, first_row_with_doc_id, normalize_optional_string,
};

/// DefraDB DID identity for the runtime principal. One active instance is an
/// operating convention; runtime enforcement is deferred to #1435. No host identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct AgentPrincipal {
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub default_behavior_id: Option<String>,
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
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
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

    crate::config_client::ConfigAccess::write_local(
        node,
        "document.upsert_agent_principal",
        &mutation,
    )
    .await?;
    Ok(())
}
