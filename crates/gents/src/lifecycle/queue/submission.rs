use super::*;
use crate::config_client::ConfigAccess;
use gents_protocol::request_input::QueueDelivery;

/// Resolve the active physical session before signing a user's input. The
/// claim owner remains authoritative if this observation races completion.
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
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
            node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }},
            purpose: {{ _eq: "normal" }},
            lifecycle_state: {{ _in: ["claimed", "processing"] }}
        }}) {{ _docID request_id }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(session_id),
        ))
        .await?;
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
