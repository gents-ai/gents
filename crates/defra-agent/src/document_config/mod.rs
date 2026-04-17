mod principal;
mod behavior;
mod inference_profile;
mod tool_selection;
mod serde_helpers;
mod graphql_fields;

use std::fmt;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use serde::de::{DeserializeOwned, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::graphql::escape_graphql_string;

const DEFAULT_BEHAVIOR_LABEL: &str = "Default";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPrincipal {
    pub agent_did: String,
    pub display_name: Option<String>,
    pub default_behavior_id: Option<String>,
    pub enabled: bool,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentBehavior {
    pub behavior_id: String,
    pub agent_did: String,
    pub display_name: Option<String>,
    pub system_prompt: Option<String>,
    pub backend_id: Option<String>,
    pub model_name: Option<String>,
    pub tool_selection_id: Option<String>,
    pub inference_profile_id: Option<String>,
    pub compaction_strategy: Option<String>,
    pub compaction_threshold: Option<f64>,
    pub enabled: bool,
    pub created_at: Option<String>,
}

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
    #[serde(default, deserialize_with = "deserialize_optional_string_vec")]
    pub cli_tool_names: Option<Vec<String>>,
    pub enable_meta_tools: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_optional_string_vec")]
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
            let behavior_id = normalize_optional_string(principal.default_behavior_id.as_deref())
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
                    normalize_optional_string(principal.default_behavior_id.as_deref())
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
            if normalize_optional_string(principal.default_behavior_id.as_deref()).is_none() {
                let fallback_display_name = default_display_name_for_did(agent_did);
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
            let fallback_display_name = default_display_name_for_did(agent_did);
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

pub async fn load_agent_principal(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Option<AgentPrincipal>> {
    Ok(load_agent_principal_record(node, agent_did)
        .await?
        .map(|(_, principal)| principal))
}

pub(crate) async fn load_agent_principal_record(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Option<(String, AgentPrincipal)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                limit: 1
            ) {{
                _docID
                agent_did
                display_name
                default_behavior_id
                enabled
                created_at
                created_by
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query AgentPrincipal failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "AgentPrincipal"))
}

pub(crate) async fn load_agent_principal_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, AgentPrincipal)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentPrincipal(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                agent_did
                display_name
                default_behavior_id
                enabled
                created_at
                created_by
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query AgentPrincipal by _docID failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "AgentPrincipal"))
}

pub async fn load_agent_behavior(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<Option<AgentBehavior>> {
    Ok(load_agent_behavior_record(node, behavior_id)
        .await?
        .map(|(_, behavior)| behavior))
}

pub(crate) async fn load_agent_behavior_record(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<Option<(String, AgentBehavior)>> {
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{escaped_behavior_id}" }} }},
                limit: 1
            ) {{
                _docID
                behavior_id
                agent_did
                display_name
                system_prompt
                backend_id
                model_name
                tool_selection_id
                inference_profile_id
                compaction_strategy
                compaction_threshold
                enabled
                created_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query AgentBehavior failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "AgentBehavior"))
}

pub(crate) async fn load_agent_behavior_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, AgentBehavior)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                behavior_id
                agent_did
                display_name
                system_prompt
                backend_id
                model_name
                tool_selection_id
                inference_profile_id
                compaction_strategy
                compaction_threshold
                enabled
                created_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query AgentBehavior by _docID failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "AgentBehavior"))
}

