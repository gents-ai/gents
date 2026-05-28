use std::path::Path;

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;
use serde_json::Value;

use crate::commands::codex_shim::ShimState;
use crate::commands::codex_shim::protocol::absolute_path;
use crate::commands::codex_shim::store::query_node_json;

use super::ConversationRow;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ProjectionRow {
    pub(super) session_id: String,
    #[serde(default)]
    pub(super) cwd: String,
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
}

pub(super) struct ProjectionUpdate<'a> {
    session_id: &'a str,
    cwd: &'a Path,
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
    pub(super) fn new(session_id: &'a str, cwd: &'a Path) -> Self {
        Self {
            session_id,
            cwd,
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

pub(super) async fn upsert_projection(
    state: &ShimState,
    update: &ProjectionUpdate<'_>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(update.session_id);
    let escaped_cwd = escape_graphql_string(&absolute_path(update.cwd));
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
                    cwd: "{escaped_cwd}",
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
                    cwd: "{escaped_cwd}",
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

pub(super) async fn update_projection_loaded_cwd(
    state: &ShimState,
    thread_id: &str,
    loaded: bool,
    cwd: &Path,
) -> Result<()> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let escaped_cwd = escape_graphql_string(&absolute_path(cwd));
    let mutation = format!(
        r#"mutation {{
            update_CodexThreadProjection(
                filter: {{ session_id: {{ _eq: "{escaped_thread_id}" }} }},
                input: {{ loaded: {loaded}, cwd: "{escaped_cwd}", updated_at: "{now}" }}
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
