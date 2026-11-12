use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::operations::session::{
    SessionArchiveInput, SessionEntry, SessionForkBoundary, SessionForkCopied, SessionForkInput,
    SessionForkReceipt, SessionListInput, SessionOperationError, SessionRef, SessionRenameInput,
    SessionRenameReceipt, SessionResumeReceipt, SessionState, SessionStopReceipt,
};
use gents_protocol::session::{AgentSession, SessionArchiveReceipt, SessionScope};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;
use rapidhash::{HashMapExt, RapidHashMap};

use crate::session::{
    fork_edit_on, fork_on, rename_session_on, resume_session_with_access, set_session_archived_on,
    stop_session_with_access, ForkBoundary, ForkError, ForkParams, SessionControlError,
    SessionExecutionWitness,
};

fn required(field: &'static str, value: &str) -> Result<(), SessionOperationError> {
    if value.trim().is_empty() {
        return Err(SessionOperationError::FieldRequired(field));
    }
    Ok(())
}

fn require_ref(reference: &SessionRef) -> Result<(), SessionOperationError> {
    required("node_did", &reference.node_did)?;
    required("session_id", &reference.session_id)
}

async fn read_sessions(
    access: &ConfigAccess,
    input: &SessionListInput,
) -> Result<Vec<AgentSession>, SessionOperationError> {
    access
        .read_view(&crate::views::SessionList {
            input: input.clone(),
        })
        .await
        .map(|rows| rows.into_iter().map(|row| row.session).collect())
        .map_err(SessionOperationError::passthrough)
}

async fn request_counts(
    access: &ConfigAccess,
    sessions: &[AgentSession],
) -> Result<RapidHashMap<SessionScope, u64>, SessionOperationError> {
    let mut counts = RapidHashMap::new();
    if sessions.is_empty() {
        return Ok(counts);
    }
    let view = crate::views::SessionRequestScopes {
        scopes: sessions.iter().map(SessionScope::from).collect(),
    };
    for row in access
        .read_view(&view)
        .await
        .map_err(SessionOperationError::passthrough)?
    {
        *counts.entry(row.scope()).or_insert(0) += 1;
    }
    Ok(counts)
}

pub async fn list(
    access: &ConfigAccess,
    input: &SessionListInput,
) -> Result<Vec<SessionEntry>, SessionOperationError> {
    let sessions = read_sessions(access, input).await?;
    let counts = request_counts(access, &sessions).await?;
    let entries: Vec<SessionEntry> = sessions
        .into_iter()
        .map(|session| SessionEntry {
            request_count: counts
                .get(&SessionScope::from(&session))
                .copied()
                .unwrap_or_default(),
            state: SessionState::of(&session),
            session,
        })
        .collect();
    Ok(entries)
}

fn exactly_one<T>(mut found: Vec<T>, reference: &SessionRef) -> Result<T, SessionOperationError> {
    match found.len() {
        0 => Err(SessionOperationError::NotFound {
            node_did: reference.node_did.clone(),
            session_id: reference.session_id.clone(),
        }),
        1 => Ok(found.remove(0)),
        _ => Err(SessionOperationError::Ambiguous {
            node_did: reference.node_did.clone(),
            session_id: reference.session_id.clone(),
        }),
    }
}

fn selector(reference: &SessionRef) -> SessionListInput {
    SessionListInput {
        node_did: Some(reference.node_did.clone()),
        requester_did: reference.requester_did.clone(),
        session_id: Some(reference.session_id.clone()),
        include_archived: true,
    }
}

async fn resolve(
    access: &ConfigAccess,
    reference: &SessionRef,
) -> Result<AgentSession, SessionOperationError> {
    require_ref(reference)?;
    exactly_one(
        read_sessions(access, &selector(reference)).await?,
        reference,
    )
}

pub async fn get(
    access: &ConfigAccess,
    reference: &SessionRef,
) -> Result<SessionEntry, SessionOperationError> {
    require_ref(reference)?;
    exactly_one(list(access, &selector(reference)).await?, reference)
}

pub async fn rename(
    access: &ConfigAccess,
    input: &SessionRenameInput,
) -> Result<SessionRenameReceipt, SessionOperationError> {
    let title = input.title.trim();
    required("title", title)?;
    let found = resolve(access, &input.session).await?;
    let requester =
        found
            .requester_did
            .as_deref()
            .ok_or_else(|| SessionOperationError::RequesterMissing {
                session_id: found.session_id.clone(),
            })?;
    rename_session_on(access, &found.node_did, requester, &found.session_id, title)
        .await
        .map_err(SessionOperationError::passthrough)?;
    Ok(SessionRenameReceipt {
        node_did: found.node_did,
        session_id: found.session_id,
        title: title.to_owned(),
    })
}

pub async fn set_archived(
    access: &ConfigAccess,
    actor: &dyn NodeIdentity,
    input: &SessionArchiveInput,
) -> Result<SessionArchiveReceipt, SessionOperationError> {
    let found = resolve(access, &input.session).await?;
    let expected_revision = input.expected_revision.unwrap_or(found.archive_revision);
    if i64::try_from(expected_revision).is_err() {
        return Err(SessionOperationError::RevisionOutOfRange {
            revision: expected_revision,
        });
    }
    set_session_archived_on(
        access,
        &found.node_did,
        &found.session_id,
        actor,
        expected_revision,
        input.archived,
    )
    .await
    .map_err(SessionOperationError::passthrough)
}

