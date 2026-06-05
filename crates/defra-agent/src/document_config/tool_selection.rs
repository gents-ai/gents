use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::graphql_fields;
use super::serde_helpers;
use crate::document_config::SubagentTarget;
use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ToolSelectionDocument {
    #[serde(default)]
    pub selection_id: String,
    #[serde(default)]
    pub agent_did: String,
    pub display_name: Option<String>,
    pub enable_file_tools: Option<bool>,
    pub file_tools_mode: Option<String>,
    pub file_tool_root: Option<String>,
    pub enable_bash: Option<bool>,
    pub bash_mode: Option<String>,
    pub command_execution_policy: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    pub command_allowed_argv_prefixes: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    pub command_forbidden_argv_prefixes: Option<Vec<String>>,
    pub command_network_mode: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    pub cli_tool_names: Option<Vec<String>>,
    pub enable_meta_tools: Option<bool>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    pub allowed_mcp_service_ids: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    pub backgroundable_tool_names: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    pub subagent_targets: Option<Vec<String>>,
    pub subagent_spawn_enabled: Option<bool>,
    pub subagent_steering_enabled: Option<bool>,
    pub subagent_background_enabled: Option<bool>,
    pub subagent_allow_cross_deployment: Option<bool>,
    pub cross_deployment_spawn_timeout_seconds: Option<i64>,
    pub enable_memory: Option<bool>,
    pub enable_defra_query: Option<bool>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    pub defra_query_collections: Option<Vec<String>>,
}

