use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::graphql_fields;
use super::serde_helpers;
use crate::document_config::SubagentTarget;
use crate::graphql::escape_graphql_string;

/// One field of a [`WriteToolDecl`]: a named slot the bound write tool exposes,
/// and whether the agent must provide it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WriteToolField {
    pub name: String,
    #[serde(default)]
    pub required: bool,
}

/// A declarative, schema-bounded document-write tool. Each declaration becomes
/// one runtime `BoundedWriteTool` that writes exactly one validated document to
/// one collection. Stored in the `ToolSelection.write_tools` `[String]` column
/// as the JSON serialization of one declaration per entry — mirroring the
/// `subagent_targets` `[String]` precedent so there is no Lean/schema change
/// beyond adding the column.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WriteToolDecl {
    pub tool_name: String,
    pub collection: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub fields: Vec<WriteToolField>,
}

impl WriteToolDecl {
    /// A decl is well-formed iff it names a non-empty tool and target collection.
    /// Single source of truth for the registration/advertisement gate.
    pub fn is_well_formed(&self) -> bool {
        !self.tool_name.trim().is_empty() && !self.collection.trim().is_empty()
    }
}

/// Deserialize the `write_tools` field from either representation:
/// - a JSON array of [`WriteToolDecl`] objects (manifest / `config apply` input),
/// - a JSON array of strings, each the JSON serialization of one
///   [`WriteToolDecl`] (how DefraDB returns the `[String]` column),
/// - `null` / missing / empty string (→ `None`).
///
/// This mirrors how `subagent_targets` survives the GraphQL `[String]` round-trip
/// while keeping the manifest-facing shape a structured list of objects.
fn deserialize_optional_write_tools<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<WriteToolDecl>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    use serde_json::Value;

    let value = Option::<Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(s) if s.trim().is_empty() => Ok(Some(Vec::new())),
        Value::String(s) => {
            // A single JSON-string entry (defensive; the column is a list).
            let decl: WriteToolDecl = serde_json::from_str(&s).map_err(D::Error::custom)?;
            Ok(Some(vec![decl]))
        }
        Value::Array(items) => {
            let mut decls = Vec::with_capacity(items.len());
            for item in items {
                let decl = match item {
                    // DefraDB `[String]` column: each entry is a JSON string.
                    Value::String(s) => {
                        serde_json::from_str::<WriteToolDecl>(&s).map_err(D::Error::custom)?
                    }
                    // Manifest input: each entry is a JSON object.
                    other => serde_json::from_value::<WriteToolDecl>(other)
                        .map_err(D::Error::custom)?,
                };
                decls.push(decl);
            }
            Ok(Some(decls))
        }
        other => Err(D::Error::custom(format!(
            "write_tools must be a list of WriteToolDecl objects or JSON strings, got {other}"
        ))),
    }
}

/// Encode the `write_tools` field for a GraphQL document mutation: each
/// [`WriteToolDecl`] is serialized to a JSON string so the value fits the
/// `[String]` column, then emitted via the shared string-list encoder (which
/// renders an empty list as `null`, never `[]`). Mirrors the `subagent_targets`
/// encode path.
fn graphql_write_tools_field(decls: Option<&[WriteToolDecl]>) -> Option<String> {
    let entries: Vec<String> = decls?
        .iter()
        .map(|decl| serde_json::to_string(decl).expect("WriteToolDecl serializes to JSON"))
        .collect();
    graphql_fields::graphql_string_list_field("write_tools", Some(&entries))
}

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
    pub enable_session_history_tool: Option<bool>,
    pub enable_defra_query: Option<bool>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_optional_string_vec"
    )]
    pub defra_query_collections: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_write_tools")]
    pub write_tools: Option<Vec<WriteToolDecl>>,
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
        if let Some(decls) = &self.write_tools {
            let mut seen_tool_names = std::collections::HashSet::new();
            for (i, decl) in decls.iter().enumerate() {
                // A decl must name a non-empty tool AND target collection.
                // `is_well_formed()` is the single source of truth for that gate;
                // mirror it here so malformed decls fail validation loudly
                // instead of being silently dropped at registration.
                if !decl.is_well_formed() {
                    return Err(anyhow::anyhow!(
                        "write_tools[{i}] is malformed (tool_name and collection must both be \
                         non-empty): tool_name={:?}, collection={:?}",
                        decl.tool_name,
                        decl.collection
                    ));
                }
                for (j, field) in decl.fields.iter().enumerate() {
                    if field.name.trim().is_empty() {
                        return Err(anyhow::anyhow!(
                            "write_tools[{i}] (tool {:?}) has a field[{j}] with an empty name; \
                             every WriteToolField must have a non-empty name",
                            decl.tool_name
                        ));
                    }
                }
                if !seen_tool_names.insert(decl.tool_name.trim()) {
                    return Err(anyhow::anyhow!(
                        "write_tools has a duplicate tool_name {:?}; each declared write tool \
                         must have a unique tool_name",
                        decl.tool_name.trim()
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
                enable_session_history_tool
                enable_defra_query
                defra_query_collections
                write_tools
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
                enable_session_history_tool
                enable_defra_query
                defra_query_collections
                write_tools
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
                enable_session_history_tool
                enable_defra_query
                defra_query_collections
                write_tools
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
                enable_session_history_tool
                enable_defra_query
                defra_query_collections
                write_tools
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
            "enable_session_history_tool",
            selection.enable_session_history_tool,
        ),
        graphql_fields::graphql_optional_bool_field(
            "enable_defra_query",
            selection.enable_defra_query,
        ),
        graphql_fields::graphql_string_list_field(
            "defra_query_collections",
            selection.defra_query_collections.as_deref(),
        ),
        graphql_write_tools_field(selection.write_tools.as_deref()),
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
            "enable_session_history_tool",
            selection.enable_session_history_tool,
        ),
        graphql_fields::graphql_optional_bool_field(
            "enable_defra_query",
            selection.enable_defra_query,
        ),
        graphql_fields::graphql_string_list_field(
            "defra_query_collections",
            selection.defra_query_collections.as_deref(),
        ),
        graphql_write_tools_field(selection.write_tools.as_deref()),
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
