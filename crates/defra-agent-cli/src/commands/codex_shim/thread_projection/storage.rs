use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;
use serde_json::Value;

use crate::commands::codex_shim::store::query_node_json;
use crate::commands::codex_shim::ShimState;

use super::ConversationRow;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ProjectionRow {
    pub(super) session_id: String,
    #[serde(default)]
    pub(super) cwd: Option<String>,
    #[serde(default)]
    pub(super) archived: bool,
    #[serde(default)]
    pub(super) loaded: bool,
    #[serde(default = "default_memory_mode")]
    pub(super) memory_mode: String,
    #[serde(default)]
    pub(super) name: String,
    #[serde(default = "empty_json_object")]
    pub(super) settings_json: String,
    #[serde(default = "empty_json_object")]
    pub(super) goal_json: String,
    #[serde(default = "empty_json_object")]
    pub(super) git_info_json: String,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    #[serde(default)]
    pub(super) updated_at: Option<String>,
}

pub(super) struct ProjectionUpdate<'a> {
    session_id: &'a str,
    archived: bool,
    loaded: bool,
    memory_mode: &'a str,
    name: &'a str,
    settings_json: &'a str,
    goal_json: &'a str,
    rollback_user_turn: i32,
    git_info_json: &'a str,
}

impl<'a> ProjectionUpdate<'a> {
    pub(super) fn new(session_id: &'a str) -> Self {
        Self {
            session_id,
            archived: false,
            loaded: false,
            memory_mode: "enabled",
            name: "",
            settings_json: "{}",
            goal_json: "{}",
            rollback_user_turn: -1,
            git_info_json: "{}",
        }
    }

    pub(super) fn loaded(mut self, loaded: bool) -> Self {
        self.loaded = loaded;
        self
    }

    pub(super) fn memory_mode(mut self, memory_mode: &'a str) -> Self {
        self.memory_mode = memory_mode;
        self
    }

    pub(super) fn settings_json(mut self, settings_json: &'a str) -> Self {
        self.settings_json = settings_json;
        self
    }

    pub(super) fn git_info_json(mut self, git_info_json: &'a str) -> Self {
        self.git_info_json = git_info_json;
        self
    }
}

pub(super) async fn ensure_agent_session(state: &ShimState, session_id: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let agent_name = agent_name(state);
    let behavior_id = behavior_id(state);
    let escaped_agent_name = escape_graphql_string(&agent_name);
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

pub(super) async fn upsert_projection(
    state: &ShimState,
    update: &ProjectionUpdate<'_>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(update.session_id);
    let escaped_memory_mode = escape_graphql_string(update.memory_mode);
    let escaped_name = escape_graphql_string(update.name);
    let escaped_settings_json = escape_graphql_string(update.settings_json);
    let escaped_goal_json = escape_graphql_string(update.goal_json);
    let escaped_git_info_json = escape_graphql_string(update.git_info_json);
    let mutation = format!(
        r#"mutation {{
            upsert_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    archived: {archived},
                    loaded: {loaded},
                    memory_mode: "{escaped_memory_mode}",
                    name: "{escaped_name}",
                    settings_json: "{escaped_settings_json}",
                    goal_json: "{escaped_goal_json}",
                    rollback_user_turn: {rollback_user_turn},
                    git_info_json: "{escaped_git_info_json}",
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    archived: {archived},
                    loaded: {loaded},
                    memory_mode: "{escaped_memory_mode}",
                    name: "{escaped_name}",
                    settings_json: "{escaped_settings_json}",
                    goal_json: "{escaped_goal_json}",
                    rollback_user_turn: {rollback_user_turn},
                    git_info_json: "{escaped_git_info_json}",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#,
        archived = update.archived,
        loaded = update.loaded,
        rollback_user_turn = update.rollback_user_turn,
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(super) async fn update_projection_loaded(
    state: &ShimState,
    thread_id: &str,
    loaded: bool,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ loaded: {loaded}, updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        now = chrono::Utc::now().to_rfc3339(),
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}

pub(super) async fn load_projection(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<ProjectionRow>> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let query = format!(
        r#"{{
            CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                limit: 1
            ) {{
                session_id cwd archived loaded memory_mode name settings_json goal_json git_info_json
                created_at updated_at
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    response
        .pointer("/data/CodexThreadProjection")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding CodexThreadProjection row")
}

pub(super) async fn list_projection_rows(
    state: &ShimState,
    archived: bool,
) -> Result<Vec<ProjectionRow>> {
    let response = query_node_json(
        &state.node,
        &format!(
            r#"{{
            CodexThreadProjection(
                filter: {{ archived: {{ _eq: {archived} }} }},
                order: {{ updated_at: DESC }}
            ) {{
                session_id cwd archived loaded memory_mode name settings_json goal_json git_info_json
                created_at updated_at
            }}
        }}"#
        ),
    )
    .await?;
    response
        .pointer("/data/CodexThreadProjection")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            serde_json::from_value(row)
                .context("decoding CodexThreadProjection row for thread list")
        })
        .collect()
}

pub(super) async fn load_conversation(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<ConversationRow>> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
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

pub(super) async fn derive_thread_cwd(
    state: &ShimState,
    thread_id: &str,
    projection: Option<&ProjectionRow>,
    cwd_hint: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(cwd) = cwd_hint {
        return Ok(absolute_cwd(&state.cwd, cwd));
    }
    if let Some(cwd) = latest_request_metadata_cwd(state, thread_id).await? {
        return Ok(cwd);
    }
    if let Some(cwd) =
        projection.and_then(|projection| settings_json_cwd(&state.cwd, &projection.settings_json))
    {
        return Ok(cwd);
    }
    if let Some(cwd) = projection
        .and_then(|projection| projection.cwd.as_deref())
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
    {
        return Ok(PathBuf::from(cwd));
    }
    Ok(state.cwd.clone())
}

async fn latest_request_metadata_cwd(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<PathBuf>> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
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

fn default_memory_mode() -> String {
    "enabled".to_string()
}

fn empty_json_object() -> String {
    "{}".to_string()
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
    }

    #[test]
    fn settings_json_cwd_reads_thread_settings_cwd() {
        let base_cwd = Path::new("/workspace");
        assert_eq!(
            settings_json_cwd(base_cwd, r#"{"cwd":"/repo-from-settings"}"#),
            Some(PathBuf::from("/repo-from-settings"))
        );
        assert_eq!(settings_json_cwd(base_cwd, "{}"), None);
    }

    #[test]
    fn projection_row_accepts_null_legacy_cwd() {
        let row: ProjectionRow = serde_json::from_value(serde_json::json!({
            "session_id": "thread-1",
            "cwd": null
        }))
        .expect("row with null cwd should decode");

        assert_eq!(row.session_id, "thread-1");
        assert_eq!(row.cwd, None);
    }
}