impl ToolSelectionDocument {
    pub fn validate(&self) -> Result<()> {
        if let Some(targets) = &self.subagent_targets {
            for (i, target) in targets.iter().enumerate() {
                if target.is_empty() {
                    return Err(anyhow::anyhow!(
                        "subagent_targets[{}] is empty; each entry must be a valid SubagentTarget JSON object",
                        i
                    ));
                }
                // Every non-empty entry must be parseable as a SubagentTarget JSON
                // object AND pass structural validation (all fields non-empty).
                // Bare behavior-id strings are not valid — the runtime silently
                // drops them, which entrenches a silent misconfiguration. Reject
                // them here with a clear diagnostic.
                let parsed = SubagentTarget::parse(target).map_err(|e| {
                    anyhow::anyhow!(
                        "subagent_targets[{i}] is not a valid SubagentTarget JSON object \
                         (got {target:?}): {e}; \
                         use subagent_target_entry(name, agent_did, behavior_id, description) \
                         to build a valid entry"
                    )
                })?;
                if !parsed.is_structurally_valid() {
                    return Err(anyhow::anyhow!(
                        "subagent_targets[{i}] parsed as SubagentTarget but is not structurally \
                         valid (name, agent_did, and behavior_id must all be non-empty): {target:?}"
                    ));
                }
            }
        }
        if let Some(tool_names) = &self.backgroundable_tool_names {
            for (i, tool_name) in tool_names.iter().enumerate() {
                if tool_name.is_empty() {
                    return Err(anyhow::anyhow!(
                        "backgroundable_tool_names[{}] is empty; tool names must be non-empty strings",
                        i
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn default_tool_selection_id_for_behavior(behavior_id: &str) -> String {
    format!("{behavior_id}-tools")
}

pub async fn load_tool_selection(
    node: &EmbeddedNode,
    selection_id: &str,
) -> Result<Option<ToolSelectionDocument>> {
    Ok(load_tool_selection_record(node, selection_id)
        .await?
        .map(|(_, selection)| selection))
}

pub(crate) async fn load_tool_selection_record(
    node: &EmbeddedNode,
    selection_id: &str,
) -> Result<Option<(String, ToolSelectionDocument)>> {
    let escaped_selection_id = escape_graphql_string(selection_id);
    let query = format!(
        r#"{{
            ToolSelection(
                filter: {{ selection_id: {{ _eq: "{escaped_selection_id}" }} }},
                limit: 1
            ) {{
                _docID
                selection_id
                agent_did
                display_name
                enable_file_tools
                file_tools_mode
                file_tool_root
                enable_bash
                bash_mode
                command_execution_policy
                command_allowed_argv_prefixes
                command_forbidden_argv_prefixes
                command_network_mode
                cli_tool_names
                enable_meta_tools
                allowed_mcp_service_ids
                backgroundable_tool_names
                subagent_targets
                subagent_spawn_enabled
                subagent_steering_enabled
                subagent_background_enabled
                subagent_allow_cross_deployment
                cross_deployment_spawn_timeout_seconds
                enable_memory
                enable_defra_query
                defra_query_collections
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query ToolSelection failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::first_row_with_doc_id(
        resp.data.as_ref(),
        "ToolSelection",
    ))
}

pub(crate) async fn load_tool_selection_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, ToolSelectionDocument)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            ToolSelection(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                selection_id
                agent_did
                display_name
                enable_file_tools
                file_tools_mode
                file_tool_root
                enable_bash
                bash_mode
                command_execution_policy
                command_allowed_argv_prefixes
                command_forbidden_argv_prefixes
                command_network_mode
                cli_tool_names
                enable_meta_tools
                allowed_mcp_service_ids
                backgroundable_tool_names
                subagent_targets
                subagent_spawn_enabled
                subagent_steering_enabled
                subagent_background_enabled
                subagent_allow_cross_deployment
                cross_deployment_spawn_timeout_seconds
                enable_memory
                enable_defra_query
                defra_query_collections
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query ToolSelection by _docID failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::first_row_with_doc_id(
        resp.data.as_ref(),
        "ToolSelection",
    ))
}

pub(crate) async fn list_tool_selection_records(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<(String, ToolSelectionDocument)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            ToolSelection(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                order: {{ selection_id: ASC }}
            ) {{
                _docID
                selection_id
                agent_did
                display_name
                enable_file_tools
                file_tools_mode
                file_tool_root
                enable_bash
                bash_mode
                command_execution_policy
                command_allowed_argv_prefixes
                command_forbidden_argv_prefixes
                command_network_mode
                cli_tool_names
                enable_meta_tools
                allowed_mcp_service_ids
                backgroundable_tool_names
                subagent_targets
                subagent_spawn_enabled
                subagent_steering_enabled
                subagent_background_enabled
                subagent_allow_cross_deployment
                cross_deployment_spawn_timeout_seconds
                enable_memory
                enable_defra_query
                defra_query_collections
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("list ToolSelection failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::rows_with_doc_id(
        resp.data.as_ref(),
        "ToolSelection",
    ))
}

pub(crate) async fn list_all_tool_selection_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, ToolSelectionDocument)>> {
    let query = r#"{
            ToolSelection(order: { selection_id: ASC }) {
                _docID
                selection_id
                agent_did
                display_name
                enable_file_tools
                file_tools_mode
                file_tool_root
                enable_bash
                bash_mode
                command_execution_policy
                command_allowed_argv_prefixes
                command_forbidden_argv_prefixes
                command_network_mode
                cli_tool_names
                enable_meta_tools
                allowed_mcp_service_ids
                backgroundable_tool_names
                subagent_targets
                subagent_spawn_enabled
                subagent_steering_enabled
                subagent_background_enabled
                subagent_allow_cross_deployment
                cross_deployment_spawn_timeout_seconds
                enable_memory
                enable_defra_query
                defra_query_collections
            }
        }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list all ToolSelection failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::rows_with_doc_id(
        resp.data.as_ref(),
        "ToolSelection",
    ))
}

pub async fn upsert_tool_selection(
    node: &EmbeddedNode,
    selection: &ToolSelectionDocument,
) -> Result<()> {
    let escaped_selection_id = escape_graphql_string(&selection.selection_id);
    let escaped_agent_did = escape_graphql_string(&selection.agent_did);

    let add_fields = vec![
        Some(format!(r#"selection_id: "{escaped_selection_id}""#)),
        Some(format!(r#"agent_did: "{escaped_agent_did}""#)),
        graphql_fields::graphql_string_field("display_name", selection.display_name.as_deref()),
        graphql_fields::graphql_optional_bool_field(
            "enable_file_tools",
            selection.enable_file_tools,
        ),
        graphql_fields::graphql_string_field(
            "file_tools_mode",
            selection.file_tools_mode.as_deref(),
        ),
        Some(graphql_fields::graphql_nullable_string_field(
            "file_tool_root",
            selection.file_tool_root.as_deref(),
        )),
        graphql_fields::graphql_optional_bool_field("enable_bash", selection.enable_bash),
        graphql_fields::graphql_string_field("bash_mode", selection.bash_mode.as_deref()),
        graphql_fields::graphql_string_field(
            "command_execution_policy",
            selection.command_execution_policy.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "command_allowed_argv_prefixes",
            selection.command_allowed_argv_prefixes.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "command_forbidden_argv_prefixes",
            selection.command_forbidden_argv_prefixes.as_deref(),
        ),
        graphql_fields::graphql_string_field(
            "command_network_mode",
            selection.command_network_mode.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "cli_tool_names",
            selection.cli_tool_names.as_deref(),
        ),
        graphql_fields::graphql_optional_bool_field(
            "enable_meta_tools",
            selection.enable_meta_tools,
        ),
        graphql_fields::graphql_string_list_field(
            "allowed_mcp_service_ids",
            selection.allowed_mcp_service_ids.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "backgroundable_tool_names",
            selection.backgroundable_tool_names.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "subagent_targets",
            selection.subagent_targets.as_deref(),
        ),
        graphql_fields::graphql_optional_bool_field(
            "subagent_spawn_enabled",
            selection.subagent_spawn_enabled,
        ),
        graphql_fields::graphql_optional_bool_field(
            "subagent_steering_enabled",
            selection.subagent_steering_enabled,
        ),
        graphql_fields::graphql_optional_bool_field(
            "subagent_background_enabled",
            selection.subagent_background_enabled,
        ),
        graphql_fields::graphql_optional_bool_field(
            "subagent_allow_cross_deployment",
            selection.subagent_allow_cross_deployment,
        ),
        selection
            .cross_deployment_spawn_timeout_seconds
            .map(|value| format!("cross_deployment_spawn_timeout_seconds: {value}")),
        graphql_fields::graphql_optional_bool_field("enable_memory", selection.enable_memory),
        graphql_fields::graphql_optional_bool_field(
            "enable_defra_query",
            selection.enable_defra_query,
        ),
        graphql_fields::graphql_string_list_field(
            "defra_query_collections",
            selection.defra_query_collections.as_deref(),
        ),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        Some(format!(r#"agent_did: "{escaped_agent_did}""#)),
        graphql_fields::graphql_string_field("display_name", selection.display_name.as_deref()),
        graphql_fields::graphql_optional_bool_field(
            "enable_file_tools",
            selection.enable_file_tools,
        ),
        graphql_fields::graphql_string_field(
            "file_tools_mode",
            selection.file_tools_mode.as_deref(),
        ),
        Some(graphql_fields::graphql_nullable_string_field(
            "file_tool_root",
            selection.file_tool_root.as_deref(),
        )),
        graphql_fields::graphql_optional_bool_field("enable_bash", selection.enable_bash),
        graphql_fields::graphql_string_field("bash_mode", selection.bash_mode.as_deref()),
        graphql_fields::graphql_string_field(
            "command_execution_policy",
            selection.command_execution_policy.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "command_allowed_argv_prefixes",
            selection.command_allowed_argv_prefixes.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "command_forbidden_argv_prefixes",
            selection.command_forbidden_argv_prefixes.as_deref(),
        ),
        graphql_fields::graphql_string_field(
            "command_network_mode",
            selection.command_network_mode.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "cli_tool_names",
            selection.cli_tool_names.as_deref(),
        ),
        graphql_fields::graphql_optional_bool_field(
            "enable_meta_tools",
            selection.enable_meta_tools,
        ),
        graphql_fields::graphql_string_list_field(
            "allowed_mcp_service_ids",
            selection.allowed_mcp_service_ids.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "backgroundable_tool_names",
            selection.backgroundable_tool_names.as_deref(),
        ),
        graphql_fields::graphql_string_list_field(
            "subagent_targets",
            selection.subagent_targets.as_deref(),
        ),
        graphql_fields::graphql_optional_bool_field(
            "subagent_spawn_enabled",
            selection.subagent_spawn_enabled,
        ),
        graphql_fields::graphql_optional_bool_field(
            "subagent_steering_enabled",
            selection.subagent_steering_enabled,
        ),
        graphql_fields::graphql_optional_bool_field(
            "subagent_background_enabled",
            selection.subagent_background_enabled,
        ),
        graphql_fields::graphql_optional_bool_field(
            "subagent_allow_cross_deployment",
            selection.subagent_allow_cross_deployment,
        ),
        selection
            .cross_deployment_spawn_timeout_seconds
            .map(|value| format!("cross_deployment_spawn_timeout_seconds: {value}")),
        graphql_fields::graphql_optional_bool_field("enable_memory", selection.enable_memory),
        graphql_fields::graphql_optional_bool_field(
            "enable_defra_query",
            selection.enable_defra_query,
        ),
        graphql_fields::graphql_string_list_field(
            "defra_query_collections",
            selection.defra_query_collections.as_deref(),
        ),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let mutation = format!(
        r#"mutation {{
            upsert_ToolSelection(
                filter: {{ selection_id: {{ _eq: "{escaped_selection_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("upsert ToolSelection failed: {:?}", resp.errors);
    }
    Ok(())
}
