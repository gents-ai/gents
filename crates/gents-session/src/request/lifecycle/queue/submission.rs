use super::*;
use gents_protocol::request_input::QueueDelivery;
use gents_store::config_client::ConfigAccess;

#[derive(Debug, thiserror::Error)]
pub enum UserMessageInputError {
    #[error("node_did is required")]
    NodeRequired,
    #[error("session_id is required")]
    SessionRequired,
    #[error("user messages require an ordinary user queue entry")]
    QueueEntryNotOrdinary,
    #[error("session {session_id} has conflicting active requests")]
    ConflictingActiveRequests { session_id: String },
    #[error("reading the session's active requests")]
    Read(#[source] gents_store::config_client::ConfigClientError),
    #[error("decoding the session's active requests")]
    Decode(#[source] serde_json::Error),
}

pub async fn prepare_user_message_input(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    input: RequestInput,
    delivery: QueueDelivery,
) -> Result<RequestInput> {
    Ok(queue_user_message_input(access, node_did, session_id, input, delivery).await?)
}

pub async fn queue_user_message_input(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    mut input: RequestInput,
    delivery: QueueDelivery,
) -> std::result::Result<RequestInput, UserMessageInputError> {
    if node_did.trim().is_empty() {
        return Err(UserMessageInputError::NodeRequired);
    }
    if session_id.trim().is_empty() {
        return Err(UserMessageInputError::SessionRequired);
    }
    if !input.queue.as_ref().is_none_or(|queue| {
        queue.source == QueueSource::User
            && queue.policy == QueuePolicy::Append
            && queue.key.is_none()
            && queue.position.is_none()
            && queue.interrupted_request_id.is_none()
            && queue.background_completion_wake_version.is_none()
    }) {
        return Err(UserMessageInputError::QueueEntryNotOrdinary);
    }
    let response = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, Filter, Query, Selection,
        };
        let query = Query::select(Selection::doc_id().field(fields::REQUEST_ID))
            .filter(Filter::eq(fields::NODE_DID, node_did))
            .filter(Filter::eq(fields::SESSION_ID, session_id))
            .filter(Filter::eq(fields::PURPOSE, "normal"))
            .filter(Filter::any_of(
                fields::LIFECYCLE_STATE,
                RequestLifecycleState::unsettled_execution().map(RequestLifecycleState::as_str),
            ))
            .render();
        access
            .read(&query)
            .await
            .map_err(UserMessageInputError::Read)?
    };
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())
            .map_err(UserMessageInputError::Decode)?;
    if rows.len() > 1 {
        return Err(UserMessageInputError::ConflictingActiveRequests {
            session_id: session_id.to_owned(),
        });
    }
    input.queue = Some(RequestQueue {
        source: QueueSource::User,
        policy: QueuePolicy::Append,
        delivery,
        position: None,
        key: None,
        queued_after_request_id: rows.first().map(|row| row.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    Ok(input)
}
