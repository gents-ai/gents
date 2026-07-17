use anyhow::{bail, Context, Result};
use chrono::Utc;
use defra_agent_protocol::row::AgentBehaviorRow;
use defra_node::EmbeddedNode;
use serde_json::Value;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, execute_remote_mutation, graphql_optional_bool_field,
    graphql_optional_float_field, graphql_string_field, graphql_string_list_field, join_fields,
    normalize_required,
};

pub async fn upsert_agent_behavior(node: &EmbeddedNode, row: &AgentBehaviorRow) -> Result<()> {
    let mutation = build_upsert_agent_behavior_mutation(row)?;
    execute_mutation(node, &mutation, "upsert_agent_behavior").await
}

pub async fn upsert_agent_behavior_to_graphql(graphql: &str, row: &AgentBehaviorRow) -> Result<()> {
    let graphql = normalize_required("graphql", graphql)?;
    let mutation = build_upsert_agent_behavior_mutation(row)?;
    execute_remote_mutation(graphql, &mutation, "upsert_agent_behavior").await
}

fn build_upsert_agent_behavior_mutation(row: &AgentBehaviorRow) -> Result<String> {
    let behavior_id = normalize_required("behavior_id", &row.behavior_id)?;
    let agent_did = normalize_required(
        "agent_did",
        row.agent_did
            .as_deref()
            .context("agent_did is required for AgentBehavior")?,
    )?;
    let created_at = row
        .created_at
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339());

    let add_fields = [
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(behavior_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "system_prompt",
            row.system_prompt.as_deref(),
        )),
        Some(graphql_string_field(
            "backend_id",
            row.backend_id.as_deref(),
        )),
        Some(graphql_string_field(
            "model_name",
            row.model_name.as_deref(),
        )),
        Some(graphql_string_field(
            "tool_selection_id",
            row.tool_selection_id.as_deref(),
        )),
        Some(graphql_string_field(
            "inference_profile_id",
            row.inference_profile_id.as_deref(),
        )),
        Some(graphql_string_field(
            "compaction_strategy",
            row.compaction_strategy.as_deref(),
        )),
        Some(graphql_optional_float_field(
            "compaction_threshold",
            row.compaction_threshold,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_list_field("skill_refs", &row.skill_refs)),
        Some(graphql_string_list_field(
            "skill_excludes",
            &row.skill_excludes,
        )),
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
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "system_prompt",
            row.system_prompt.as_deref(),
        )),
        Some(graphql_string_field(
            "backend_id",
            row.backend_id.as_deref(),
        )),
        Some(graphql_string_field(
            "model_name",
            row.model_name.as_deref(),
        )),
        Some(graphql_string_field(
            "tool_selection_id",
            row.tool_selection_id.as_deref(),
        )),
        Some(graphql_string_field(
            "inference_profile_id",
            row.inference_profile_id.as_deref(),
        )),
        Some(graphql_string_field(
            "compaction_strategy",
            row.compaction_strategy.as_deref(),
        )),
        Some(graphql_optional_float_field(
            "compaction_threshold",
            row.compaction_threshold,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_list_field("skill_refs", &row.skill_refs)),
        Some(graphql_string_list_field(
            "skill_excludes",
            &row.skill_excludes,
        )),
    ];

    Ok(format!(
        r#"mutation {{
            upsert_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        behavior_id = escape_graphql_string(behavior_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    ))
}

pub async fn delete_agent_behavior(node: &EmbeddedNode, behavior_id: &str) -> Result<usize> {
    let behavior_id = normalize_required("behavior_id", behavior_id)?;
    let behavior_id = escape_graphql_string(behavior_id);
    let mutation = format!(
        r#"mutation {{
            delete_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "delete_agent_behavior failed: {}",
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
        .and_then(|data| data.get("delete_AgentBehavior"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}
