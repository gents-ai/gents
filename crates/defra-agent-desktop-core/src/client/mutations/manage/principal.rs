use anyhow::Result;
use chrono::Utc;
use defra_agent_protocol::row::AgentPrincipalRow;
use defra_node::EmbeddedNode;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, graphql_optional_bool_field, graphql_string_field,
    join_fields, normalize_required,
};

pub async fn upsert_agent_principal(node: &EmbeddedNode, row: &AgentPrincipalRow) -> Result<()> {
    let agent_did = normalize_required("agent_did", &row.agent_did)?;
    let created_at = row
        .created_at
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let created_by = row.created_by.as_deref().unwrap_or(agent_did);

    let add_fields = [
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "default_behavior_id",
            row.default_behavior_id.as_deref(),
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(&created_at)
        )),
        Some(format!(
            r#"created_by: "{}""#,
            escape_graphql_string(created_by)
        )),
    ];
    let update_fields = [
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "default_behavior_id",
            row.default_behavior_id.as_deref(),
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_agent_principal").await
}
