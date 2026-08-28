use anyhow::Result;
use gents::graphql::escape_graphql_string;
use gents_codex_protocol as codex;
use serde_json::Value;

use super::super::protocol::{send_error, send_result};
use super::super::store::{execute_committed, query_node_json};
use super::super::{Outbound, ShimState, JSONRPC_INVALID_PARAMS};
use crate::extract_mutation_doc_id;

fn skill_doc_path(skill_id: &str) -> gents_codex_protocol::AbsolutePathBuf {
    std::path::PathBuf::from(format!("/gents/skills/{skill_id}"))
        .try_into()
        .expect("synthetic skill path is absolute")
}

pub(super) async fn load_skill_metadata(state: &ShimState) -> Result<Vec<codex::SkillMetadata>> {
    let query = format!(
        r#"{{ Skill(filter: {{ agent_did: {{ _eq: "{did}" }} }}) {{
            skill_id name description scope enabled
        }} }}"#,
        did = escape_graphql_string(&state.agent_did),
    );
    let response = query_node_json(state.node.as_ref(), &query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("Skill"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut skills = Vec::new();
    for row in rows {
        let skill_id = row
            .get("skill_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if skill_id.trim().is_empty() {
            continue;
        }
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(skill_id)
            .to_string();
        let description = row
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let enabled = row.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        let scope = match row.get("scope").and_then(Value::as_str) {
            Some("principal") => codex::SkillScope::System,
            _ => codex::SkillScope::User,
        };
        skills.push(codex::SkillMetadata {
            name,
            description,
            short_description: None,
            interface: None,
            dependencies: None,
            path: skill_doc_path(skill_id),
            scope,
            enabled,
        });
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

pub(super) async fn handle_skills_config_write(
    outbound: &Outbound,
    state: &ShimState,
    request_id: codex::RequestId,
    params: codex::SkillsConfigWriteParams,
) -> Result<()> {
    let skill_id = match resolve_skill_id(state, &params).await {
        Ok(skill_id) => skill_id,
        Err(error) => {
            return send_error(
                outbound,
                request_id,
                JSONRPC_INVALID_PARAMS,
                error.to_string(),
            )
            .await;
        }
    };
    let agent_did = escape_graphql_string(&state.agent_did);
    let mutation = format!(
        r#"mutation {{ update_Skill(
            filter: {{
                skill_id: {{ _eq: "{skill_id}" }},
                agent_did: {{ _eq: "{agent_did}" }}
            }},
            input: {{ enabled: {enabled} }}
        ) {{ _docID }} }}"#,
        skill_id = escape_graphql_string(&skill_id),
        enabled = params.enabled,
    );
    let response = match execute_committed(state.node.as_ref(), &mutation).await {
        Ok(response) => response,
        Err(error) => {
            return send_error(
                outbound,
                request_id,
                JSONRPC_INVALID_PARAMS,
                format!("failed to update skill {skill_id}: {error}"),
            )
            .await;
        }
    };
    if extract_mutation_doc_id(&response, "Skill").is_err() {
        return send_error(
            outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            format!(
                "no skill {skill_id:?} belongs to bound agent {:?}",
                state.agent_did
            ),
        )
        .await;
    }
    send_result(
        outbound,
        request_id,
        codex::SkillsConfigWriteResponse {
            effective_enabled: params.enabled,
        },
    )
    .await
}

async fn resolve_skill_id(
    state: &ShimState,
    params: &codex::SkillsConfigWriteParams,
) -> Result<String> {
    if let Some(path) = params.path.as_ref() {
        if let Some(skill_id) = std::path::Path::new(path.as_path())
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|value| !value.trim().is_empty())
        {
            return Ok(skill_id.to_string());
        }
    }
    if let Some(name) = params
        .name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let metadata = load_skill_metadata(state).await?;
        if let Some(found) = metadata.iter().find(|skill| {
            skill.name == name
                || std::path::Path::new(skill.path.as_path())
                    .file_name()
                    .and_then(|segment| segment.to_str())
                    == Some(name)
        }) {
            return Ok(std::path::Path::new(found.path.as_path())
                .file_name()
                .and_then(|segment| segment.to_str())
                .unwrap_or(name)
                .to_string());
        }
        anyhow::bail!("no skill named {name:?}");
    }
    anyhow::bail!("skills/config/write requires a path or name selector")
}
