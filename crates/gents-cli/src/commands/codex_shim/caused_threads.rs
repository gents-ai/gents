use std::collections::{HashMap, HashSet};

use anyhow::Result;
use gents_protocol::client_protocol::{project_persisted_attempt, ClientHeadProjection};

use super::bound_behavior::load_bound_model_selection_id;
use super::ShimState;
use crate::caused_sessions::{load_caused_sessions, CausedSession, SessionScope};

const CAUSED_THREAD_COLLECTIONS: [&str; 4] = [
    "AgentRequest",
    "AgentBehavior",
    "InferenceProfile",
    "InferenceBackend",
];

#[derive(Clone, Debug)]
pub(super) struct CausedThreadUpdateFilter {
    collection_ids: HashSet<String>,
    match_all_updates: bool,
}

impl CausedThreadUpdateFilter {
    pub(super) fn from_state(state: &ShimState) -> Self {
        let mut collection_ids = HashSet::new();
        let mut match_all_updates = false;
        for collection_name in CAUSED_THREAD_COLLECTIONS {
            match state.node.get_collection(collection_name) {
                Ok(Some(definition)) => {
                    collection_ids.insert(definition.collection_id);
                }
                Ok(None) => {
                    match_all_updates = true;
                    tracing::warn!(
                        collection_name,
                        "Codex shim could not resolve a caused-thread collection; \
                         falling back to invalidation on every document update"
                    );
                }
                Err(error) => {
                    match_all_updates = true;
                    tracing::warn!(
                        collection_name,
                        %error,
                        "Codex shim failed to resolve a caused-thread collection; \
                         falling back to invalidation on every document update"
                    );
                }
            }
        }
        Self {
            collection_ids,
            match_all_updates,
        }
    }

    pub(super) fn affects_collection_id(&self, collection_id: &str) -> bool {
        self.match_all_updates || self.collection_ids.contains(collection_id)
    }
}

/// A session started from a Codex-owned root, projected as a read-only
/// Codex sub-agent thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CausedThread {
    pub(super) session_id: String,
    pub(super) agent_did: String,
    pub(super) requester_did: Option<String>,
    pub(super) behavior_id: String,
    pub(super) parent_session_id: String,
    pub(super) root_session_id: String,
    pub(super) depth: u32,
    pub(super) nickname: String,
    pub(super) model: Option<String>,
    pub(super) client_projection: Option<ClientHeadProjection>,
    pub(super) failure_reason: Option<String>,
    pub(super) created_at: Option<String>,
    pub(super) latest_request_id: String,
    pub(super) latest_request_doc_id: String,
    pub(super) latest_request_content: String,
    pub(super) latest_request_created_at: Option<String>,
}

pub(super) async fn load_caused_threads(state: &ShimState) -> Result<Vec<CausedThread>> {
    let roots = super::thread_projection::root_thread_ids(state).await?;
    load_caused_threads_for_root_ids(state, &roots).await
}

pub(super) async fn load_caused_threads_for_root(
    state: &ShimState,
    root_session_id: &str,
) -> Result<Vec<CausedThread>> {
    load_caused_threads_for_root_ids(state, &[root_session_id.to_string()]).await
}

pub(super) async fn load_caused_threads_for_root_ids(
    state: &ShimState,
    root_session_ids: &[String],
) -> Result<Vec<CausedThread>> {
    if root_session_ids.is_empty() {
        return Ok(Vec::new());
    }
    let roots = root_session_ids
        .iter()
        .map(|session_id| SessionScope {
            agent_did: state.agent_did.to_string(),
            session_id: session_id.clone(),
            requester_did: Some(state.local_requester_did().to_string()),
        })
        .collect::<Vec<_>>();
    let sessions = load_caused_sessions(&state.node, &roots).await?;
    let mut threads = Vec::with_capacity(sessions.len());
    let mut labels = HashMap::<String, SessionScope>::new();
    let mut models = HashMap::<(String, String), Option<String>>::new();
    for session in sessions {
        // A wire thread ID has no principal/requester component.
        if let Some(existing) = labels.get(&session.scope.session_id) {
            anyhow::ensure!(
                existing == &session.scope,
                "ambiguous Codex thread label across canonical session scopes: {}",
                session.scope.session_id
            );
            continue;
        }
        if root_session_ids.contains(&session.scope.session_id) {
            anyhow::ensure!(
                session.scope.agent_did == state.agent_did.as_ref()
                    && session.scope.requester_did.as_deref() == Some(state.local_requester_did()),
                "ambiguous Codex root/caused thread label across canonical scopes: {}",
                session.scope.session_id
            );
            continue;
        }
        labels.insert(session.scope.session_id.clone(), session.scope.clone());
        let key = (session.scope.agent_did.clone(), session.behavior_id.clone());
        if !models.contains_key(&key) {
            let model = match load_bound_model_selection_id(&state.node, &key.0, &key.1).await {
                Ok(model) => Some(model),
                Err(error) => {
                    tracing::debug!(
                        error = format!("{error:#}"),
                        agent_did = key.0,
                        behavior_id = key.1,
                        "Codex caused thread has no locally bound model"
                    );
                    None
                }
            };
            models.insert(key.clone(), model);
        }
        let model = models.get(&key).cloned().flatten();
        threads.push(caused_thread(session, model));
    }
    Ok(threads)
}

fn caused_thread(session: CausedSession, model: Option<String>) -> CausedThread {
    let latest = &session.latest;
    CausedThread {
        session_id: session.scope.session_id.clone(),
        agent_did: session.scope.agent_did.clone(),
        requester_did: session.scope.requester_did.clone(),
        nickname: session.behavior_id.clone(),
        behavior_id: session.behavior_id.clone(),
        parent_session_id: session.caused_by_scope.session_id.clone(),
        root_session_id: session.root_session_id.clone(),
        depth: session.depth,
        model,
        client_projection: project_persisted_attempt(
            latest
                .lifecycle_state
                .map(|state| state.as_str())
                .unwrap_or(""),
            nonempty(latest.superseded_by_request.as_deref()).is_some(),
        ),
        failure_reason: nonempty(latest.failure_reason.as_deref()).map(ToOwned::to_owned),
        created_at: session.first.created_at.clone(),
        latest_request_id: latest.request_id.clone(),
        latest_request_doc_id: latest.doc_id.clone().unwrap_or_default(),
        latest_request_content: latest.content.clone().unwrap_or_default(),
        latest_request_created_at: latest.created_at.clone(),
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
