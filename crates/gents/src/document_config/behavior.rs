use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

use super::references::ConfigReferences;
use super::serde_helpers::{
    default_enabled, deserialize_default_on_null, deserialize_enabled, first_row_with_doc_id,
    is_enabled, rows_with_doc_id,
};

/// Selects context and inference as one unit. Agents are reusable
/// interfaces; the session binds to one by `agent_id` only. There are no
/// backend/model/compaction/skill copies here — `AgentContext` owns literal
/// instructions, explicit skill selection, tools, and compaction, and
/// `InferenceProfile` is the only model-selection path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Agent {
    /// Logical configuration key; `_docID` is the storage identity.
    pub agent_id: String,
    pub node_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    /// Absent context means no system instructions, skills, or tools, with
    /// runtime-default compaction (StripThenSummarize, threshold 0.75).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub context_id: Option<String>,
    /// Required: the only model selection path is
    /// Task -> Agent -> InferenceProfile. There is no default
    /// fallback.
    pub inference_profile_id: String,
    #[serde(
        default = "default_enabled",
        deserialize_with = "deserialize_enabled",
        skip_serializing_if = "is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub enabled: bool,
    /// Optional UI/discovery labels. References, never tags, determine
    /// execution.
    #[serde(
        default,
        deserialize_with = "deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
}

impl Agent {
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

pub async fn load_agent(node: &EmbeddedNode, agent_id: &str) -> Result<Option<Agent>> {
    Ok(load_agent_record(node, agent_id)
        .await?
        .map(|(_, behavior)| behavior))
}

pub(crate) async fn load_agent_record(
    node: &EmbeddedNode,
    agent_id: &str,
) -> Result<Option<(String, Agent)>> {
    let escaped_agent_id = escape_graphql_string(agent_id);
    let query = format!(
        r#"{{
            Agent(
                filter: {{ agent_id: {{ _eq: "{escaped_agent_id}" }} }},
                limit: 1
            ) {{
                _docID
                agent_id
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
                agent_id
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

pub async fn upsert_agent(node: &EmbeddedNode, behavior: &Agent) -> Result<()> {
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "document_config.agent.upsert",
        |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(behavior)?;
                let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
                    crate::config_client::DesiredStateApplyDocument {
                        collection: crate::Collection::Agent,
                        add: value.clone(),
                        update: value,
                    },
                ])?;
                crate::config_client::apply_desired_state_plan(txn, &plan).await?;
                Ok(())
            })
        },
    )
    .await
}
