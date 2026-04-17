use anyhow::{Context, Result};
use defra_agent_protocol::row::ToolSelectionRow;
use defra_node::EmbeddedNode;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, graphql_optional_bool_field, graphql_string_field,
    graphql_string_list_field, join_fields, normalize_required,
};

pub async fn upsert_tool_selection(node: &EmbeddedNode, row: &ToolSelectionRow) -> Result<()> {
    let selection_id = normalize_required("selection_id", &row.selection_id)?;
    let agent_did = normalize_required(
        "agent_did",
        row.agent_did
            .as_deref()
            .context("agent_did is required for ToolSelection")?,
    )?;

    let add_fields = [
        Some(format!(
            r#"selection_id: "{}""#,
            escape_graphql_string(selection_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_optional_bool_field(
            "enable_file_tools",
            row.enable_file_tools,
        )),
        Some(graphql_string_field(
            "file_tools_mode",
            row.file_tools_mode.as_deref(),
        )),
        Some(graphql_optional_bool_field("enable_bash", row.enable_bash)),
        Some(graphql_string_field("bash_mode", row.bash_mode.as_deref())),
        Some(graphql_string_list_field(
            "cli_tool_names",
            &row.cli_tool_names,
        )),
        Some(graphql_optional_bool_field(
            "enable_meta_tools",
            row.enable_meta_tools,
        )),
        Some(graphql_string_list_field("delegate_to", &row.delegate_to)),
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
        Some(graphql_optional_bool_field(
            "enable_file_tools",
            row.enable_file_tools,
        )),
        Some(graphql_string_field(
            "file_tools_mode",
            row.file_tools_mode.as_deref(),
        )),
        Some(graphql_optional_bool_field("enable_bash", row.enable_bash)),
        Some(graphql_string_field("bash_mode", row.bash_mode.as_deref())),
        Some(graphql_string_list_field(
            "cli_tool_names",
            &row.cli_tool_names,
        )),
        Some(graphql_optional_bool_field(
            "enable_meta_tools",
            row.enable_meta_tools,
        )),
        Some(graphql_string_list_field("delegate_to", &row.delegate_to)),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_ToolSelection(
                filter: {{ selection_id: {{ _eq: "{selection_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        selection_id = escape_graphql_string(selection_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_tool_selection").await
}