fn fork_failure(error: ForkError, found: &AgentSession) -> SessionOperationError {
    match error {
        ForkError::ForkSourceNotFound(_) => SessionOperationError::NotFound {
            node_did: found.node_did.clone(),
            session_id: found.session_id.clone(),
        },
        ForkError::ForkNotSameAgent => SessionOperationError::RequesterMismatch {
            session_id: found.session_id.clone(),
        },
        ForkError::ForkSourceBusy => SessionOperationError::ForkSourceBusy {
            session_id: found.session_id.clone(),
        },
        ForkError::ForkAtUserTurnOutOfRange(at_user_turn, user_messages) => {
            SessionOperationError::ForkTurnOutOfRange {
                session_id: found.session_id.clone(),
                at_user_turn,
                user_messages,
            }
        }
        ForkError::ForkMessageNotEditable(message_key) => {
            SessionOperationError::ForkMessageNotEditable {
                message_key,
                reason: "it is not a user message of this session".to_owned(),
            }
        }
        ForkError::ForkAgentNotFound(agent_did) => {
            SessionOperationError::AgentUnavailable { agent_did }
        }
        ForkError::CrossAgentForkUnavailable => SessionOperationError::CrossAgentForkUnavailable,
        ForkError::ForkRelationshipIncomplete {
            session_id, source, ..
        } => SessionOperationError::ForkRelationshipsIncomplete {
            session_id,
            source: source.into(),
        },
        ForkError::ForkRecoveryIntentRequired(session_id) => {
            SessionOperationError::ForkRecoveryIntentRequired { session_id }
        }
        ForkError::ForkCopyFailed(source) => SessionOperationError::passthrough(source),
    }
}

pub async fn fork(
    access: &ConfigAccess,
    input: &SessionForkInput,
) -> Result<SessionForkReceipt, SessionOperationError> {
    let found = resolve(access, &input.source).await?;
    let params = |boundary| ForkParams {
        source_session_id: &found.session_id,
        boundary,
        caller_node_did: &found.node_did,
        caller_requester_did: found.requester_did.as_deref(),
        target_agent_did: input.target_agent_did.as_deref(),
    };
    let (session_id, copied) = match &input.boundary {
        SessionForkBoundary::UserTurn(at_user_turn) => {
            let outcome = fork_on(access, params(ForkBoundary::UserTurn(*at_user_turn)))
                .await
                .map_err(|error| fork_failure(error, &found))?;
            (
                outcome.session_id,
                Some(SessionForkCopied {
                    messages: outcome.copied_messages,
                    tool_calls: outcome.copied_tool_calls,
                    tool_results: outcome.copied_tool_results,
                    compaction_entries: outcome.copied_compaction_entries,
                }),
            )
        }
        SessionForkBoundary::BeforeMessage {
            message_key,
            child_session_id,
            client_did,
        } => {
            required("message_key", message_key)?;
            if child_session_id.trim().is_empty() || child_session_id != child_session_id.trim() {
                return Err(SessionOperationError::ForkChildNotCanonical {
                    child_session_id: child_session_id.clone(),
                });
            }
            if client_did.trim().is_empty() || access.actor_did().as_deref() != Some(client_did) {
                return Err(SessionOperationError::ForkClientMismatch {
                    client_did: client_did.clone(),
                });
            }
            let child = fork_edit_on(
                access,
                params(ForkBoundary::BeforeMessageKey(message_key)),
                child_session_id,
                client_did,
            )
            .await
            .map_err(|error| fork_failure(error, &found))?;
            (child, None)
        }
    };
    Ok(SessionForkReceipt {
        node_did: found.node_did,
        source_session_id: found.session_id,
        session_id,
        copied,
    })
}

fn control_failure(error: SessionControlError, found: &AgentSession) -> SessionOperationError {
    match error {
        SessionControlError::IdentityRequired => SessionOperationError::IdentityRequired,
        SessionControlError::ActorUnavailable | SessionControlError::ManagedSignerRequired => {
            SessionOperationError::IdentityRequired
        }
        SessionControlError::RemoteNode { node_did } => {
            SessionOperationError::RemoteNodeUnsupported { node_did }
        }
        SessionControlError::SessionNotFound { session_id } => SessionOperationError::NotFound {
            node_did: found.node_did.clone(),
            session_id,
        },
        SessionControlError::ResumeBlocked {
            session_id,
            blocker,
        } => SessionOperationError::ResumeBlocked {
            session_id,
            blocker,
        },
        SessionControlError::ProcessOwnershipUnverifiable { session_id, source } => {
            SessionOperationError::ProcessOwnershipUnverifiable {
                session_id,
                source: source.into(),
            }
        }
        SessionControlError::Access(source) => SessionOperationError::passthrough(source),
        SessionControlError::Failed(source) => SessionOperationError::passthrough(source),
    }
}

pub async fn stop(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    reference: &SessionRef,
) -> Result<SessionStopReceipt, SessionOperationError> {
    let found = resolve(access, reference).await?;
    stop_session_with_access(
        node,
        access,
        &found.node_did,
        &found.session_id,
        found.requester_did.as_deref(),
    )
    .await
    .map_err(|error| control_failure(error, &found))
}

pub async fn resume(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    reference: &SessionRef,
    executions: Arc<dyn SessionExecutionWitness>,
) -> Result<SessionResumeReceipt, SessionOperationError> {
    let found = resolve(access, reference).await?;
    resume_session_with_access(
        node,
        access,
        &found.node_did,
        &found.session_id,
        found.requester_did.as_deref(),
        executions,
    )
    .await
    .map_err(|error| control_failure(error, &found))?;
    Ok(SessionResumeReceipt {
        node_did: found.node_did,
        session_id: found.session_id,
    })
}
