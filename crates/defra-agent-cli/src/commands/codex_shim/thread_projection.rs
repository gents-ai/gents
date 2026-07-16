use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::host_runtime::thread_git_info;
use super::subagent_projection::{
    load_authorized_subagent_threads, load_authorized_subagent_threads_for_root_ids,
    LinkedSubagentThread,
};
use super::ShimState;

mod goal;
mod json;
mod mutations;
mod storage;
mod usage;

pub(in crate::commands::codex_shim) use goal::StoredGoal;
pub(super) use goal::{clear_codex_thread_goal, get_codex_thread_goal, set_codex_thread_goal};
pub(super) use json::{
    codex_thread_json, codex_thread_json_with_turns, thread_response_json,
    thread_resume_response_json, thread_start_response_json,
};
pub(super) use mutations::{
    set_codex_thread_archived, set_codex_thread_git_info, set_codex_thread_loaded,
    set_codex_thread_memory_mode, set_codex_thread_name, set_codex_thread_settings,
};

pub(super) use storage::{ensure_agent_session, ensure_agent_session_pinning};
use storage::{list_scoped_sessions, load_conversation, load_scoped_session};
pub(super) use usage::{
    latest_inference_usage_observation, latest_requests_token_usage, session_token_usage,
    thread_record_token_usage, thread_token_usage, TokenTotals,
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
    pub(super) git_info: Option<Value>,
    pub(super) projection_started: Option<String>,
    pub(super) conversation: Option<ConversationRow>,
    pub(super) subagent: Option<LinkedSubagentThread>,
}

