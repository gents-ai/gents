use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use serde::Deserialize;
use serde_json::Value;

use crate::commands::codex_shim::store::query_node_json;
use crate::commands::codex_shim::ShimState;

use super::ConversationRow;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SessionRow {
    pub(super) session_id: String,
    #[serde(default)]
    pub(super) started: Option<String>,
}

pub(in crate::commands::codex_shim) async fn ensure_agent_session(
    state: &ShimState,
    session_id: &str,
) -> Result<()> {
    let agent_name = agent_name(state);
    let behavior_id = behavior_id(state);
    gents::session::ensure_session_with_behavior_id(
        state.node.as_ref(),
        session_id,
        &agent_name,
        state.agent_did.as_ref(),
        &behavior_id,
    )
    .await
    .context("ensuring exact AgentSession document")
}

pub(super) async fn load_scoped_session(
    state: &ShimState,
    session_id: &str,
) -> Result<Option<SessionRow>> {
    let Some(session) = gents::session::load_agent_session_exact(state.node.as_ref(), session_id)
        .await
        .context("loading exact AgentSession document")?
    else {
        return Ok(None);
    };
    if session.agent_did.as_deref() != Some(state.agent_did.as_ref()) {
        anyhow::bail!(
            "session {session_id:?} belongs to agent {:?}, not shim agent {:?}",
            session.agent_did,
            state.agent_did
        );
    }
    if session.behavior_id.as_deref() != Some(state.behavior_id.as_ref()) {
        anyhow::bail!(
            "session {session_id:?} is pinned to behavior {:?}, not shim behavior {:?}",
            session.behavior_id,
            state.behavior_id
        );
    }
    Ok(Some(SessionRow {
        session_id: session.session_id,
        started: Some(session.started),
    }))
}

