use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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

pub(super) use goal::{clear_codex_thread_goal, get_codex_thread_goal, set_codex_thread_goal};
pub(super) use json::{
    codex_thread_json, codex_thread_json_with_turns, projected_thread_status, thread_response_json,
    thread_resume_response_json, thread_start_response_json,
};
pub(super) use mutations::{
    set_codex_thread_archived, set_codex_thread_git_info, set_codex_thread_loaded,
    set_codex_thread_memory_mode, set_codex_thread_name, set_codex_thread_settings,
};

use storage::{list_scoped_sessions, load_thread_state};
pub(super) use usage::{
    latest_inference_usage_observation, submitted_token_usage, thread_record_token_usage,
    thread_token_usage, TokenTotals,
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
    pub(super) session: Option<gents_protocol::session::AgentSession>,
    pub(super) latest_request: Option<gents_protocol::graphql::GraphqlTurnState>,
    pub(super) subagent: Option<LinkedSubagentThread>,
}

impl CodexThreadRecord {
    pub(super) fn is_subagent(&self) -> bool {
        self.subagent.is_some()
    }

    pub(super) fn projection_behavior_id<'a>(&'a self, root_behavior_id: &'a str) -> &'a str {
        self.subagent
            .as_ref()
            .map(|link| link.behavior_id.as_str())
            .unwrap_or(root_behavior_id)
    }
}

