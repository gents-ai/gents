//! `Skill` mutations for the desktop client.
//!
//! A `Skill` is an apply-owned document (decision D1): a reusable instruction
//! + tool-dependency fragment owned by a principal (`agent_did`). Every field
//! is operator-authored — the runtime never writes skills back — so this
//! writer projects all of them. `tool_refs` is a `[String!]` and MUST render
//! an empty list as `null`, never `[]` (AGENTS.md sharp edge: a bare `[]`
//! literal types as `JsonArray` and corrupts the nillable array column); the
//! shared `graphql_string_list_field` helper enforces that.
//!
//! Mirrors the simpler `upsert_*` shape the other manage writers use; the
//! CLI's `commands/config/skill.rs` carries the import/export + SKILL.md
//! parsing surface, which the desktop intentionally does not duplicate.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use defra_node::EmbeddedNode;
use gents_protocol::row::SkillRow;
use serde_json::Value;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, graphql_optional_bool_field, graphql_string_field,
    graphql_string_list_field, join_fields, normalize_required,
};

pub async fn upsert_skill(node: &EmbeddedNode, row: &SkillRow) -> Result<()> {
    let skill_id = normalize_required("skill_id", &row.skill_id)?;
    let agent_did = normalize_required(
        "agent_did",
        row.agent_did
            .as_deref()
            .context("agent_did is required for Skill")?,
    )?;
    let created_at = row
        .created_at
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339());

    let add_fields = [
        Some(format!(
            r#"skill_id: "{}""#,
            escape_graphql_string(skill_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field("scope", row.scope.as_deref())),
        Some(graphql_string_field("name", row.name.as_deref())),
        Some(graphql_string_field(
            "description",
            row.description.as_deref(),
        )),
        Some(graphql_string_field(
            "instructions",
            row.instructions.as_deref(),
        )),
        Some(graphql_string_list_field("tool_refs", &row.tool_refs)),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "interface_json",
            row.interface_json.as_deref(),
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(&created_at)
        )),
    ];
    let update_fields = [
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field("scope", row.scope.as_deref())),
        Some(graphql_string_field("name", row.name.as_deref())),
        Some(graphql_string_field(
            "description",
            row.description.as_deref(),
        )),
        Some(graphql_string_field(
            "instructions",
            row.instructions.as_deref(),
        )),
        Some(graphql_string_list_field("tool_refs", &row.tool_refs)),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "interface_json",
            row.interface_json.as_deref(),
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_Skill(
                filter: {{ skill_id: {{ _eq: "{skill_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        skill_id = escape_graphql_string(skill_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_skill").await
}

pub async fn delete_skill(node: &EmbeddedNode, agent_did: &str, skill_id: &str) -> Result<usize> {
    let mutation = build_delete_skill_mutation(agent_did, skill_id)?;
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "delete_skill failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_Skill"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}

fn build_delete_skill_mutation(agent_did: &str, skill_id: &str) -> Result<String> {
    let agent_did = normalize_required("agent_did", agent_did)?;
    let skill_id = normalize_required("skill_id", skill_id)?;
    let agent_did = escape_graphql_string(agent_did);
    let skill_id = escape_graphql_string(skill_id);
    Ok(format!(
        r#"mutation {{
            delete_Skill(
                filter: {{
                    _and: [
                        {{ skill_id: {{ _eq: "{skill_id}" }} }},
                        {{ agent_did: {{ _eq: "{agent_did}" }} }}
                    ]
                }}
            ) {{ _docID }}
        }}"#
    ))
}
