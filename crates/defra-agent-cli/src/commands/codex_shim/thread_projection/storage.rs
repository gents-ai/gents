use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
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
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let agent_name = agent_name(state);
    let behavior_id = behavior_id(state);
    let escaped_agent_name = escape_graphql_string(&agent_name);
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
    let escaped_behavior_id = escape_graphql_string(&behavior_id);
    // agent_name + behavior_id are write-once-at-create. The pin lives on the
    // AgentSession; reopening a session under a different shim binding must
    // not silently rebind it.
    let mutation = format!(
        r#"mutation {{
            upsert_AgentSession(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_agent_name}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    started: "{now}",
                    status: "active"
                }},
                update: {{
                    status: "active"
                }}
            ) {{ _docID }}
        }}"#
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

/// Load a single AgentSession scoped to the shim's identity. Returns `None`
/// for unknown ids OR sessions owned by another (agent_did, behavior_id).
pub(super) async fn load_scoped_session(
    state: &ShimState,
    session_id: &str,
) -> Result<Option<SessionRow>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(state.agent_did.as_ref());
    let escaped_behavior_id = escape_graphql_string(state.behavior_id.as_ref());
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }},
                limit: 1
            ) {{
                session_id
                started
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    response
        .pointer("/data/AgentSession/0")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding AgentSession row")
}

/// List all AgentSessions scoped to the shim's identity, newest first.
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

/// Session ids (scoped to the shim's identity) that carry at least one
/// `codex_shim`-marked request. This is the durable Codex-ownership signal that
/// distinguishes Codex threads from ordinary CLI/chat/runtime sessions that
/// share the same `(agent_did, behavior_id)` — those create `AgentSession`s too
/// but never a `codex_shim` request.
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

/// Verify the existing AgentSession (if any) is pinned to the shim's current
/// bound behavior. If a session was created under a different binding,
/// resuming it under the current shim is rejected so we don't silently
/// reroute its turns.
pub(in crate::commands::codex_shim) async fn ensure_agent_session_pinning(
    state: &ShimState,
    session_id: &str,
) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                limit: 1
            ) {{
                behavior_id
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    let stored_behavior_id = response
        .pointer("/data/AgentSession/0/behavior_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);
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
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    response
        .pointer("/data/AgentConversation")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding AgentConversation row")
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
