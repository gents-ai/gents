use std::path::{Path, PathBuf};

use anyhow::Result;
use gents::config_client::{ConfigAccess, ConfigApplyTxn};
use gents::graphql::escape_graphql_string;
use gents::session::{
    decode_session_row, load_agent_session_row_in_txn, load_latest_request_in_txn,
    session_scope_filter, AGENT_SESSION_FIELDS,
};
use gents_protocol::graphql::GraphqlTurnState;
use gents_protocol::session::AgentSession;
use serde_json::Value;

use super::CodexShimProjectionError;
use crate::commands::codex_shim::ShimState;

fn rows<'a>(
    response: &'a Value,
    collection: &str,
) -> std::result::Result<&'a Vec<Value>, CodexShimProjectionError> {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .ok_or_else(|| CodexShimProjectionError::ThreadRowsMissing {
            collection: collection.to_owned(),
        })
}

pub(super) async fn load_scoped_session(
    state: &ShimState,
    session_id: &str,
) -> Result<Option<AgentSession>> {
    Ok(load_thread_state(state, session_id)
        .await?
        .map(|(session, _)| session))
}

fn is_started(session: &AgentSession) -> bool {
    session
        .provenance
        .as_ref()
        .is_some_and(|provenance| provenance.parent_request_doc_id.is_some())
}

pub(super) async fn load_thread_state(
    state: &ShimState,
    session_id: &str,
) -> Result<Option<(AgentSession, Option<GraphqlTurnState>)>> {
    ConfigAccess::transact_local_write(&state.node, None, "codex.thread.read", |txn| {
        Box::pin(async move {
            let Some(row) = load_agent_session_row_in_txn(
                txn,
                &state.node_did,
                session_id,
                Some(state.local_requester_did()),
            )
            .await?
            else {
                return Ok(None);
            };
            if row.session.agent_id != state.agent_id.as_ref() || is_started(&row.session) {
                return Ok(None);
            }
            let head = load_head_in_txn(txn, state, session_id).await?;
            Ok(Some((row.session, head)))
        })
    })
    .await
}

pub(super) async fn load_local_head(
    state: &ShimState,
    session_id: &str,
) -> Result<Option<GraphqlTurnState>> {
    ConfigAccess::transact_local_write(&state.node, None, "codex.thread.pending_head", |txn| {
        Box::pin(async move { load_head_in_txn(txn, state, session_id).await })
    })
    .await
}

async fn load_head_in_txn(
    txn: &ConfigApplyTxn<'_>,
    state: &ShimState,
    session_id: &str,
) -> Result<Option<GraphqlTurnState>> {
    let Some(head) = load_latest_request_in_txn(
        txn,
        &state.node_did,
        session_id,
        Some(Some(state.local_requester_did())),
    )
    .await?
    else {
        return Ok(None);
    };
    if head.agent_id != state.agent_id.as_ref() {
        return Err(CodexShimProjectionError::ThreadHeadConflict.into());
    }
    let doc_id = escape_graphql_string(&head.observed.request_doc_id);
    let scope = session_scope_filter(
        &state.node_did,
        session_id,
        Some(state.local_requester_did()),
    );
    let response = txn
        .execute(&format!(
            r#"{{
        AgentRequest(filter:{{{scope},_docID:{{_eq:"{doc_id}"}}}}) {{
            _docID request_id node_did requester_did session_id agent_id created_at lifecycle_state
            retry_parent_request superseded_by_request failure_reason content input
        }}
    }}"#
        ))
        .await?;
    let requests = rows(&response, "AgentRequest")?;
    if requests.len() != 1 {
        return Err(CodexShimProjectionError::ThreadHeadAmbiguous.into());
    }
    let request = serde_json::from_value(requests[0].clone())
        .map_err(|source| CodexShimProjectionError::DecodeThreadHead { source })?;
    Ok(Some(GraphqlTurnState {
        request: Some(request),
    }))
}

pub(super) async fn list_scoped_sessions(
    state: &ShimState,
) -> Result<Vec<AgentSession>, CodexShimProjectionError> {
    let owner = escape_graphql_string(&state.node_did);
    let agent = escape_graphql_string(&state.agent_id);
    ConfigAccess::transact_local_write(&state.node,None,"codex.thread.list",|txn| {
        let query = format!(r#"{{AgentSession(filter:{{node_did:{{_eq:"{owner}"}},requester_did:{{_eq:"{owner}"}},agent_id:{{_eq:"{agent}"}}}},order:{{created_at:DESC}}){{{AGENT_SESSION_FIELDS}}}}}"#);
        Box::pin(async move {
            let response = txn.execute(&query).await?;
            let mut identities = rapidhash::RapidHashSet::default();
            rows(&response,"AgentSession")?.iter().map(|row| {
                let session = decode_session_row(row)?.session;
                if session.node_did != state.node_did.as_ref() || session.requester_did.as_deref() != Some(state.local_requester_did()) || session.agent_id != state.agent_id.as_ref() {
                    return Err(CodexShimProjectionError::ThreadListScopeCrossed);
                }
                if !identities.insert(session.session_id.clone()) {
                    return Err(CodexShimProjectionError::ThreadListDuplicateIdentity);
                }
                Ok(session)
            }).filter(|session| !matches!(session, Ok(session) if is_started(session))).collect()
        })
    }).await
}

pub(super) async fn derive_thread_cwd(state: &ShimState, thread_id: &str) -> Result<PathBuf> {
    if let Some(cwd) = state.thread_cwd_override(thread_id).await {
        return Ok(cwd);
    }
    if let Some(head) = load_local_head(state, thread_id).await? {
        if let Some(cwd) = head
            .request
            .and_then(|request| request.input)
            .and_then(|input| input.cwd)
        {
            return Ok(absolute_cwd(&state.cwd, Path::new(&cwd)));
        }
    }
    if let Some(cwd) = settings_json_cwd(&state.cwd, &state.thread_settings(thread_id).await) {
        return Ok(cwd);
    }
    Ok(state.cwd.clone())
}

fn settings_json_cwd(base_cwd: &Path, settings_json: &str) -> Option<PathBuf> {
    serde_json::from_str::<Value>(settings_json)
        .ok()?
        .get("cwd")?
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
