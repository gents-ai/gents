use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigApplyTxn;
use gents::ToolSelectionDocument;
use gents_protocol::graphql::graphql_rows_from_response;
use gents_protocol::row::{ToolSelectionRow, ToolServiceRegistryRow};
use serde_json::Value;

use super::super::graphql::{
    escape_graphql_string, execute_mutation, graphql_optional_bool_field,
    graphql_optional_int_field, graphql_string_field, graphql_string_list_field, join_fields,
    normalize_required,
};

pub async fn upsert_tool_selection(node: &EmbeddedNode, row: &ToolSelectionRow) -> Result<()> {
    let mutation = build_upsert_tool_selection_mutation(row)?;
    let mut candidate = tool_selection_document(row)?;
    let txn = ConfigApplyTxn::begin_local(node, None).await?;
    let result = async {
        if let Some(existing) = preserved_tool_selection_fields(&txn, &row.selection_id).await? {
            // These columns are not represented by this desktop mutation, so
            // they remain stored state rather than being cleared by the save.
            candidate.read_only_command_allowlist = existing.read_only_command_allowlist;
            candidate.approval_required_tools = existing.approval_required_tools;
        }
        candidate.validate()?;
        txn.execute(&mutation).await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => txn.commit().await,
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn preserved_tool_selection_fields(
    txn: &ConfigApplyTxn<'_>,
    selection_id: &str,
) -> Result<Option<ToolSelectionDocument>> {
    let selection_id = escape_graphql_string(selection_id);
    let response = txn
        .execute(&format!(
            r#"{{ ToolSelection(
                filter: {{ selection_id: {{ _eq: "{selection_id}" }} }}, limit: 1
            ) {{ selection_id agent_did read_only_command_allowlist approval_required_tools }} }}"#
        ))
        .await?;
    graphql_rows_from_response(&response, "ToolSelection")
        .into_iter()
        .next()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding preserved ToolSelection fields")
}