pub(super) async fn create_codex_thread(
    state: &ShimState,
    thread_id: &str,
    cwd: &Path,
) -> Result<CodexThreadRecord> {
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
    // A wire thread ID has no principal/requester component. Validate child
    // identities before choosing a root record with the same label.
    let links = load_authorized_subagent_threads(state).await?;
    if state.is_thread_created(thread_id).await {
        for link in links.iter().filter(|link| link.session_id == thread_id) {
            anyhow::ensure!(
                link.agent_did == state.agent_did.as_ref()
                    && link.requester_did.as_deref() == Some(state.local_requester_did()),
                "ambiguous Codex ephemeral/child thread label across canonical scopes: {thread_id}"
            );
        }
    }
    if let Some((session, head)) = load_thread_state(state, thread_id).await? {
        for link in links.iter().filter(|link| link.session_id == thread_id) {
            anyhow::ensure!(
                link.agent_did == session.agent_did && link.requester_did == session.requester_did,
                "ambiguous Codex root/child thread label across canonical scopes: {thread_id}"
            );
        }
        return Ok(Some(
            assemble_record(state, thread_id, Some(session), head).await?,
        ));
    }
    if state.is_thread_created(thread_id).await {
        return Ok(Some(
            assemble_record(
                state,
                thread_id,
                None,
                storage::load_local_head(state, thread_id).await?,
            )
            .await?,
        ));
    }
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
    let mut seen = HashSet::with_capacity(sessions.len());
    let mut records = Vec::with_capacity(sessions.len());
    for session in sessions {
        let session_id = session.session_id;
        if state.is_thread_archived(&session_id).await != archived {
            continue;
        }
        let Some((session, head)) = load_thread_state(state, &session_id).await? else {
            continue;
        };
        seen.insert(session_id.clone());
        records.push(
            assemble_record_with_git_cache(state, &session_id, Some(session), head, git_info_cache)
                .await?,
        );
    }
    for session_id in state.created_thread_ids().await {
        if seen.contains(&session_id) || state.is_thread_archived(&session_id).await != archived {
            continue;
        }
        records.push(
            assemble_record_with_git_cache(
                state,
                &session_id,
                None,
                storage::load_local_head(state, &session_id).await?,
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
    // graph. This keeps thread/list scoped to the same GENTS authority as
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
    for link in &links {
        if root_workspaces.contains_key(&link.session_id) {
            anyhow::ensure!(
                link.agent_did == state.agent_did.as_ref()
                    && link.requester_did.as_deref() == Some(state.local_requester_did()),
                "ambiguous Codex root/child thread label across canonical scopes: {}",
                link.session_id
            );
        }
    }
    let mut seen = HashSet::<String>::new();
    let mut records = if include_cli {
        active_roots
    } else {
        Vec::new()
    };
    records.reserve(links.len());
    seen.extend(records.iter().map(|record| record.session_id.clone()));
    for link in links {
        if seen.insert(link.session_id.clone()) {
            let Some(workspace) = root_workspace_for_link(&root_workspaces, &link) else {
                tracing::warn!(
                    thread_id = %link.session_id,
                    root_session_id = %link.root_session_id,
                    "skipping authorized subagent thread whose Codex root workspace is unavailable"
                );
                continue;
            };
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
    session: Option<gents_protocol::session::AgentSession>,
    latest_request: Option<gents_protocol::graphql::GraphqlTurnState>,
) -> Result<CodexThreadRecord> {
    let mut git_info_cache = ThreadGitInfoCache::default();
    assemble_record_with_git_cache(
        state,
        session_id,
        session,
        latest_request,
        &mut git_info_cache,
    )
    .await
}

async fn assemble_record_with_git_cache(
    state: &ShimState,
    session_id: &str,
    session: Option<gents_protocol::session::AgentSession>,
    latest_request: Option<gents_protocol::graphql::GraphqlTurnState>,
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
        name: state.thread_name(session_id).await,
        settings_json: state.thread_settings(session_id).await,
        git_info,
        projection_started: state.thread_created_at(session_id).await,
        session,
        latest_request,
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
    let session = gents::config_client::ConfigAccess::transact_local(
        &state.node,
        None,
        "codex.subagent.session",
        |txn| {
            let owner = &link.agent_did;
            let id = &link.session_id;
            let requester = link.requester_did.as_deref();
            Box::pin(async move {
                Ok(
                    gents::session::load_agent_session_row_in_txn(txn, owner, id, requester)
                        .await?
                        .map(|row| row.session),
                )
            })
        },
    )
    .await?;
    if let Some(session) = &session {
        anyhow::ensure!(
            session.behavior_id == link.behavior_id,
            "child session binding differs from its authorized request"
        );
    } else {
        // An admitted child request can be visible before its runtime session is created.
        // Its actual request timestamp is the only creation fact available then.
        chrono::DateTime::parse_from_rfc3339(
            link.created_at
                .as_deref()
                .context("child request has no creation timestamp")?,
        )
        .context("child request has an invalid creation timestamp")?;
    }
    state.set_thread_cwd(&link.session_id, cwd.clone()).await;
    Ok(CodexThreadRecord {
        session_id: link.session_id.clone(),
        cwd,
        archived: false,
        loaded: state.is_thread_loaded(&link.session_id).await,
        memory_mode: "disabled".to_string(),
        name: session
            .as_ref()
            .and_then(|session| session.title.as_ref())
            .map(|title| title.text.clone())
            .unwrap_or_else(|| link.nickname.clone()),
        settings_json: String::new(),
        git_info,
        projection_started: link.created_at.clone(),
        session,
        latest_request: None,
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
            session: None,
            latest_request: None,
            subagent: None,
        };
        let workspaces = index_root_workspaces([&root]);
        let expected = workspaces.get("root-session").expect("root workspace");

        for index in 0..128 {
            let link = LinkedSubagentThread {
                parent_request_doc_id: "test-parent-doc".into(),
                parent_agent_did: "did:parent".into(),
                parent_requester_did: None,
                request_doc_id: "test-request-doc".into(),
                latest_request_doc_id: "test-request-doc".into(),
                requester_did: Some("did:parent".into()),
                request_id: format!("request-{index}"),
                latest_request_id: format!("request-{index}"),
                latest_request_content: String::new(),
                latest_request_created_at: None,
                session_id: format!("child-{index}"),
                parent_request_id: "parent-request".to_string(),
                parent_tool_call_id: format!("spawn-{index}"),
                parent_session_id: "root-session".to_string(),
                root_session_id: "root-session".to_string(),
                depth: 1,
                agent_did: "did:test:child".to_string(),
                behavior_id: "child-behavior".to_string(),
                model: None,
                nickname: format!("child-{index}"),
                client_projection: gents_protocol::client_protocol::project_persisted_attempt(
                    "processing",
                    false,
                    None,
                ),
                failure_reason: None,
                created_at: None,
            };
            let workspace = root_workspace_for_link(&workspaces, &link)
                .expect("fan-out child should resolve its root workspace");
            assert!(std::ptr::eq(workspace, expected));
        }
        assert_eq!(workspaces.len(), 1);
    }
    #[test]
    fn projection_behavior_id_preserves_exact_child_binding() {
        let mut record = CodexThreadRecord {
            session_id: "root-session".to_string(),
            cwd: PathBuf::from("/workspace/root"),
            archived: false,
            loaded: true,
            memory_mode: "disabled".to_string(),
            name: String::new(),
            settings_json: String::new(),
            git_info: Some(json!({"sha": "abc", "branch": "main"})),
            projection_started: None,
            session: None,
            latest_request: None,
            subagent: None,
        };
        assert_eq!(
            record.projection_behavior_id("root-behavior"),
            "root-behavior"
        );
        let link = LinkedSubagentThread {
            parent_request_doc_id: "test-parent-doc".into(),
            parent_agent_did: "did:parent".into(),
            parent_requester_did: None,
            request_doc_id: "test-request-doc".into(),
            latest_request_doc_id: "test-request-doc".into(),
            requester_did: Some("did:parent".into()),
            request_id: "request-0".to_string(),
            latest_request_id: "request-0".to_string(),
            latest_request_content: String::new(),
            latest_request_created_at: None,
            session_id: "child-0".to_string(),
            parent_request_id: "parent-request".to_string(),
            parent_tool_call_id: "spawn-0".to_string(),
            parent_session_id: "root-session".to_string(),
            root_session_id: "root-session".to_string(),
            depth: 1,
            agent_did: "did:test:child".to_string(),
            behavior_id: "child-behavior".to_string(),
            model: None,
            nickname: "child-0".to_string(),
            client_projection: gents_protocol::client_protocol::project_persisted_attempt(
                "processing",
                false,
                None,
            ),
            failure_reason: None,
            created_at: None,
        };
        record.subagent = Some(link);
        assert_eq!(
            record.projection_behavior_id("root-behavior"),
            "child-behavior"
        );
        record.subagent.as_mut().unwrap().behavior_id = "  ".to_string();
        assert_eq!(record.projection_behavior_id("root-behavior"), "  ");
    }
}
