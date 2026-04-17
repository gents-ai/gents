mod principal;
mod behavior;
mod inference_profile;
mod tool_selection;
mod serde_helpers;
mod graphql_fields;

pub use principal::{load_agent_principal, upsert_agent_principal, AgentPrincipal};
pub(crate) use principal::{load_agent_principal_by_doc_id, load_agent_principal_record};

pub use behavior::{
    list_agent_behaviors, load_agent_behavior, upsert_agent_behavior, AgentBehavior,
};
#[allow(unused_imports)]
pub(crate) use behavior::{
    list_agent_behavior_records, load_agent_behavior_by_doc_id, load_agent_behavior_record,
};
use behavior::create_default_behavior;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceProfile {
    pub profile_id: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub max_turns: Option<i64>,
    pub temperature: Option<f64>,
    pub stream_batch_ms: Option<i64>,
    pub deadline_duration_secs: Option<i64>,
}

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
    #[serde(default, deserialize_with = "serde_helpers::deserialize_optional_string_vec")]
    pub cli_tool_names: Option<Vec<String>>,
    pub enable_meta_tools: Option<bool>,
    #[serde(default, deserialize_with = "serde_helpers::deserialize_optional_string_vec")]
    pub delegate_to: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PrincipalBootstrap {
    pub principal: AgentPrincipal,
    pub default_behavior: AgentBehavior,
    pub created_principal: bool,
    pub created_default_behavior: bool,
}

pub fn default_behavior_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:default")
}

pub async fn ensure_agent_principal(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<PrincipalBootstrap> {
    let existing_principal = load_agent_principal(node, agent_did).await?;
    let (default_behavior_id, created_principal) = match existing_principal.as_ref() {
        Some(principal) => {
            let behavior_id = serde_helpers::normalize_optional_string(principal.default_behavior_id.as_deref())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| default_behavior_id_for_agent(agent_did));
            (behavior_id, false)
        }
        None => (default_behavior_id_for_agent(agent_did), true),
    };

    let (default_behavior, created_default_behavior) = match load_agent_behavior(
        node,
        &default_behavior_id,
    )
    .await?
    {
        Some(behavior) => {
            if behavior.agent_did != agent_did {
                return Err(anyhow!(
                    "AgentBehavior {default_behavior_id} belongs to {} not {agent_did}",
                    behavior.agent_did
                ));
            }
            (behavior, false)
        }
        None => {
            if existing_principal
                .as_ref()
                .and_then(|principal| {
                    serde_helpers::normalize_optional_string(principal.default_behavior_id.as_deref())
                })
                .is_some()
            {
                return Err(anyhow!(
                    "AgentPrincipal {agent_did} references missing default behavior {default_behavior_id}"
                ));
            }

            create_default_behavior(node, agent_did, &default_behavior_id).await?;
            let behavior = load_agent_behavior(node, &default_behavior_id)
                .await?
                .ok_or_else(|| {
                    anyhow!("default behavior {default_behavior_id} was not persisted")
                })?;
            (behavior, true)
        }
    };

    match existing_principal {
        Some(principal) => {
            if serde_helpers::normalize_optional_string(principal.default_behavior_id.as_deref()).is_none() {
                let fallback_display_name = serde_helpers::default_display_name_for_did(agent_did);
                upsert_agent_principal(
                    node,
                    agent_did,
                    principal
                        .display_name
                        .as_deref()
                        .or(Some(fallback_display_name.as_str())),
                    Some(&default_behavior_id),
                    principal.enabled,
                )
                .await?;
            }
        }
        None => {
            let fallback_display_name = serde_helpers::default_display_name_for_did(agent_did);
            upsert_agent_principal(
                node,
                agent_did,
                Some(fallback_display_name.as_str()),
                Some(&default_behavior_id),
                true,
            )
            .await?;
        }
    }

    let principal = load_agent_principal(node, agent_did)
        .await?
        .ok_or_else(|| anyhow!("AgentPrincipal {agent_did} was not persisted"))?;

    Ok(PrincipalBootstrap {
        principal,
        default_behavior,
        created_principal,
        created_default_behavior,
    })
}

pub async fn load_inference_profile(
    node: &EmbeddedNode,
    profile_id: &str,
) -> Result<Option<InferenceProfile>> {
    Ok(load_inference_profile_record(node, profile_id)
        .await?
        .map(|(_, profile)| profile))
}

pub(crate) async fn load_inference_profile_record(
    node: &EmbeddedNode,
    profile_id: &str,
) -> Result<Option<(String, InferenceProfile)>> {
    let escaped_profile_id = escape_graphql_string(profile_id);
    let query = format!(
        r#"{{
            InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{escaped_profile_id}" }} }},
                limit: 1
            ) {{
                _docID
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                stream_batch_ms
                deadline_duration_secs
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceProfile failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::first_row_with_doc_id(
        resp.data.as_ref(),
        "InferenceProfile",
    ))
}

pub(crate) async fn load_inference_profile_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, InferenceProfile)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            InferenceProfile(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                stream_batch_ms
                deadline_duration_secs
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceProfile by _docID failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::first_row_with_doc_id(
        resp.data.as_ref(),
        "InferenceProfile",
    ))
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

    Ok(serde_helpers::first_row_with_doc_id(resp.data.as_ref(), "ToolSelection"))
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

    Ok(serde_helpers::first_row_with_doc_id(resp.data.as_ref(), "ToolSelection"))
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

    Ok(serde_helpers::rows_with_doc_id(resp.data.as_ref(), "ToolSelection"))
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

    Ok(serde_helpers::rows_with_doc_id(resp.data.as_ref(), "ToolSelection"))
}

