use super::*;
use crate::store::config_client::ConfigAccess;
use gents_protocol::request_input::QueueDelivery;

pub async fn prepare_user_message_input(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    mut input: RequestInput,
    delivery: QueueDelivery,
) -> Result<RequestInput> {
    anyhow::ensure!(!node_did.trim().is_empty(), "node_did is required");
    anyhow::ensure!(!session_id.trim().is_empty(), "session_id is required");
    anyhow::ensure!(
        input.queue.as_ref().is_none_or(|queue| {
            queue.source == QueueSource::User
                && queue.policy == QueuePolicy::Append
                && queue.key.is_none()
                && queue.position.is_none()
                && queue.interrupted_request_id.is_none()
                && queue.background_completion_wake_version.is_none()
        }),
        "user messages require an ordinary user queue entry"
    );
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
                [
                    RequestLifecycleState::Claimed,
                    RequestLifecycleState::Processing,
                ]
                .into_iter()
                .map(RequestLifecycleState::as_str),
            ))
            .render();
        access.read(&query).await?
    };
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    anyhow::ensure!(rows.len() <= 1, "session has conflicting active requests");
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
