use rapidhash::{HashSetExt, RapidHashMap, RapidHashSet};
use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::caused_threads::{load_caused_thread, load_caused_threads_for_root_ids, CausedThread};
use super::host_runtime::thread_git_info;
use super::ShimState;

#[derive(Debug, thiserror::Error)]
pub(in crate::commands::codex_shim) enum CodexShimProjectionError {
    #[error("thread projection omitted {collection} rows")]
    ThreadRowsMissing { collection: String },
    #[error("usage query omitted {name} rows")]
    UsageRowsMissing { name: String },
    #[error("thread list crossed owner/requester/agent scope")]
    ThreadListScopeCrossed,
    #[error("thread list has duplicate canonical session identity")]
    ThreadListDuplicateIdentity,
    #[error(transparent)]
    DecodeUsageRows(serde_json::Error),
    #[error("decoding {collection} rows")]
    DecodeRows {
        collection: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("thread request head conflicts with bound session agent")]
    ThreadHeadConflict,
    #[error("thread head has missing or ambiguous physical request")]
    ThreadHeadAmbiguous,
    #[error("decode exact thread head")]
    DecodeThreadHead {
        #[source]
        source: serde_json::Error,
    },
    #[error("thread session vanished during rename")]
    ThreadSessionVanished,
    #[error("thread agent changed during rename")]
    ThreadAgentChanged,
    #[error("encoding Codex thread settings")]
    EncodeThreadSettings(#[source] serde_json::Error),
    #[error("ambiguous Codex root/child thread label across canonical scopes: {session_id}")]
    AmbiguousRootChildLabel { session_id: String },
    #[error("child session binding differs from its authorized request")]
    ChildBindingMismatch,
    #[error("child request has no creation timestamp")]
    ChildMissingCreatedAt,
    #[error("child request has an invalid creation timestamp")]
    ChildInvalidCreatedAt {
        #[source]
        source: chrono::ParseError,
    },
    #[error("committed request missing agent")]
    CommittedRequestMissingAgent,
    #[error("usage request missing logical ID")]
    UsageRequestMissingLogicalId,
    #[error("usage request missing physical ID")]
    UsageRequestMissingPhysicalId,
    #[error("ambiguous scoped usage request")]
    AmbiguousScopedUsageRequest,
    #[error("usage request is absent from exact requester scope")]
    UsageRequestOutOfScope,
    #[error("invalid durable Goal status")]
    InvalidGoalStatus,
    #[error("history request missing physical identity")]
    HistoryRequestMissingIdentity,
    #[error("duplicate scoped request label in history")]
    DuplicateRequestLabel,
    #[error("history dependent row missing request label")]
    HistoryDependentMissingRequestLabel,
    #[error("history dependent row missing physical request")]
    HistoryDependentMissingPhysicalRequest,
    #[error("history dependent row crosses physical request identity")]
    HistoryDependentCrossesIdentity,
    #[error("history request lacks physical identity")]
    HistoryRequestLacksIdentity,
    #[error("decoding canonical AgentMessage header")]
    DecodeMessageHeader {
        #[source]
        source: gents::session::canonical_rows::CanonicalRowsError,
    },
    #[error("duplicate physical AgentMessage row in history")]
    DuplicatePhysicalMessage,
    #[error("canonical history header changed during exact physical reconstruction")]
    CanonicalHeaderChanged,
    #[error("history tool row omitted physical request")]
    HistoryToolRowMissingRequest,
    #[error("terminal AgentToolCall has no canonical delivery yet")]
    TerminalToolCallUndelivered,
    #[error("cycle in Codex steering history ancestry at request {request_id}")]
    SteeringCycle { request_id: String },
    #[error(transparent)]
    Session(#[from] gents::session::SessionError),
    #[error(transparent)]
    Access(#[from] gents::config_client::ConfigClientError),
}

impl gents::config_client::TransactionFailure for CodexShimProjectionError {
    fn config_client_error(&self) -> Option<&gents::config_client::ConfigClientError> {
        match self {
            Self::Session(error) => error.config_client_error(),
            Self::Access(error) => Some(error),
            _ => None,
        }
    }
}

mod goal;
mod json;
mod mutations;
mod storage;
mod usage;

#[cfg(test)]
mod conformance;

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
    pub(super) caused: Option<CausedThread>,
}

impl CodexThreadRecord {
    pub(super) fn is_caused(&self) -> bool {
        self.caused.is_some()
    }

    pub(super) fn projection_agent_id<'a>(&'a self, root_agent_id: &'a str) -> &'a str {
        self.caused
            .as_ref()
            .map(|link| link.agent_id.as_str())
            .unwrap_or(root_agent_id)
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
    if let Some((session, head)) = load_thread_state(state, thread_id).await? {
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
    let Some(link) = load_caused_thread(state, thread_id).await? else {
        return Ok(None);
    };
    Ok(Some(assemble_caused_record(state, link).await?))
}

pub(super) async fn root_thread_ids(state: &ShimState) -> Result<RapidHashSet<String>> {
    let mut ids = list_scoped_sessions(state)
        .await?
        .into_iter()
        .map(|session| session.session_id)
        .collect::<RapidHashSet<_>>();
    ids.extend(state.created_thread_ids().await);
    Ok(ids)
}

pub(super) async fn loaded_codex_thread_ids(state: &ShimState) -> Result<Vec<String>> {
    let mut loaded = state.loaded_thread_ids().await;
    let root_ids = loaded
        .iter()
        .cloned()
        .collect::<rapidhash::RapidHashSet<_>>();
    for link in load_caused_threads_for_root_ids(state, &loaded).await? {
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
    let mut seen = RapidHashSet::with_capacity(sessions.len());
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
    include_caused: bool,
) -> Result<Vec<CodexThreadRecord>> {
    let mut git_info_cache = ThreadGitInfoCache::default();
    if archived || !include_caused {
        return if include_cli {
            list_codex_threads_by_archived_with_git_cache(state, archived, &mut git_info_cache)
                .await
        } else {
            Ok(Vec::new())
        };
    }

    let active_roots =
        list_codex_threads_by_archived_with_git_cache(state, false, &mut git_info_cache).await?;
    let archived_roots =
        list_codex_threads_by_archived_with_git_cache(state, true, &mut git_info_cache).await?;
    let root_workspaces = index_root_workspaces(active_roots.iter().chain(&archived_roots));
    let root_ids = root_workspaces.keys().cloned().collect::<Vec<_>>();
    let links = load_caused_threads_for_root_ids(state, &root_ids).await?;
    for link in &links {
        if root_workspaces.contains_key(&link.session_id)
            && !(link.node_did == state.node_did.as_ref()
                && link.requester_did.as_deref() == Some(state.local_requester_did()))
        {
            return Err(CodexShimProjectionError::AmbiguousRootChildLabel {
                session_id: link.session_id.clone(),
            }
            .into());
        }
    }
    let mut seen = RapidHashSet::<String>::new();
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
                    "skipping authorized caused thread whose Codex root workspace is unavailable"
                );
                continue;
            };
            records.push(assemble_caused_record_with_workspace(state, link, workspace).await?);
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
        caused: None,
    })
}

async fn assemble_caused_record(
    state: &ShimState,
    link: CausedThread,
) -> Result<CodexThreadRecord> {
    let cwd = storage::derive_thread_cwd(state, &link.root_session_id).await?;
    let git_info = thread_git_info(&cwd).await;
    assemble_caused_record_parts(state, link, cwd, git_info).await
}

async fn assemble_caused_record_with_workspace(
    state: &ShimState,
    link: CausedThread,
    workspace: &RootThreadWorkspace,
) -> Result<CodexThreadRecord> {
    assemble_caused_record_parts(
        state,
        link,
        workspace.cwd.clone(),
        workspace.git_info.clone(),
    )
    .await
}

async fn assemble_caused_record_parts(
    state: &ShimState,
    link: CausedThread,
    cwd: PathBuf,
    git_info: Option<Value>,
) -> Result<CodexThreadRecord> {
    let session = gents::config_client::ConfigAccess::transact_local_write(
        &state.node,
        None,
        "codex.caused.session",
        |txn| {
            let owner = &link.node_did;
            let id = &link.session_id;
            let requester = link.requester_did.as_deref();
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents::session::load_agent_session_row_in_txn(txn, owner, id, requester)
                        .await?
                        .map(|row| row.session),
                )
            })
        },
    )
    .await?;
    if let Some(session) = &session {
        if session.agent_id != link.agent_id {
            return Err(CodexShimProjectionError::ChildBindingMismatch.into());
        }
    } else {
        chrono::DateTime::parse_from_rfc3339(
            link.created_at
                .as_deref()
                .ok_or(CodexShimProjectionError::ChildMissingCreatedAt)?,
        )
        .map_err(|source| CodexShimProjectionError::ChildInvalidCreatedAt { source })?;
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
        caused: Some(link),
    })
}

#[derive(Debug, Clone, PartialEq)]
struct RootThreadWorkspace {
    cwd: PathBuf,
    git_info: Option<Value>,
}

fn index_root_workspaces<'a>(
    roots: impl IntoIterator<Item = &'a CodexThreadRecord>,
) -> RapidHashMap<String, RootThreadWorkspace> {
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
    root_workspaces: &'a RapidHashMap<String, RootThreadWorkspace>,
    link: &CausedThread,
) -> Option<&'a RootThreadWorkspace> {
    root_workspaces.get(&link.root_session_id)
}

#[derive(Default)]
struct ThreadGitInfoCache {
    by_cwd: RapidHashMap<PathBuf, Option<Value>>,
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