impl CodexThreadRecord {
    pub(super) fn is_subagent(&self) -> bool {
        self.subagent.is_some()
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ConversationRow {
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub(super) title: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub(super) preview_text: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub(super) status: String,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    #[serde(default)]
    pub(super) updated_at: Option<String>,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub(super) latest_request_id: String,
    #[serde(default)]
    pub(super) forked_from_session_id: Option<String>,
}

fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

pub(super) async fn create_codex_thread(
    state: &ShimState,
    thread_id: &str,
    cwd: &Path,
) -> Result<CodexThreadRecord> {
    ensure_agent_session(state, thread_id).await?;
    state.mark_thread_created(thread_id).await;
    state.set_thread_cwd(thread_id, cwd.to_path_buf()).await;
    state.set_thread_loaded(thread_id, true).await;
    state.set_thread_memory_mode(thread_id, "disabled").await;
    load_codex_thread(state, thread_id)
        .await?
        .with_context(|| format!("loading newly-created Codex thread {thread_id}"))
}

pub(super) async fn resume_loaded_codex_thread(
    state: &ShimState,
    thread_id: &str,
    cwd_override: Option<&str>,
    record: Option<CodexThreadRecord>,
) -> Result<Option<CodexThreadRecord>> {
    let Some(mut record) = record else {
        return Ok(None);
    };
    if let Some(cwd) = cwd_override.filter(|value| !value.trim().is_empty()) {
        let cwd = PathBuf::from(cwd);
        state.set_thread_cwd(thread_id, cwd.clone()).await;
        record.git_info = thread_git_info(&cwd).await;
        record.cwd = cwd;
    }
    state.set_thread_loaded(thread_id, true).await;
    record.loaded = true;
    Ok(Some(record))
}

pub(super) async fn load_codex_thread(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<CodexThreadRecord>> {
    if let Some(session) = load_scoped_session(state, thread_id).await? {
        let conversation = load_conversation(state, thread_id).await?;
        return Ok(Some(
            assemble_record(state, thread_id, session.started, conversation).await?,
        ));
    }
    let links = load_authorized_subagent_threads(state).await?;
    let Some(link) = links.into_iter().find(|link| link.session_id == thread_id) else {
        return Ok(None);
    };
    Ok(Some(assemble_subagent_record(state, link).await?))
}

pub(super) async fn loaded_codex_thread_ids(state: &ShimState) -> Result<Vec<String>> {
    let mut loaded = state.loaded_thread_ids().await;
    let root_ids = loaded
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    for link in load_authorized_subagent_threads_for_root_ids(state, &loaded).await? {
        if root_ids.contains(&link.root_session_id) && !loaded.contains(&link.session_id) {
            loaded.push(link.session_id);
        }
    }
    Ok(loaded)
}

async fn list_codex_threads_by_archived_with_git_cache(
    state: &ShimState,
    archived: bool,
    git_info_cache: &mut ThreadGitInfoCache,
) -> Result<Vec<CodexThreadRecord>> {
    let sessions = list_scoped_sessions(state).await?;
    // Ordinary CLI/chat/runtime sessions share the shim's (agent_did,
    // behavior_id), so the AgentSession spine is not sufficient on its own. A
    // session is a Codex thread only if it carries a durable `codex_shim`
    // request OR was created by this shim process (covers zero-turn starts that
    // have no request yet).
    let codex_marked = storage::codex_marked_session_ids(state).await?;
    let mut records = Vec::with_capacity(sessions.len());
    for session in sessions {
        let is_codex_thread = codex_marked.contains(&session.session_id)
            || state.is_thread_created(&session.session_id).await;
        if !is_codex_thread {
            continue;
        }
        if state.is_thread_archived(&session.session_id).await != archived {
            continue;
        }
        let conversation = load_conversation(state, &session.session_id).await?;
        records.push(
            assemble_record_with_git_cache(
                state,
                &session.session_id,
                session.started,
                conversation,
                git_info_cache,
            )
            .await?,
        );
    }
    Ok(records)
}

pub(super) async fn list_codex_threads_for_sources(
    state: &ShimState,
    archived: bool,
    include_cli: bool,
    include_subagents: bool,
) -> Result<Vec<CodexThreadRecord>> {
    let mut git_info_cache = ThreadGitInfoCache::default();
    if archived || !include_subagents {
        return if include_cli {
            list_codex_threads_by_archived_with_git_cache(state, archived, &mut git_info_cache)
                .await
        } else {
            Ok(Vec::new())
        };
    }

    // Start from durable Codex roots, then walk only their authorized bridge
    // graph. This keeps thread/list scoped to the same DEFRA authority as
    // thread/read and avoids a fleet-wide child scan. Git metadata is resolved
    // once per unique root workspace; children inherit their authorized root's
    // projection instead of spawning one git process per child.
    let active_roots =
        list_codex_threads_by_archived_with_git_cache(state, false, &mut git_info_cache).await?;
    let archived_roots =
        list_codex_threads_by_archived_with_git_cache(state, true, &mut git_info_cache).await?;
    let root_workspaces = index_root_workspaces(active_roots.iter().chain(&archived_roots));
    let root_ids = root_workspaces.keys().cloned().collect::<Vec<_>>();
    let links = load_authorized_subagent_threads_for_root_ids(state, &root_ids).await?;
    let mut seen = HashSet::<String>::new();
    let mut records = if include_cli {
        active_roots
    } else {
        Vec::new()
    };
    records.reserve(links.len());
    for link in links {
        if seen.insert(link.session_id.clone()) {
            let workspace =
                root_workspace_for_link(&root_workspaces, &link).with_context(|| {
                    format!(
                        "authorized subagent thread {} references unavailable Codex root {}",
                        link.session_id, link.root_session_id
                    )
                })?;
            records.push(assemble_subagent_record_with_workspace(state, link, workspace).await?);
        }
    }
    Ok(records)
}

pub(super) async fn store_forked_codex_thread(
    state: &ShimState,
    source: &CodexThreadRecord,
    child_session_id: &str,
    cwd: &Path,
) -> Result<CodexThreadRecord> {
    ensure_agent_session(state, child_session_id).await?;
    state.mark_thread_created(child_session_id).await;
    state
        .set_thread_cwd(child_session_id, cwd.to_path_buf())
        .await;
    state.set_thread_loaded(child_session_id, true).await;
    state
        .set_thread_memory_mode(child_session_id, &source.memory_mode)
        .await;
    state
        .set_thread_settings(child_session_id, &source.settings_json)
        .await;
    load_codex_thread(state, child_session_id)
        .await?
        .with_context(|| format!("loading forked Codex thread {child_session_id}"))
}

async fn assemble_record(
    state: &ShimState,
    session_id: &str,
    started: Option<String>,
    conversation: Option<ConversationRow>,
) -> Result<CodexThreadRecord> {
    let mut git_info_cache = ThreadGitInfoCache::default();
    assemble_record_with_git_cache(
        state,
        session_id,
        started,
        conversation,
        &mut git_info_cache,
    )
    .await
}

async fn assemble_record_with_git_cache(
    state: &ShimState,
    session_id: &str,
    started: Option<String>,
    conversation: Option<ConversationRow>,
    git_info_cache: &mut ThreadGitInfoCache,
) -> Result<CodexThreadRecord> {
    let cwd = storage::derive_thread_cwd(state, session_id).await?;
    state.set_thread_cwd(session_id, cwd.clone()).await;
    let git_info = git_info_cache.resolve(&cwd).await;
    Ok(CodexThreadRecord {
        session_id: session_id.to_string(),
        cwd,
        archived: state.is_thread_archived(session_id).await,
        loaded: state.is_thread_loaded(session_id).await,
        memory_mode: state.thread_memory_mode(session_id).await,
        name: String::new(),
        settings_json: state.thread_settings(session_id).await,
        git_info,
        projection_started: started,
        conversation,
        subagent: None,
    })
}

async fn assemble_subagent_record(
    state: &ShimState,
    link: LinkedSubagentThread,
) -> Result<CodexThreadRecord> {
    let cwd = storage::derive_thread_cwd(state, &link.root_session_id).await?;
    let git_info = thread_git_info(&cwd).await;
    assemble_subagent_record_parts(state, link, cwd, git_info).await
}

async fn assemble_subagent_record_with_workspace(
    state: &ShimState,
    link: LinkedSubagentThread,
    workspace: &RootThreadWorkspace,
) -> Result<CodexThreadRecord> {
    assemble_subagent_record_parts(
        state,
        link,
        workspace.cwd.clone(),
        workspace.git_info.clone(),
    )
    .await
}

async fn assemble_subagent_record_parts(
    state: &ShimState,
    link: LinkedSubagentThread,
    cwd: PathBuf,
    git_info: Option<Value>,
) -> Result<CodexThreadRecord> {
    state.set_thread_cwd(&link.session_id, cwd.clone()).await;
    Ok(CodexThreadRecord {
        session_id: link.session_id.clone(),
        cwd,
        archived: false,
        loaded: state.is_thread_loaded(&link.session_id).await,
        memory_mode: "disabled".to_string(),
        name: link.nickname.clone(),
        settings_json: String::new(),
        git_info,
        projection_started: link.created_at.clone(),
        conversation: None,
        subagent: Some(link),
    })
}

#[derive(Debug, Clone, PartialEq)]
struct RootThreadWorkspace {
    cwd: PathBuf,
    git_info: Option<Value>,
}

fn index_root_workspaces<'a>(
    roots: impl IntoIterator<Item = &'a CodexThreadRecord>,
) -> HashMap<String, RootThreadWorkspace> {
    roots
        .into_iter()
        .map(|root| {
            (
                root.session_id.clone(),
                RootThreadWorkspace {
                    cwd: root.cwd.clone(),
                    git_info: root.git_info.clone(),
                },
            )
        })
        .collect()
}

fn root_workspace_for_link<'a>(
    root_workspaces: &'a HashMap<String, RootThreadWorkspace>,
    link: &LinkedSubagentThread,
) -> Option<&'a RootThreadWorkspace> {
    root_workspaces.get(&link.root_session_id)
}