pub(super) async fn list_scoped_sessions(state: &ShimState) -> Result<Vec<SessionRow>> {
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
    let escaped_behavior_id = escape_graphql_string(state.behavior_id.as_ref());
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }},
                order: {{ started: DESC }}
            ) {{
                session_id
                started
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    response
        .pointer("/data/AgentSession")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(serde_json::from_value)
        .collect::<serde_json::Result<Vec<_>>>()
        .context("decoding AgentSession rows")
}

pub(super) async fn codex_marked_session_ids(state: &ShimState) -> Result<HashSet<String>> {
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
    let escaped_behavior_id = escape_graphql_string(state.behavior_id.as_ref());
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }}
            ) {{
                session_id
                metadata
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    Ok(response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| {
            row.get("metadata")
                .and_then(Value::as_str)
                .is_some_and(|metadata| metadata.contains("\"codex_shim\""))
        })
        .filter_map(|row| {
            row.get("session_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect())
}

pub(in crate::commands::codex_shim) async fn ensure_agent_session_pinning(
    state: &ShimState,
    session_id: &str,
) -> Result<()> {
    let stored_behavior_id =
        gents::session::load_agent_session_exact(state.node.as_ref(), session_id)
            .await
            .context("loading exact AgentSession document for behavior pinning")?
            .and_then(|session| session.behavior_id);
    let bound_behavior_id = state.behavior_id.as_ref();
    if let Some(stored) = stored_behavior_id {
        if stored != bound_behavior_id {
            anyhow::bail!(
                "session {session_id:?} is pinned to behavior {stored:?}, but the shim \
                 is bound to {bound_behavior_id:?}. Restart the server with \
                 --codex-shim-behavior-id {stored} to resume this session."
            );
        }
    }
    Ok(())
}

pub(super) async fn load_conversation(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<ConversationRow>> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
    let escaped_behavior_id = escape_graphql_string(state.behavior_id.as_ref());
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{
                    session_id: {{ _eq: "{escaped_thread_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }},
                limit: 1
            ) {{
                title preview_text status created_at updated_at latest_request_id forked_from_session_id
            }}
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped_thread_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }},
                order: [{{ created_at: DESC }}, {{ request_id: DESC }}],
                limit: 1
            ) {{
                request_id
                lifecycle_state
                failure_reason
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    let conversation: Option<ConversationRow> = response
        .pointer("/data/AgentConversation")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding AgentConversation row")?;
    Ok(attach_latest_request(
        conversation,
        response.pointer("/data/AgentRequest/0"),
    ))
}

fn attach_latest_request(
    mut conversation: Option<ConversationRow>,
    request: Option<&Value>,
) -> Option<ConversationRow> {
    if conversation.is_none() && request.is_some() {
        conversation = Some(ConversationRow::default());
    }
    if let Some(conversation) = conversation.as_mut() {
        conversation.latest_request_lifecycle_state = request
            .and_then(|row| row.get("lifecycle_state"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        conversation.latest_request_failure_reason = request
            .and_then(|row| row.get("failure_reason"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
    }
    conversation
}

pub(super) async fn derive_thread_cwd(state: &ShimState, thread_id: &str) -> Result<PathBuf> {
    if let Some(cwd) = state.thread_cwd_override(thread_id).await {
        return Ok(cwd);
    }
    if let Some(cwd) = latest_request_metadata_cwd(state, thread_id).await? {
        return Ok(cwd);
    }
    if let Some(cwd) = settings_json_cwd(&state.cwd, &state.thread_settings(thread_id).await) {
        return Ok(cwd);
    }
    Ok(state.cwd.clone())
}

async fn latest_request_metadata_cwd(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<PathBuf>> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
    let escaped_behavior_id = escape_graphql_string(state.behavior_id.as_ref());
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped_thread_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }},
                order: {{ created_at: DESC }},
                limit: 10
            ) {{
                metadata
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for row in rows {
        let Some(metadata) = row.get("metadata").and_then(Value::as_str) else {
            continue;
        };
        if let Some(cwd) = metadata_json_cwd(&state.cwd, metadata) {
            return Ok(Some(cwd));
        }
    }
    Ok(None)
}

fn settings_json_cwd(base_cwd: &Path, settings_json: &str) -> Option<PathBuf> {
    json_path_cwd(base_cwd, settings_json, &["cwd"])
}

fn metadata_json_cwd(base_cwd: &Path, metadata: &str) -> Option<PathBuf> {
    json_path_cwd(base_cwd, metadata, &["codex_shim", "cwd"])
}

fn json_path_cwd(base_cwd: &Path, raw: &str, path: &[&str]) -> Option<PathBuf> {
    let parsed = serde_json::from_str::<Value>(raw).ok()?;
    let mut value = &parsed;
    for segment in path {
        value = value.get(*segment)?;
    }
    value
        .as_str()
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(Path::new)
        .map(|cwd| absolute_cwd(base_cwd, cwd))
}

fn absolute_cwd(base_cwd: &Path, cwd: &Path) -> PathBuf {
    if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        base_cwd.join(cwd)
    }
}

fn behavior_id(state: &ShimState) -> String {
    state.behavior_id.as_ref().to_string()
}

fn agent_name(state: &ShimState) -> String {
    behavior_id(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pending_request_projects_even_before_conversation_materializes() {
        let request = json!({
            "request_id": "request-1",
            "lifecycle_state": "pending",
            "failure_reason": ""
        });
        let conversation = attach_latest_request(None, Some(&request))
            .expect("request should provide a projection shell");
        assert_eq!(
            conversation.latest_request_lifecycle_state.as_deref(),
            Some("pending")
        );
        assert_eq!(conversation.latest_request_failure_reason, None);
    }

    #[test]
    fn metadata_json_cwd_reads_codex_shim_cwd() {
        let base_cwd = Path::new("/workspace");
        assert_eq!(
            metadata_json_cwd(base_cwd, r#"{"codex_shim":{"cwd":"/repo"}}"#),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(
            metadata_json_cwd(base_cwd, r#"{"codex_shim":{"cwd":"repo"}}"#),
            Some(PathBuf::from("/workspace/repo"))
        );
        assert_eq!(metadata_json_cwd(base_cwd, r#"{"cwd":"/wrong"}"#), None);
    }

    #[test]
    fn settings_json_cwd_reads_thread_settings_cwd() {
        let base_cwd = Path::new("/workspace");
        assert_eq!(
            settings_json_cwd(base_cwd, r#"{"cwd":"/repo-from-settings"}"#),
            Some(PathBuf::from("/repo-from-settings"))
        );
        assert_eq!(
            settings_json_cwd(base_cwd, r#"{"cwd":"repo-from-settings"}"#),
            Some(PathBuf::from("/workspace/repo-from-settings"))
        );
        assert_eq!(settings_json_cwd(base_cwd, "{}"), None);
    }
}
