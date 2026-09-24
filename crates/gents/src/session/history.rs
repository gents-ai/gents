use super::*;

/// Injective key for a sequence within one canonical session scope.
/// Explicit caller-owned keys keep their own vocabulary (steering, receipts).
pub fn sequence_message_key(
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    sequence: u32,
) -> String {
    format!(
        "message:{}",
        serde_json::to_string(&(agent_did, session_id, requester_did, sequence))
            .expect("session scope serializes")
    )
}

pub async fn load_history(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<Message>> {
    load_history_through_sequence(node, session_id, agent_did, requester_did, None).await
}

pub(crate) async fn load_history_through_sequence(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
) -> Result<Vec<Message>> {
    Ok(load_sequenced_history_projection(
        node,
        session_id,
        agent_did,
        requester_did,
        through_sequence,
        None,
        None,
    )
    .await?
    .into_iter()
    .map(|row| row.message)
    .collect())
}

pub(crate) async fn load_sequenced_history_for_request(
    node: &EmbeddedNode,
    request: &crate::watcher::AgentRequest,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    provider_profile: crate::provider_input::ProviderInputProfile,
) -> Result<Vec<SequencedMessage>> {
    super::output::load_sequenced_messages(
        node,
        &request.session_id,
        &request.agent_did,
        request.requester_did.as_deref(),
        through_sequence,
        after_sequence,
        Some(request.doc_id.as_str()),
        Some(provider_profile),
    )
    .await
}

#[cfg(test)]
pub(super) async fn load_history_projection(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    current_input_request_doc_id: Option<&str>,
) -> Result<Vec<Message>> {
    Ok(load_sequenced_history_projection(
        node,
        session_id,
        agent_did,
        requester_did,
        through_sequence,
        None,
        current_input_request_doc_id,
    )
    .await?
    .into_iter()
    .map(|row| row.message)
    .collect())
}

pub(crate) async fn load_sequenced_history_projection(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    current_input_request_doc_id: Option<&str>,
) -> Result<Vec<SequencedMessage>> {
    let history = super::output::load_sequenced_messages(
        node,
        session_id,
        agent_did,
        requester_did,
        through_sequence,
        after_sequence,
        current_input_request_doc_id,
        None,
    )
    .await?;

    tracing::Span::current().record("history_message_count", history.len() as i64);
    tracing::debug!(session_id = %session_id, ?through_sequence, ?after_sequence, ?current_input_request_doc_id, count = history.len(), "loaded canonical history");
    Ok(history)
}
