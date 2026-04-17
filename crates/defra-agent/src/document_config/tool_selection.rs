use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::graphql_fields;
use super::serde_helpers;
use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSelectionDocument {
    pub selection_id: String,
    pub agent_did: String,
    pub display_name: Option<String>,
    pub enable_file_tools: Option<bool>,
    pub file_tools_mode: Option<String>,
    pub file_tool_root: Option<String>,
    pub enable_bash: Option<bool>,
    pub bash_mode: Option<String>,
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
    pub delegate_to: Option<Vec<String>>,
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
                cli_tool_names
                enable_meta_tools
                delegate_to
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
                cli_tool_names
                enable_meta_tools
                delegate_to
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
                cli_tool_names
                enable_meta_tools
                delegate_to
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
                cli_tool_names
                enable_meta_tools
                delegate_to
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
        graphql_fields::graphql_string_list_field(
            "cli_tool_names",
            selection.cli_tool_names.as_deref(),
        ),
        graphql_fields::graphql_optional_bool_field(
            "enable_meta_tools",
            selection.enable_meta_tools,
        ),
        graphql_fields::graphql_string_list_field("delegate_to", selection.delegate_to.as_deref()),
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
        graphql_fields::graphql_string_list_field(
            "cli_tool_names",
            selection.cli_tool_names.as_deref(),
        ),
        graphql_fields::graphql_optional_bool_field(
            "enable_meta_tools",
            selection.enable_meta_tools,
        ),
        graphql_fields::graphql_string_list_field("delegate_to", selection.delegate_to.as_deref()),
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