fn build_upsert_tool_selection_mutation(row: &ToolSelectionRow) -> Result<String> {
    // Keep the desktop encoder: the canonical upsert writes columns this path
    // historically leaves alone and omits an absent cross-deployment timeout
    // where the desktop's full-row save explicitly clears it.
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
        Some(graphql_optional_bool_field(
            "enable_goal_tools",
            row.enable_goal_tools,
        )),
        Some(graphql_optional_bool_field(
            "enable_goal_creation",
            row.enable_goal_creation,
        )),
        Some(graphql_string_list_field(
            "allowed_mcp_service_ids",
            &row.allowed_mcp_service_ids,
        )),
        Some(graphql_string_list_field(
            "required_mcp_service_ids",
            &row.required_mcp_service_ids,
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
        Some(graphql_optional_bool_field(
            "enable_memory",
            row.enable_memory,
        )),
        Some(graphql_optional_bool_field(
            "enable_session_history_tool",
            row.enable_session_history_tool,
        )),
        Some(graphql_optional_bool_field(
            "enable_context_budget",
            row.enable_context_budget,
        )),
        Some(graphql_optional_bool_field(
            "enable_defra_query",
            row.enable_defra_query,
        )),
        Some(graphql_string_field(
            "tool_policy_version",
            row.tool_policy_version.as_deref(),
        )),
        Some(graphql_string_list_field(
            "defra_query_collections",
            &row.defra_query_collections,
        )),
        // Each entry is JSON-serialized WriteToolDecl; graphql_string_list_field
        // escapes every element and emits null (not []) when empty.
        Some(graphql_string_list_field("write_tools", &row.write_tools)),
        Some(graphql_string_list_field(
            "datastore_tool_surface_ids",
            &row.datastore_tool_surface_ids,
        )),
        Some(graphql_string_list_field("eth_tool_ids", &row.eth_tool_ids)),
        Some(graphql_optional_bool_field(
            "enable_self_config",
            row.enable_self_config,
        )),
        Some(graphql_string_list_field(
            "self_config_categories",
            &row.self_config_categories,
        )),
        Some(graphql_optional_bool_field(
            "self_config_no_lockout",
            row.self_config_no_lockout,
        )),
        Some(graphql_optional_bool_field(
            "self_config_dry_run",
            row.self_config_dry_run,
        )),
        Some(graphql_optional_bool_field("enable_lsp", row.enable_lsp)),
        Some(graphql_string_field(
            "lsp_config",
            row.lsp_config.as_deref(),
        )),
        Some(graphql_string_field(
            "subagent_default_await_mode",
            row.subagent_default_await_mode.as_deref(),
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
        Some(graphql_optional_bool_field(
            "enable_goal_tools",
            row.enable_goal_tools,
        )),
        Some(graphql_optional_bool_field(
            "enable_goal_creation",
            row.enable_goal_creation,
        )),
        Some(graphql_string_list_field(
            "allowed_mcp_service_ids",
            &row.allowed_mcp_service_ids,
        )),
        Some(graphql_string_list_field(
            "required_mcp_service_ids",
            &row.required_mcp_service_ids,
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
        Some(graphql_optional_bool_field(
            "enable_memory",
            row.enable_memory,
        )),
        Some(graphql_optional_bool_field(
            "enable_session_history_tool",
            row.enable_session_history_tool,
        )),
        Some(graphql_optional_bool_field(
            "enable_context_budget",
            row.enable_context_budget,
        )),
        Some(graphql_optional_bool_field(
            "enable_defra_query",
            row.enable_defra_query,
        )),
        Some(graphql_string_field(
            "tool_policy_version",
            row.tool_policy_version.as_deref(),
        )),
        Some(graphql_string_list_field(
            "defra_query_collections",
            &row.defra_query_collections,
        )),
        Some(graphql_string_list_field("write_tools", &row.write_tools)),
        Some(graphql_string_list_field(
            "datastore_tool_surface_ids",
            &row.datastore_tool_surface_ids,
        )),
        Some(graphql_string_list_field("eth_tool_ids", &row.eth_tool_ids)),
        Some(graphql_optional_bool_field(
            "enable_self_config",
            row.enable_self_config,
        )),
        Some(graphql_string_list_field(
            "self_config_categories",
            &row.self_config_categories,
        )),
        Some(graphql_optional_bool_field(
            "self_config_no_lockout",
            row.self_config_no_lockout,
        )),
        Some(graphql_optional_bool_field(
            "self_config_dry_run",
            row.self_config_dry_run,
        )),
        Some(graphql_optional_bool_field("enable_lsp", row.enable_lsp)),
        Some(graphql_string_field(
            "lsp_config",
            row.lsp_config.as_deref(),
        )),
        Some(graphql_string_field(
            "subagent_default_await_mode",
            row.subagent_default_await_mode.as_deref(),
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

fn tool_selection_document(row: &ToolSelectionRow) -> Result<ToolSelectionDocument> {
    serde_json::from_value(serde_json::to_value(row)?).map_err(Into::into)
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

pub async fn delete_tool_selection(
    node: &EmbeddedNode,
    agent_did: &str,
    selection_id: &str,
) -> Result<usize> {
    let mutation = build_delete_tool_selection_mutation(agent_did, selection_id)?;
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "delete_tool_selection failed: {}",
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
        .and_then(|data| data.get("delete_ToolSelection"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}

fn build_delete_tool_selection_mutation(agent_did: &str, selection_id: &str) -> Result<String> {
    let agent_did = normalize_required("agent_did", agent_did)?;
    let selection_id = normalize_required("selection_id", selection_id)?;
    let agent_did = escape_graphql_string(agent_did);
    let selection_id = escape_graphql_string(selection_id);
    Ok(format!(
        r#"mutation {{
            delete_ToolSelection(
                filter: {{
                    _and: [
                        {{ selection_id: {{ _eq: "{selection_id}" }} }},
                        {{ agent_did: {{ _eq: "{agent_did}" }} }}
                    ]
                }}
            ) {{ _docID }}
        }}"#
    ))
}

pub async fn delete_tool_service_registry(node: &EmbeddedNode, service_id: &str) -> Result<usize> {
    let mutation = build_delete_tool_service_registry_mutation(service_id)?;
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "delete_tool_service_registry failed: {}",
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
        .and_then(|data| data.get("delete_ToolServiceRegistry"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}

fn build_delete_tool_service_registry_mutation(service_id: &str) -> Result<String> {
    let service_id = normalize_required("service_id", service_id)?;
    let service_id = escape_graphql_string(service_id);
    Ok(format!(
        r#"mutation {{
            delete_ToolServiceRegistry(
                filter: {{ service_id: {{ _eq: "{service_id}" }} }}
            ) {{ _docID }}
        }}"#
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        build_delete_tool_selection_mutation, tool_selection_document, upsert_tool_selection,
    };
    use defra_node::{EmbeddedNode, StorageBackend};
    use gents::ensure_runtime_schemas;
    use gents_protocol::row::ToolSelectionRow;

    #[test]
    fn tool_selection_delete_is_scoped_to_agent_and_escapes_values() {
        let mutation = build_delete_tool_selection_mutation("did:test:remote", "tools-\"safe\"")
            .expect("delete mutation");

        assert!(mutation.contains(r#"agent_did: { _eq: "did:test:remote" }"#));
        assert!(mutation.contains(r#"selection_id: { _eq: "tools-\"safe\"" }"#));
    }

    #[test]
    fn upsert_rejects_an_invalid_subagent_target() {
        let row: ToolSelectionRow = serde_json::from_value(serde_json::json!({
            "selection_id": "tools",
            "agent_did": "did:test:amy",
            "subagent_targets": ["bare-behavior-id"]
        }))
        .expect("tool selection row");

        let error = tool_selection_document(&row)
            .expect("tool selection document")
            .validate()
            .expect_err("invalid tool selection")
            .to_string();
        assert!(error.contains("not a valid SubagentTarget JSON object"));
    }

    #[tokio::test]
    async fn upsert_validates_preserved_approval_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        let node = EmbeddedNode::builder()
            .data_path(dir.path().join("data"))
            .with_storage_backend(StorageBackend::Regolith)
            .build()
            .await
            .expect("node");
        ensure_runtime_schemas(&node).await.expect("schemas");
        let response = node
            .execute(
                r#"mutation { create_ToolSelection(input: {
                    selection_id: "desktop-tools", agent_did: "did:test:desktop",
                    display_name: "Original", approval_required_tools: [""]
                }) { _docID } }"#,
            )
            .await;
        assert!(!response.has_errors(), "seed: {:?}", response.errors);

        let row: ToolSelectionRow = serde_json::from_value(serde_json::json!({
            "selection_id": "desktop-tools",
            "agent_did": "did:test:desktop",
            "display_name": "Updated"
        }))
        .expect("tool selection row");
        let error = upsert_tool_selection(&node, &row)
            .await
            .expect_err("preserved invalid approval tool must reject the save")
            .to_string();
        assert!(
            error.contains("approval_required_tools[0] is empty"),
            "{error}"
        );
    }
}
