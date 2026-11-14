use gents_documents::mailbox_write_decl::MailboxAction;
use gents_protocol::document_query::fields::{agent, default_agent_selection, node};
use gents_protocol::document_query::{
    get as field, rows, Collection, Filter, Query, QuerySet, Selection,
};
use gents_protocol::operations::request::{MailboxCauseRefusal, RequestOperationError};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

use crate::request::lifecycle::{ClientSubmissionError, UserMessageInputError};
use crate::session::mailbox::MailboxStatus;

pub(super) fn nonblank(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(super) fn required<'a>(
    name: &'static str,
    value: &'a str,
) -> Result<&'a str, RequestOperationError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(RequestOperationError::FieldRequired(name));
    }
    Ok(value)
}

pub(super) fn row_field(
    name: &'static str,
    value: Option<String>,
) -> Result<String, RequestOperationError> {
    nonblank(value).ok_or(RequestOperationError::FieldRequired(name))
}

pub(super) fn client_actor(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
) -> Result<String, RequestOperationError> {
    access
        .actor_did()
        .filter(|actor| actor == signer.did())
        .ok_or(RequestOperationError::IdentityRequired)
}

pub(super) fn submission_error(
    request_id: &str,
    error: ClientSubmissionError,
) -> RequestOperationError {
    match error {
        ClientSubmissionError::Rejected(source) => RequestOperationError::passthrough(source),
        ClientSubmissionError::Unconfirmed(source) => RequestOperationError::Unconfirmed {
            request_id: request_id.to_owned(),
            source: source.into(),
        },
        ClientSubmissionError::IdentityConflict(source) => {
            RequestOperationError::IdentityConflict {
                request_id: request_id.to_owned(),
                source: source.into(),
            }
        }
    }
}

pub(super) fn user_message_error(error: UserMessageInputError) -> RequestOperationError {
    match error {
        UserMessageInputError::NodeRequired => RequestOperationError::FieldRequired("node_did"),
        UserMessageInputError::SessionRequired => {
            RequestOperationError::FieldRequired("session_id")
        }
        UserMessageInputError::QueueEntryNotOrdinary => RequestOperationError::QueueEntryOwned,
        UserMessageInputError::ConflictingActiveRequests { session_id } => {
            RequestOperationError::ConflictingActiveRequests { session_id }
        }
        UserMessageInputError::Read(source) => RequestOperationError::passthrough(source),
        UserMessageInputError::Decode(source) => RequestOperationError::passthrough(source),
    }
}

pub(super) async fn resolve_agent(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requested: Option<&str>,
) -> Result<String, RequestOperationError> {
    let session_agent = access
        .transact_readonly("operations.request.session_agent", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn, node_did, session_id,
                    )
                    .await?
                    .map(|row| row.session.agent_did),
                )
            })
        })
        .await
        .map_err(RequestOperationError::passthrough)?;
    let requested = requested.map(str::trim).filter(|value| !value.is_empty());
    if let (Some(existing), Some(requested)) = (session_agent.as_deref(), requested) {
        if existing != requested {
            return Err(RequestOperationError::SessionAgentMismatch {
                session_id: session_id.to_owned(),
                session_agent: existing.to_owned(),
                requested: requested.to_owned(),
            });
        }
    }
    let wanted = session_agent.or_else(|| requested.map(ToOwned::to_owned));
    let nodes = Query::select(Selection::new().field(node::NODE_DID).field(node::ENABLED))
        .filter(Filter::eq(node::NODE_DID, node_did))
        .limit(2);
    let default_agent =
        Query::select(Selection::new().field(default_agent_selection::DEFAULT_AGENT_DID))
            .filter(Filter::eq(default_agent_selection::NODE_DID, node_did))
            .limit(1);
    let agents = Query::select(
        Selection::new()
            .field(agent::AGENT_DID)
            .field(agent::NODE_DID)
            .field(agent::ENABLED),
    )
    .filter(Filter::eq(agent::NODE_DID, node_did));
    let rendered = QuerySet::new()
        .plain(&nodes)
        .plain(&default_agent)
        .plain(&agents)
        .render();
    let response = access
        .read(&rendered)
        .await
        .map_err(RequestOperationError::passthrough)?;
    let node_rows = rows(&response, node::Doc::NAME);
    if node_rows.len() != 1 || field(&node_rows[0], node::ENABLED) != Some(true) {
        return Err(RequestOperationError::NodeUnavailable {
            node_did: node_did.to_owned(),
        });
    }
    let agent_did = match wanted {
        Some(agent_did) => agent_did,
        None => rows(&response, default_agent_selection::Doc::NAME)
            .first()
            .and_then(|row| field(row, default_agent_selection::DEFAULT_AGENT_DID))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| RequestOperationError::NoDefaultAgent {
                node_did: node_did.to_owned(),
            })?,
    };
    let matching = rows(&response, agent::Doc::NAME)
        .iter()
        .filter(|row| field(row, agent::AGENT_DID) == Some(agent_did.as_str()))
        .collect::<Vec<_>>();
    if matching.len() != 1 || field(matching[0], agent::ENABLED) != Some(true) {
        return Err(RequestOperationError::AgentUnavailable {
            node_did: node_did.to_owned(),
            agent_did,
        });
    }
    Ok(agent_did)
}

pub(super) async fn validate_mailbox_cause(
    access: &ConfigAccess,
    item_id: &str,
    requester_did: &str,
    node_did: &str,
    agent_did: &str,
    session_id: &str,
) -> Result<(), RequestOperationError> {
    let refuse = |refusal| {
        Err(RequestOperationError::MailboxCause {
            item_id: item_id.to_owned(),
            refusal,
        })
    };
    let Some(item) = crate::session::mailbox::load_mailbox_item_with_access(access, item_id)
        .await
        .map_err(RequestOperationError::passthrough)?
    else {
        return refuse(MailboxCauseRefusal::NotOneItem);
    };
    if item.parsed_status() != Some(MailboxStatus::Open) {
        return refuse(MailboxCauseRefusal::Closed);
    }
    if !matches!(
        item.parsed_action(),
        Some(MailboxAction::StartRequest | MailboxAction::WriteDocument)
    ) {
        return refuse(MailboxCauseRefusal::NotCompose);
    }
    if item.target_node_did != node_did || item.target_agent_did != agent_did {
        return refuse(MailboxCauseRefusal::OtherAgent);
    }
    if let Some(expected_session) = item
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if item.requester_did != requester_did {
            return refuse(MailboxCauseRefusal::OtherRequester);
        }
        if expected_session != session_id {
            return refuse(MailboxCauseRefusal::OtherSession);
        }
    }
    Ok(())
}