#[derive(Default)]
struct ThreadGitInfoCache {
    by_cwd: HashMap<PathBuf, Option<Value>>,
}

impl ThreadGitInfoCache {
    async fn resolve(&mut self, cwd: &Path) -> Option<Value> {
        self.resolve_with(cwd, |cwd| async move { thread_git_info(&cwd).await })
            .await
    }

    async fn resolve_with<F, Fut>(&mut self, cwd: &Path, load: F) -> Option<Value>
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: Future<Output = Option<Value>>,
    {
        if let Some(git_info) = self.by_cwd.get(cwd) {
            return git_info.clone();
        }
        let cwd = cwd.to_path_buf();
        let git_info = load(cwd.clone()).await;
        self.by_cwd.insert(cwd, git_info.clone());
        git_info
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn git_info_cache_resolves_once_per_unique_workspace_including_misses() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut cache = ThreadGitInfoCache::default();
        let shared = Path::new("/workspace/shared");

        for _ in 0..128 {
            let calls = Arc::clone(&calls);
            let git_info = cache
                .resolve_with(shared, move |_| async move {
                    calls.fetch_add(1, Ordering::Relaxed);
                    None
                })
                .await;
            assert!(git_info.is_none());
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        let calls_for_other = Arc::clone(&calls);
        let other = cache
            .resolve_with(Path::new("/workspace/other"), move |_| async move {
                calls_for_other.fetch_add(1, Ordering::Relaxed);
                Some(json!({"sha": "abc"}))
            })
            .await;
        assert_eq!(other, Some(json!({"sha": "abc"})));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn subagent_fanout_reuses_the_authorized_root_workspace_projection() {
        let root = CodexThreadRecord {
            session_id: "root-session".to_string(),
            cwd: PathBuf::from("/workspace/root"),
            archived: false,
            loaded: true,
            memory_mode: "disabled".to_string(),
            name: String::new(),
            settings_json: String::new(),
            git_info: Some(json!({"sha": "abc", "branch": "main"})),
            projection_started: None,
            conversation: None,
            subagent: None,
        };
        let workspaces = index_root_workspaces([&root]);
        let expected = workspaces.get("root-session").expect("root workspace");

        for index in 0..128 {
            let link = LinkedSubagentThread {
                request_id: format!("request-{index}"),
                latest_request_id: format!("request-{index}"),
                latest_request_content: String::new(),
                session_id: format!("child-{index}"),
                parent_request_id: "parent-request".to_string(),
                parent_tool_call_id: format!("spawn-{index}"),
                parent_session_id: "root-session".to_string(),
                root_session_id: "root-session".to_string(),
                depth: 1,
                agent_did: "did:defra:child".to_string(),
                behavior_id: "child-behavior".to_string(),
                model: None,
                nickname: format!("child-{index}"),
                lifecycle_state: "running".to_string(),
                failure_reason: None,
                created_at: None,
            };
            let workspace = root_workspace_for_link(&workspaces, &link)
                .expect("fan-out child should resolve its root workspace");
            assert!(std::ptr::eq(workspace, expected));
        }
        assert_eq!(workspaces.len(), 1);
    }
}