pub(crate) async fn list_inference_profile_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, InferenceProfile)>> {
    let query = r#"{
            InferenceProfile(order: { profile_id: ASC }) {
                _docID
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                stream_batch_ms
                deadline_duration_secs
            }
        }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list InferenceProfile failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::rows_with_doc_id(resp.data.as_ref(), "InferenceProfile"))
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
        graphql_fields::graphql_optional_bool_field("enable_file_tools", selection.enable_file_tools),
        graphql_fields::graphql_string_field("file_tools_mode", selection.file_tools_mode.as_deref()),
        Some(graphql_fields::graphql_nullable_string_field(
            "file_tool_root",
            selection.file_tool_root.as_deref(),
        )),
        graphql_fields::graphql_optional_bool_field("enable_bash", selection.enable_bash),
        graphql_fields::graphql_string_field("bash_mode", selection.bash_mode.as_deref()),
        graphql_fields::graphql_string_list_field("cli_tool_names", selection.cli_tool_names.as_deref()),
        graphql_fields::graphql_optional_bool_field("enable_meta_tools", selection.enable_meta_tools),
        graphql_fields::graphql_string_list_field("delegate_to", selection.delegate_to.as_deref()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        Some(format!(r#"agent_did: "{escaped_agent_did}""#)),
        graphql_fields::graphql_string_field("display_name", selection.display_name.as_deref()),
        graphql_fields::graphql_optional_bool_field("enable_file_tools", selection.enable_file_tools),
        graphql_fields::graphql_string_field("file_tools_mode", selection.file_tools_mode.as_deref()),
        Some(graphql_fields::graphql_nullable_string_field(
            "file_tool_root",
            selection.file_tool_root.as_deref(),
        )),
        graphql_fields::graphql_optional_bool_field("enable_bash", selection.enable_bash),
        graphql_fields::graphql_string_field("bash_mode", selection.bash_mode.as_deref()),
        graphql_fields::graphql_string_list_field("cli_tool_names", selection.cli_tool_names.as_deref()),
        graphql_fields::graphql_optional_bool_field("enable_meta_tools", selection.enable_meta_tools),
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

pub async fn upsert_inference_profile(
    node: &EmbeddedNode,
    profile: &InferenceProfile,
) -> Result<()> {
    let escaped_profile_id = escape_graphql_string(&profile.profile_id);

    let add_fields = vec![
        Some(format!(r#"profile_id: "{escaped_profile_id}""#)),
        graphql_fields::graphql_string_field("display_name", profile.display_name.as_deref()),
        graphql_fields::graphql_optional_int_field("context_window", profile.context_window),
        graphql_fields::graphql_optional_int_field("max_output_tokens", profile.max_output_tokens),
        graphql_fields::graphql_optional_int_field("max_turns", profile.max_turns),
        graphql_fields::graphql_optional_float_field("temperature", profile.temperature),
        graphql_fields::graphql_optional_int_field("stream_batch_ms", profile.stream_batch_ms),
        graphql_fields::graphql_optional_int_field("deadline_duration_secs", profile.deadline_duration_secs),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        graphql_fields::graphql_string_field("display_name", profile.display_name.as_deref()),
        graphql_fields::graphql_optional_int_field("context_window", profile.context_window),
        graphql_fields::graphql_optional_int_field("max_output_tokens", profile.max_output_tokens),
        graphql_fields::graphql_optional_int_field("max_turns", profile.max_turns),
        graphql_fields::graphql_optional_float_field("temperature", profile.temperature),
        graphql_fields::graphql_optional_int_field("stream_batch_ms", profile.stream_batch_ms),
        graphql_fields::graphql_optional_int_field("deadline_duration_secs", profile.deadline_duration_secs),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let mutation = format!(
        r#"mutation {{
            upsert_InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{escaped_profile_id}" }} }},
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
        anyhow::bail!("upsert InferenceProfile failed: {:?}", resp.errors);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_selection_document_accepts_empty_string_arrays() {
        let document: ToolSelectionDocument = serde_json::from_value(serde_json::json!({
            "selection_id": "did:defra-agent:test:default:tools",
            "agent_did": "did:defra-agent:test",
            "display_name": "Tools",
            "enable_file_tools": true,
            "file_tools_mode": "ReadOnly",
            "file_tool_root": null,
            "enable_bash": false,
            "bash_mode": "disabled",
            "cli_tool_names": "",
            "enable_meta_tools": false,
            "delegate_to": ""
        }))
        .expect("empty string arrays should deserialize");

        assert_eq!(document.cli_tool_names, Some(Vec::new()));
        assert_eq!(document.delegate_to, Some(Vec::new()));
    }

    #[test]
    fn tool_selection_document_accepts_string_array_values() {
        let document: ToolSelectionDocument = serde_json::from_value(serde_json::json!({
            "selection_id": "did:defra-agent:test:default:tools",
            "agent_did": "did:defra-agent:test",
            "display_name": "Tools",
            "enable_file_tools": true,
            "file_tools_mode": "ReadOnly",
            "file_tool_root": null,
            "enable_bash": false,
            "bash_mode": "disabled",
            "cli_tool_names": ["rg"],
            "enable_meta_tools": false,
            "delegate_to": ["did:defra-agent:other"]
        }))
        .expect("string arrays should deserialize");

        assert_eq!(document.cli_tool_names, Some(vec!["rg".to_string()]));
        assert_eq!(
            document.delegate_to,
            Some(vec!["did:defra-agent:other".to_string()])
        );
    }
}
