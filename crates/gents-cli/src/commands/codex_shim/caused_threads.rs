use gents_protocol::error_chain::ErrorChain;
use rapidhash::{HashMapExt, RapidHashMap};

use anyhow::Result;
use gents_protocol::client_protocol::{project_persisted_attempt, ClientHeadProjection};
use gents_protocol::row::AgentRequestRow;

use super::bound_behavior::load_bound_model_selection_id;
use super::ShimState;

#[derive(Debug, thiserror::Error)]
enum CausedThreadsError {
    #[error("ambiguous Codex thread label across canonical session scopes: {session_id}")]
    AmbiguousLabel { session_id: String },
    #[error("ambiguous Codex root/caused thread label across canonical scopes: {session_id}")]
    AmbiguousRootLabel { session_id: String },
}
use crate::caused_sessions::{
    load_caused_session, load_caused_sessions, load_session_head, CausedSession, SessionScope,
};

pub(super) const CAUSED_THREAD_COLLECTIONS: [&str; 4] = [
    "AgentRequest",
    "Agent",
    "InferenceProfile",
    "InferenceBackend",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CausedThread {
    pub(super) session_id: String,
    pub(super) node_did: String,
    pub(super) requester_did: Option<String>,
    pub(super) agent_id: String,
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

pub(super) async fn load_caused_thread(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<CausedThread>> {
    let roots = super::thread_projection::root_thread_ids(state).await?;
    let Some(session) = load_caused_session(&state.node, thread_id, |scope| {
        scope.node_did == state.node_did.as_ref()
            && scope.requester_did.as_deref() == Some(state.local_requester_did())
            && roots.contains(&scope.session_id)
    })
    .await?
    else {
        return Ok(None);
    };
    let model = bound_model(state, &session.scope.node_did, &session.agent_id).await;
    Ok(Some(caused_thread(session, model)))
}

impl CausedThread {
    pub(super) fn scope(&self) -> SessionScope {
        SessionScope {
            node_did: self.node_did.clone(),
            session_id: self.session_id.clone(),
            requester_did: self.requester_did.clone(),
        }
    }

    pub(super) async fn refresh(&mut self, state: &ShimState) -> Result<()> {
        if let Some(latest) = load_session_head(&state.node, &self.scope()).await? {
            self.apply_latest(&latest);
        }
        Ok(())
    }

    fn apply_latest(&mut self, latest: &AgentRequestRow) {
        self.client_projection = project_persisted_attempt(
            latest
                .lifecycle_state
                .map(|state| state.as_str())
                .unwrap_or(""),
            nonempty(latest.superseded_by_request.as_deref()).is_some(),
        );
        self.failure_reason = nonempty(latest.failure_reason.as_deref()).map(ToOwned::to_owned);
        self.latest_request_id = latest.request_id.clone();
        self.latest_request_doc_id = latest.doc_id.clone().unwrap_or_default();
        self.latest_request_content = latest.content.clone().unwrap_or_default();
        self.latest_request_created_at = latest.created_at.clone();
    }
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
            node_did: state.node_did.to_string(),
            session_id: session_id.clone(),
            requester_did: Some(state.local_requester_did().to_string()),
        })
        .collect::<Vec<_>>();
    let sessions = load_caused_sessions(&state.node, &roots).await?;
    let mut threads = Vec::with_capacity(sessions.len());
    let mut labels = RapidHashMap::<String, SessionScope>::new();
    let mut models = RapidHashMap::<(String, String), Option<String>>::new();
    for session in sessions {
        if let Some(existing) = labels.get(&session.scope.session_id) {
            if existing != &session.scope {
                return Err(CausedThreadsError::AmbiguousLabel {
                    session_id: session.scope.session_id.clone(),
                }
                .into());
            }
            continue;
        }
        if root_session_ids.contains(&session.scope.session_id) {
            if !(session.scope.node_did == state.node_did.as_ref()
                && session.scope.requester_did.as_deref() == Some(state.local_requester_did()))
            {
                return Err(CausedThreadsError::AmbiguousRootLabel {
                    session_id: session.scope.session_id.clone(),
                }
                .into());
            }
            continue;
        }
        labels.insert(session.scope.session_id.clone(), session.scope.clone());
        let key = (session.scope.node_did.clone(), session.agent_id.clone());
        if !models.contains_key(&key) {
            let model = bound_model(state, &key.0, &key.1).await;
            models.insert(key.clone(), model);
        }
        let model = models.get(&key).cloned().flatten();
        threads.push(caused_thread(session, model));
    }
    Ok(threads)
}

async fn bound_model(state: &ShimState, node_did: &str, agent_id: &str) -> Option<String> {
    match load_bound_model_selection_id(&state.node, node_did, agent_id).await {
        Ok(model) => Some(model),
        Err(error) => {
            tracing::debug!(
                error = format!("{}", ErrorChain(error.as_ref())),
                node_did,
                agent_id,
                "Codex caused thread has no locally bound model"
            );
            None
        }
    }
}

fn caused_thread(session: CausedSession, model: Option<String>) -> CausedThread {
    let mut thread = CausedThread {
        session_id: session.scope.session_id.clone(),
        node_did: session.scope.node_did.clone(),
        requester_did: session.scope.requester_did.clone(),
        nickname: session.agent_id.clone(),
        agent_id: session.agent_id.clone(),
        parent_session_id: session.caused_by_scope.session_id.clone(),
        root_session_id: session.root_session_id.clone(),
        depth: session.depth,
        model,
        client_projection: None,
        failure_reason: None,
        created_at: session.first.created_at.clone(),
        latest_request_id: String::new(),
        latest_request_doc_id: String::new(),
        latest_request_content: String::new(),
        latest_request_created_at: None,
    };
    thread.apply_latest(&session.latest);
    thread
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_call_that_started_two_sessions_projects_two_threads() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state =
            super::super::turn_projection::tests::notification_test_state(temp.path()).await;
        crate::caused_sessions::fixtures::seed_one_call_two_sessions(
            &state.node,
            "did:test:notification",
            "parent",
        )
        .await;
        let mut threads = load_caused_threads_for_root_ids(&state, &["parent".to_owned()])
            .await
            .unwrap()
            .into_iter()
            .map(|thread| thread.session_id)
            .collect::<Vec<_>>();
        threads.sort();
        assert_eq!(threads, ["child-a", "child-b"]);
        state.node.shutdown().await;
    }

    #[tokio::test]
    async fn generated_plugin_job_lineage_cases_drive_codex_threads() {
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        for case in snapshot["plugin_job_lineage_cases"].as_array().unwrap() {
            let mut case = case.clone();
            let queries = case["queries"].as_array().unwrap().clone();
            let mut causes: Vec<&str> = queries
                .iter()
                .map(|query| query["cause_request_doc_id"].as_str().unwrap())
                .collect();
            causes.sort();
            causes.dedup();
            // A cause outside the case's rows is the calling request: it opens
            // the calling session, ahead of the rows that name it.
            let rows = case["rows"].as_array_mut().unwrap();
            let mut roots = Vec::new();
            for cause in causes {
                let root = match rows.iter().find(|row| row["_docID"] == cause) {
                    Some(row) => row["session_id"].as_str().unwrap().to_owned(),
                    None => {
                        rows.insert(
                            0,
                            serde_json::json!({
                                "created_at": 1, "_docID": cause, "session_id": "parent",
                                "is_public": true, "caused_by_parent_request_doc_id": null,
                                "caused_by_parent_tool_call_doc_id": null,
                                "opening_request_doc_id": null,
                            }),
                        );
                        "parent".to_owned()
                    }
                };
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
            let temp = tempfile::tempdir().expect("tempdir");
            let state =
                super::super::turn_projection::tests::notification_test_state(temp.path()).await;
            crate::caused_sessions::fixtures::seed_lineage_case(
                &state.node,
                &case,
                "did:test:notification",
            )
            .await;
            let mut actual = load_caused_threads_for_root_ids(&state, &roots)
                .await
                .unwrap()
                .into_iter()
                .map(|thread| thread.session_id)
                .collect::<Vec<_>>();
            actual.sort();
            let mut expected = queries
                .iter()
                .filter(|query| query["expected_opening_doc_id"].is_string())
                .map(|query| query["session_id"].as_str().unwrap().to_owned())
                .filter(|session| !roots.contains(session))
                .collect::<Vec<_>>();
            expected.sort();
            expected.dedup();
            assert_eq!(actual, expected, "{}", case["name"]);
            state.node.shutdown().await;
        }
    }
}
