use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::references::ConfigReferences;
use super::serde_helpers::{
    default_enabled, deserialize_default_on_null, deserialize_enabled, first_row_with_doc_id,
    is_enabled, rows_with_doc_id,
};

/// Selects context and inference as one unit. Behaviors are reusable
/// interfaces; the session binds to one by `behavior_id` only. There are no
/// backend/model/compaction/skill copies here — `AgentContext` owns literal
/// instructions, explicit skill selection, tools, and compaction, and
/// `InferenceProfile` is the only model-selection path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct AgentBehavior {
    /// Logical configuration key; `_docID` is the storage identity.
    pub behavior_id: String,
    pub agent_did: String,
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
    /// Task -> AgentBehavior -> InferenceProfile. There is no default
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

impl AgentBehavior {
    pub fn reference_violations(&self, refs: &ConfigReferences) -> Vec<String> {
        self.validate_references(refs)
            .err()
            .map(|error| vec![error.to_string()])
            .unwrap_or_default()
    }

    pub fn validate_references(&self, refs: &ConfigReferences) -> Result<()> {
        refs.validate_document(
            crate::Collection::AgentBehavior,
            &serde_json::to_value(self)?,
        )?;
        refs.validate()
    }
}

pub async fn load_agent_behavior(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<Option<AgentBehavior>> {
    Ok(load_agent_behavior_record(node, behavior_id)
        .await?
        .map(|(_, behavior)| behavior))
}

pub(crate) async fn load_agent_behavior_record(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<Option<(String, AgentBehavior)>> {
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{escaped_behavior_id}" }} }},
                limit: 1
            ) {{
                _docID
                behavior_id
                agent_did
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

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query AgentBehavior failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "AgentBehavior"))
}

pub async fn list_agent_behaviors(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<AgentBehavior>> {
    Ok(list_agent_behavior_records(node, agent_did)
        .await?
        .into_iter()
        .map(|(_, behavior)| behavior)
        .collect())
}

pub(crate) async fn list_agent_behavior_records(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<(String, AgentBehavior)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                behavior_id
                agent_did
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

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("list AgentBehavior failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "AgentBehavior"))
}

pub async fn upsert_agent_behavior(node: &EmbeddedNode, behavior: &AgentBehavior) -> Result<()> {
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "document_config.agent_behavior.upsert",
        |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(behavior)?;
                let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
                    crate::config_client::DesiredStateApplyDocument {
                        collection: crate::Collection::AgentBehavior,
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