pub async fn list_agent_behaviors(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<AgentBehavior>> {
    Ok(list_agent_behavior_records(node, agent_did)
        .await?
        .into_iter()
        .map(|(_, behavior)| behavior)
        .collect())
}

pub(crate) async fn list_agent_behavior_records(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<(String, AgentBehavior)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                behavior_id
                agent_did
                display_name
                system_prompt
                backend_id
                model_name
                tool_selection_id
                inference_profile_id
                compaction_strategy
                compaction_threshold
                enabled
                created_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("list AgentBehavior failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "AgentBehavior"))
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

    Ok(first_row_with_doc_id(
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

    Ok(first_row_with_doc_id(
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

    Ok(first_row_with_doc_id(resp.data.as_ref(), "ToolSelection"))
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

    Ok(first_row_with_doc_id(resp.data.as_ref(), "ToolSelection"))
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

    Ok(rows_with_doc_id(resp.data.as_ref(), "ToolSelection"))
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

    Ok(rows_with_doc_id(resp.data.as_ref(), "ToolSelection"))
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

    Ok(rows_with_doc_id(resp.data.as_ref(), "InferenceProfile"))
}

pub async fn upsert_agent_behavior(node: &EmbeddedNode, behavior: &AgentBehavior) -> Result<()> {
    let escaped_behavior_id = escape_graphql_string(&behavior.behavior_id);
    let escaped_agent_did = escape_graphql_string(&behavior.agent_did);
    let created_at = behavior
        .created_at
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    let add_fields = vec![
        Some(format!(r#"behavior_id: "{escaped_behavior_id}""#)),
        Some(format!(r#"agent_did: "{escaped_agent_did}""#)),
        graphql_string_field("display_name", behavior.display_name.as_deref()),
        graphql_string_field("system_prompt", behavior.system_prompt.as_deref()),
        graphql_string_field("backend_id", behavior.backend_id.as_deref()),
        graphql_string_field("model_name", behavior.model_name.as_deref()),
        graphql_string_field("tool_selection_id", behavior.tool_selection_id.as_deref()),
        graphql_string_field(
            "inference_profile_id",
            behavior.inference_profile_id.as_deref(),
        ),
        graphql_string_field(
            "compaction_strategy",
            behavior.compaction_strategy.as_deref(),
        ),
        graphql_optional_float_field("compaction_threshold", behavior.compaction_threshold),
        Some(format!("enabled: {}", graphql_bool(behavior.enabled))),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(created_at.as_str())
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        Some(format!(r#"agent_did: "{escaped_agent_did}""#)),
        graphql_string_field("display_name", behavior.display_name.as_deref()),
        graphql_string_field("system_prompt", behavior.system_prompt.as_deref()),
        graphql_string_field("backend_id", behavior.backend_id.as_deref()),
        graphql_string_field("model_name", behavior.model_name.as_deref()),
        graphql_string_field("tool_selection_id", behavior.tool_selection_id.as_deref()),
        graphql_string_field(
            "inference_profile_id",
            behavior.inference_profile_id.as_deref(),
        ),
        graphql_string_field(
            "compaction_strategy",
            behavior.compaction_strategy.as_deref(),
        ),
        graphql_optional_float_field("compaction_threshold", behavior.compaction_threshold),
        Some(format!("enabled: {}", graphql_bool(behavior.enabled))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let mutation = format!(
        r#"mutation {{
            upsert_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{escaped_behavior_id}" }} }},
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
        anyhow::bail!("upsert AgentBehavior failed: {:?}", resp.errors);
    }
    Ok(())
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
        graphql_string_field("display_name", selection.display_name.as_deref()),
        graphql_optional_bool_field("enable_file_tools", selection.enable_file_tools),
        graphql_string_field("file_tools_mode", selection.file_tools_mode.as_deref()),
        Some(graphql_nullable_string_field(
            "file_tool_root",
            selection.file_tool_root.as_deref(),
        )),
        graphql_optional_bool_field("enable_bash", selection.enable_bash),
        graphql_string_field("bash_mode", selection.bash_mode.as_deref()),
        graphql_string_list_field("cli_tool_names", selection.cli_tool_names.as_deref()),
        graphql_optional_bool_field("enable_meta_tools", selection.enable_meta_tools),
        graphql_string_list_field("delegate_to", selection.delegate_to.as_deref()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        Some(format!(r#"agent_did: "{escaped_agent_did}""#)),
        graphql_string_field("display_name", selection.display_name.as_deref()),
        graphql_optional_bool_field("enable_file_tools", selection.enable_file_tools),
        graphql_string_field("file_tools_mode", selection.file_tools_mode.as_deref()),
        Some(graphql_nullable_string_field(
            "file_tool_root",
            selection.file_tool_root.as_deref(),
        )),
        graphql_optional_bool_field("enable_bash", selection.enable_bash),
        graphql_string_field("bash_mode", selection.bash_mode.as_deref()),
        graphql_string_list_field("cli_tool_names", selection.cli_tool_names.as_deref()),
        graphql_optional_bool_field("enable_meta_tools", selection.enable_meta_tools),
        graphql_string_list_field("delegate_to", selection.delegate_to.as_deref()),
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
        graphql_string_field("display_name", profile.display_name.as_deref()),
        graphql_optional_int_field("context_window", profile.context_window),
        graphql_optional_int_field("max_output_tokens", profile.max_output_tokens),
        graphql_optional_int_field("max_turns", profile.max_turns),
        graphql_optional_float_field("temperature", profile.temperature),
        graphql_optional_int_field("stream_batch_ms", profile.stream_batch_ms),
        graphql_optional_int_field("deadline_duration_secs", profile.deadline_duration_secs),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        graphql_string_field("display_name", profile.display_name.as_deref()),
        graphql_optional_int_field("context_window", profile.context_window),
        graphql_optional_int_field("max_output_tokens", profile.max_output_tokens),
        graphql_optional_int_field("max_turns", profile.max_turns),
        graphql_optional_float_field("temperature", profile.temperature),
        graphql_optional_int_field("stream_batch_ms", profile.stream_batch_ms),
        graphql_optional_int_field("deadline_duration_secs", profile.deadline_duration_secs),
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

async fn create_default_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
) -> Result<()> {
    upsert_agent_behavior(
        node,
        &AgentBehavior {
            behavior_id: behavior_id.to_string(),
            agent_did: agent_did.to_string(),
            display_name: Some(DEFAULT_BEHAVIOR_LABEL.to_string()),
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            created_at: Some(chrono::Utc::now().to_rfc3339()),
        },
    )
    .await
}

pub async fn upsert_agent_principal(
    node: &EmbeddedNode,
    agent_did: &str,
    display_name: Option<&str>,
    default_behavior_id: Option<&str>,
    enabled: bool,
) -> Result<()> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let fallback_display_name = default_display_name_for_did(agent_did);
    let display_name =
        normalize_optional_string(display_name).unwrap_or(fallback_display_name.as_str());
    let escaped_display_name = escape_graphql_string(display_name);
    let escaped_default_behavior_id =
        escape_graphql_string(normalize_optional_string(default_behavior_id).unwrap_or_default());
    let escaped_created_by = escape_graphql_string(agent_did);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            upsert_AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                add: {{
                    agent_did: "{escaped_agent_did}",
                    display_name: "{escaped_display_name}",
                    default_behavior_id: "{escaped_default_behavior_id}",
                    enabled: {enabled},
                    created_at: "{created_at}",
                    created_by: "{escaped_created_by}"
                }},
                update: {{
                    display_name: "{escaped_display_name}",
                    default_behavior_id: "{escaped_default_behavior_id}",
                    enabled: {enabled}
                }}
            ) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("upsert AgentPrincipal failed: {:?}", resp.errors);
    }
    Ok(())
}

fn default_display_name_for_did(agent_did: &str) -> String {
    agent_did
        .rsplit(':')
        .next()
        .filter(|segment| !segment.trim().is_empty())
        .unwrap_or(agent_did)
        .to_string()
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn deserialize_optional_string_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalStringVecVisitor;

    impl<'de> Visitor<'de> for OptionalStringVecVisitor {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a string list, null, or empty string")
        }

        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value.trim().is_empty() {
                Ok(Some(Vec::new()))
            } else {
                Ok(Some(vec![value.to_string()]))
            }
        }

        fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            self.visit_str(&value)
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                values.push(value);
            }
            Ok(Some(values))
        }
    }

    deserializer.deserialize_any(OptionalStringVecVisitor)
}

fn first_row_with_doc_id<T>(data: Option<&serde_json::Value>, field: &str) -> Option<(String, T)>
where
    T: DeserializeOwned,
{
    rows_with_doc_id(data, field).into_iter().next()
}

fn rows_with_doc_id<T>(data: Option<&serde_json::Value>, field: &str) -> Vec<(String, T)>
where
    T: DeserializeOwned,
{
    data.and_then(|data| data.get(field))
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let doc_id = row.get("_docID")?.as_str()?.to_string();
                    let parsed = match serde_json::from_value(row.clone()) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            tracing::warn!(
                                field = field,
                                doc_id = %doc_id,
                                error = %error,
                                "failed to deserialize document row"
                            );
                            return None;
                        }
                    };
                    Some((doc_id, parsed))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn graphql_string_field(name: &str, value: Option<&str>) -> Option<String> {
    Some(format!(
        r#"{name}: "{}""#,
        escape_graphql_string(normalize_optional_string(value).unwrap_or_default())
    ))
}

fn graphql_nullable_string_field(name: &str, value: Option<&str>) -> String {
    match normalize_optional_string(value) {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

fn graphql_optional_int_field(name: &str, value: Option<i64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

fn graphql_optional_float_field(name: &str, value: Option<f64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

fn graphql_optional_bool_field(name: &str, value: Option<bool>) -> Option<String> {
    value.map(|value| format!("{name}: {}", graphql_bool(value)))
}

fn graphql_string_list_field(name: &str, value: Option<&[String]>) -> Option<String> {
    let values = value?;
    Some(format!(
        "{name}: [{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn graphql_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
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
