use anyhow::{Context, Result};
use defra_agent_protocol::row::{ToolSelectionRow, ToolServiceRegistryRow};
use defra_node::EmbeddedNode;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, execute_remote_mutation, graphql_optional_bool_field,
    graphql_optional_int_field, graphql_string_field, graphql_string_list_field, join_fields,
    normalize_required,
};

pub async fn upsert_tool_selection(node: &EmbeddedNode, row: &ToolSelectionRow) -> Result<()> {
    let mutation = build_upsert_tool_selection_mutation(row)?;
    execute_mutation(node, &mutation, "upsert_tool_selection").await
}

pub async fn upsert_tool_selection_to_graphql(graphql: &str, row: &ToolSelectionRow) -> Result<()> {
    let graphql = normalize_required("graphql", graphql)?;
    let mutation = build_upsert_tool_selection_mutation(row)?;
    execute_remote_mutation(graphql, &mutation, "upsert_tool_selection").await
}

fn build_upsert_tool_selection_mutation(row: &ToolSelectionRow) -> Result<String> {
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
        Some(graphql_string_field(
            "file_tool_root",
            row.file_tool_root.as_deref(),
        )),
        Some(graphql_optional_bool_field("enable_bash", row.enable_bash)),
        Some(graphql_string_field("bash_mode", row.bash_mode.as_deref())),
        Some(graphql_string_field(
            "command_execution_policy",
            row.command_execution_policy.as_deref(),
        )),
        Some(graphql_string_list_field(
            "command_allowed_argv_prefixes",
            &row.command_allowed_argv_prefixes,
        )),
        Some(graphql_string_list_field(
            "command_forbidden_argv_prefixes",
            &row.command_forbidden_argv_prefixes,
        )),
        Some(graphql_string_field(
            "command_network_mode",
            row.command_network_mode.as_deref(),
        )),
        Some(graphql_string_list_field(
            "cli_tool_names",
            &row.cli_tool_names,
        )),
        Some(graphql_optional_bool_field(
            "enable_meta_tools",
            row.enable_meta_tools,
        )),
        Some(graphql_string_list_field(
            "allowed_mcp_service_ids",
            &row.allowed_mcp_service_ids,
        )),
        Some(graphql_string_list_field(
            "backgroundable_tool_names",
            &row.backgroundable_tool_names,
        )),
        Some(graphql_string_list_field(
            "subagent_targets",
            &row.subagent_targets,
        )),
        Some(graphql_optional_bool_field(
            "subagent_spawn_enabled",
            row.subagent_spawn_enabled,
        )),
        Some(graphql_optional_bool_field(
            "subagent_steering_enabled",
            row.subagent_steering_enabled,
        )),
        Some(graphql_optional_bool_field(
            "subagent_background_enabled",
            row.subagent_background_enabled,
        )),
        Some(graphql_optional_bool_field(
            "subagent_allow_cross_deployment",
            row.subagent_allow_cross_deployment,
        )),
        Some(graphql_optional_int_field(
            "cross_deployment_spawn_timeout_seconds",
            row.cross_deployment_spawn_timeout_seconds,
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
        Some(graphql_optional_bool_field(
            "enable_file_tools",
            row.enable_file_tools,
        )),
        Some(graphql_string_field(
            "file_tools_mode",
            row.file_tools_mode.as_deref(),
        )),
        Some(graphql_string_field(
            "file_tool_root",
            row.file_tool_root.as_deref(),
        )),
        Some(graphql_optional_bool_field("enable_bash", row.enable_bash)),
        Some(graphql_string_field("bash_mode", row.bash_mode.as_deref())),
        Some(graphql_string_field(
            "command_execution_policy",
            row.command_execution_policy.as_deref(),
        )),
        Some(graphql_string_list_field(
            "command_allowed_argv_prefixes",
            &row.command_allowed_argv_prefixes,
        )),
        Some(graphql_string_list_field(
            "command_forbidden_argv_prefixes",
            &row.command_forbidden_argv_prefixes,
        )),
        Some(graphql_string_field(
            "command_network_mode",
            row.command_network_mode.as_deref(),
        )),
        Some(graphql_string_list_field(
            "cli_tool_names",
            &row.cli_tool_names,
        )),
        Some(graphql_optional_bool_field(
            "enable_meta_tools",
            row.enable_meta_tools,
        )),
        Some(graphql_string_list_field(
            "allowed_mcp_service_ids",
            &row.allowed_mcp_service_ids,
        )),
        Some(graphql_string_list_field(
            "backgroundable_tool_names",
            &row.backgroundable_tool_names,
        )),
        Some(graphql_string_list_field(
            "subagent_targets",
            &row.subagent_targets,
        )),
        Some(graphql_optional_bool_field(
            "subagent_spawn_enabled",
            row.subagent_spawn_enabled,
        )),
        Some(graphql_optional_bool_field(
            "subagent_steering_enabled",
            row.subagent_steering_enabled,
        )),
        Some(graphql_optional_bool_field(
            "subagent_background_enabled",
            row.subagent_background_enabled,
        )),
        Some(graphql_optional_bool_field(
            "subagent_allow_cross_deployment",
            row.subagent_allow_cross_deployment,
        )),
        Some(graphql_optional_int_field(
            "cross_deployment_spawn_timeout_seconds",
            row.cross_deployment_spawn_timeout_seconds,
        )),
    ];

    Ok(format!(
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
    ))
}

pub async fn upsert_tool_service_registry(
    node: &EmbeddedNode,
    row: &ToolServiceRegistryRow,
) -> Result<()> {
    let service_id = normalize_required("service_id", &row.service_id)?;
    let status = row
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("online");

    let add_fields = [
        Some(format!(
            r#"service_id: "{}""#,
            escape_graphql_string(service_id)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "description",
            row.description.as_deref(),
        )),
        Some(graphql_string_field("hostname", row.hostname.as_deref())),
        Some(graphql_string_field(
            "tailscale_ip",
            row.tailscale_ip.as_deref(),
        )),
        Some(graphql_string_field("lan_ip", row.lan_ip.as_deref())),
        Some(graphql_optional_int_field("mcp_port", row.mcp_port)),
        Some(graphql_string_field("mcp_path", row.mcp_path.as_deref())),
        Some(format!(r#"status: "{}""#, escape_graphql_string(status))),
    ];
    let update_fields = [
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "description",
            row.description.as_deref(),
        )),
        Some(graphql_string_field("hostname", row.hostname.as_deref())),
        Some(graphql_string_field(
            "tailscale_ip",
            row.tailscale_ip.as_deref(),
        )),
        Some(graphql_string_field("lan_ip", row.lan_ip.as_deref())),
        Some(graphql_optional_int_field("mcp_port", row.mcp_port)),
        Some(graphql_string_field("mcp_path", row.mcp_path.as_deref())),
        Some(format!(r#"status: "{}""#, escape_graphql_string(status))),
    ];

    let mutation = format!(
        r#"mutation {{
            upsert_ToolServiceRegistry(
                filter: {{ service_id: {{ _eq: "{service_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        service_id = escape_graphql_string(service_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    );
    execute_mutation(node, &mutation, "upsert_tool_service_registry").await
}
