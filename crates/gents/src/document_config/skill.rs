use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};

/// Document-layer view of a `Skill` row (decision D1). Mirrors
/// `crates/gents-protocol/schemas/agent/skill.graphql`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SkillDocument {
    pub skill_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface_json: Option<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

const SKILL_FIELDS: &str = r#"
                _docID
                skill_id
                agent_did
                scope
                name
                description
                instructions
                tool_refs
                display_name
                interface_json
                enabled
                created_at
"#;

/// List all `Skill` documents owned by `agent_did`. Returns `(doc_id, doc)`
/// pairs. Tolerates a missing `Skill` collection (older nodes) by surfacing the
/// query error to the caller, who treats absence as an empty set.
pub(crate) async fn list_skill_records(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<(String, SkillDocument)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            Skill(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}
            ) {{{SKILL_FIELDS}}}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("list Skill failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "Skill"))
}

/// Load a single `Skill` document by its DefraDB `_docID` (used by the control
/// watcher to hot-reload skill changes into the runtime snapshot).
pub(crate) async fn load_skill_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, SkillDocument)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            Skill(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{{SKILL_FIELDS}}}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query Skill by _docID failed: {:?}", resp.errors);
    }
    Ok(first_row_with_doc_id(resp.data.as_ref(), "Skill"))
}
