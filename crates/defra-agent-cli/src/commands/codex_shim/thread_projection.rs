use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::ShimState;

mod goal;
mod json;
mod mutations;
mod storage;

pub(super) use goal::{clear_codex_thread_goal, get_codex_thread_goal, set_codex_thread_goal};
pub(super) use json::{
    codex_thread_json, codex_thread_json_with_turns, thread_response_json,
    thread_resume_response_json, thread_start_response_json,
};
pub(super) use mutations::{
    loaded_codex_thread_ids, set_codex_thread_archived, set_codex_thread_git_info,
    set_codex_thread_loaded, set_codex_thread_memory_mode, set_codex_thread_name,
    set_codex_thread_settings,
};

pub(super) use storage::ensure_agent_session_pinning;
use storage::{
    ensure_agent_session, list_projection_rows, load_conversation, load_projection,
    update_projection_loaded_cwd, upsert_projection, ProjectionUpdate,
};

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) struct CodexThreadRecord {
    pub(super) session_id: String,
    pub(super) cwd: PathBuf,
    pub(super) archived: bool,
    pub(super) loaded: bool,
    pub(super) memory_mode: String,
    pub(super) name: String,
    pub(super) settings_json: String,
    pub(super) goal_json: String,
    pub(super) git_info_json: String,
    pub(super) projection_created_at: Option<String>,
    pub(super) projection_updated_at: Option<String>,
    pub(super) conversation: Option<ConversationRow>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ConversationRow {
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) preview_text: String,
    #[serde(default)]
    pub(super) status: String,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    #[serde(default)]
    pub(super) updated_at: Option<String>,
    #[serde(default)]
    pub(super) latest_request_id: String,
    #[serde(default)]
    pub(super) forked_from_session_id: Option<String>,
}

pub(super) async fn create_codex_thread(
    state: &ShimState,
    thread_id: &str,
    cwd: &Path,
) -> Result<CodexThreadRecord> {
    ensure_agent_session(state, thread_id).await?;
    upsert_projection(
        state,
        &ProjectionUpdate::new(thread_id, cwd)
            .loaded(true)
            .memory_mode("enabled"),
    )
    .await?;
    load_codex_thread(state, thread_id)
        .await?
        .with_context(|| format!("loading newly-created Codex thread {thread_id}"))
}

pub(super) async fn resume_codex_thread(
    state: &ShimState,
    thread_id: &str,
    cwd_override: Option<&str>,
) -> Result<CodexThreadRecord> {
    let cwd = cwd_override
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| state.cwd.clone());

    if load_projection(state, thread_id).await?.is_none()
        && load_conversation(state, thread_id).await?.is_none()
    {
        ensure_agent_session(state, thread_id).await?;
    }

    match load_projection(state, thread_id).await? {
        Some(existing) => {
            let cwd = cwd_override
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(existing.cwd));
            update_projection_loaded_cwd(state, thread_id, true, &cwd).await?;
        }
        None => {
            upsert_projection(
                state,
                &ProjectionUpdate::new(thread_id, &cwd)
                    .loaded(true)
                    .memory_mode("enabled"),
            )
            .await?;
        }
    }

    load_codex_thread(state, thread_id)
        .await?
        .with_context(|| format!("loading resumed Codex thread {thread_id}"))
}

pub(super) async fn load_codex_thread(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<CodexThreadRecord>> {
    let conversation = load_conversation(state, thread_id).await?;
    let projection = load_projection(state, thread_id).await?;

    match (conversation, projection) {
        (None, None) => Ok(None),
        (conversation, Some(projection)) => Ok(Some(CodexThreadRecord {
            session_id: projection.session_id,
            cwd: PathBuf::from(projection.cwd),
            archived: projection.archived,
            loaded: projection.loaded,
            memory_mode: projection.memory_mode,
            name: projection.name,
            settings_json: projection.settings_json,
            goal_json: projection.goal_json,
            git_info_json: projection.git_info_json,
            projection_created_at: projection.created_at,
            projection_updated_at: projection.updated_at,
            conversation,
        })),
        (Some(conversation), None) => {
            upsert_projection(
                state,
                &ProjectionUpdate::new(thread_id, &state.cwd).memory_mode("enabled"),
            )
            .await?;
            Ok(Some(CodexThreadRecord {
                session_id: thread_id.to_string(),
                cwd: state.cwd.clone(),
                archived: false,
                loaded: false,
                memory_mode: "enabled".to_string(),
                name: String::new(),
                settings_json: "{}".to_string(),
                goal_json: "{}".to_string(),
                git_info_json: "{}".to_string(),
                projection_created_at: None,
                projection_updated_at: None,
                conversation: Some(conversation),
            }))
        }
    }
}

pub(super) async fn list_codex_threads_by_archived(
    state: &ShimState,
    archived: bool,
) -> Result<Vec<CodexThreadRecord>> {
    let rows = list_projection_rows(state, archived).await?;
    let mut records = Vec::with_capacity(rows.len());
    for projection in rows {
        let conversation = load_conversation(state, &projection.session_id).await?;
        records.push(CodexThreadRecord {
            session_id: projection.session_id,
            cwd: PathBuf::from(projection.cwd),
            archived: projection.archived,
            loaded: projection.loaded,
            memory_mode: projection.memory_mode,
            name: projection.name,
            settings_json: projection.settings_json,
            goal_json: projection.goal_json,
            git_info_json: projection.git_info_json,
            projection_created_at: projection.created_at,
            projection_updated_at: projection.updated_at,
            conversation,
        });
    }
    Ok(records)
}

pub(super) async fn store_forked_codex_thread(
    state: &ShimState,
    source: &CodexThreadRecord,
    child_session_id: &str,
    cwd: &Path,
) -> Result<CodexThreadRecord> {
    upsert_projection(
        state,
        &ProjectionUpdate::new(child_session_id, cwd)
            .loaded(true)
            .memory_mode(&source.memory_mode)
            .settings_json(&source.settings_json)
            .git_info_json(&source.git_info_json),
    )
    .await?;
    load_codex_thread(state, child_session_id)
        .await?
        .with_context(|| format!("loading forked Codex thread {child_session_id}"))
}
