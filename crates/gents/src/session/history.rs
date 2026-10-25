use super::*;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error(transparent)]
    Output(#[from] super::output::SessionOutputError),
    #[error(transparent)]
    Config(#[from] crate::config_client::ConfigClientError),
    #[error("retry parent chain contains a cycle")]
    RetryParentCycle,
    #[error("retry frontier query omitted data")]
    RetryFrontierDataOmitted,
    #[error("retry parent query omitted rows")]
    RetryParentRowsOmitted,
    #[error("retry parent must be an exact terminal request in the same agent scope")]
    RetryParentNotTerminal,
    #[error("retry continuation cannot cross requester scope")]
    RetryRequesterScope,
    #[error(
        "retry parent still has unsettled foreground tool execution; wait for tool recovery before retrying"
    )]
    RetryToolUnsettled,
    #[error("retry input query omitted rows")]
    RetryInputRowsOmitted,
    #[error("retry parent has ambiguous authored input")]
    RetryAmbiguousInput,
}

impl crate::config_client::TransactionFailure for HistoryError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            Self::Output(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            _ => None,
        }
    }
}

type Result<T, E = HistoryError> = std::result::Result<T, E>;

pub fn sequence_message_key(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    sequence: u32,
) -> String {
    format!(
        "message:{}",
        serde_json::to_string(&(node_did, session_id, requester_did, sequence))
            .expect("session scope serializes")
    )
}

pub async fn load_history(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<Message>> {
    load_history_through_sequence(node, session_id, node_did, requester_did, None).await
}

pub(crate) async fn load_history_through_sequence(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
) -> Result<Vec<Message>> {
    Ok(load_sequenced_history_projection(
        node,
        session_id,
        node_did,
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
        &request.node_did,
        request.requester_did.as_deref(),
        through_sequence,
        after_sequence,
        Some(request.doc_id.as_str()),
        Some(provider_profile),
    )
    .await
    .map_err(HistoryError::from)
}

pub(crate) async fn retry_has_published_input(
    node: &EmbeddedNode,
    request: &crate::watcher::AgentRequest,
) -> Result<bool> {
    let Some(mut parent_id) = request.retry_parent_request_doc_id.clone() else {
        return Ok(false);
    };
    let mut visited = rapidhash::RapidHashSet::default();
    loop {
        if !visited.insert(parent_id.clone()) {
            return Err(HistoryError::RetryParentCycle);
        }
        let parent = crate::graphql::escape_graphql_string(&parent_id);
        let scope = super::query::session_scope_filter(
            &request.node_did,
            &request.session_id,
            request.requester_did.as_deref(),
        );
        let key = crate::graphql::escape_graphql_string(
            &super::canonical_rows::authored_message_key(&parent_id, "prompt"),
        );
        let query = format!(
            r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{parent}" }} }}, limit: 2) {{
                _docID node_did requester_did session_id lifecycle_state retry_parent_request_doc_id
            }}
            AgentMessage(filter: {{ {scope}, message_key: {{ _eq: "{key}" }} }}, limit: 2) {{ _docID }}
            AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{parent}" }},
                lifecycle_state: {{ _in: ["pending", "running"] }},
                _or: [{{ await_mode: {{ _eq: "foreground" }} }}, {{ await_mode: {{ _eq: null }} }}] }}, limit: 1) {{ _docID }}
        }}"#
        );
        let response =
            crate::graphql::graphql_with_transaction_retry(node, &query, "resolve retry frontier")
                .await?;
        let rows = response
            .data
            .as_ref()
            .ok_or(HistoryError::RetryFrontierDataOmitted)?;
        let parents = rows["AgentRequest"]
            .as_array()
            .ok_or(HistoryError::RetryParentRowsOmitted)?;
        if !(parents.len() == 1
            && parents[0]["node_did"].as_str() == Some(request.node_did.as_str())
            && matches!(
                parents[0]["lifecycle_state"].as_str(),
                Some("failed" | "dead")
            ))
        {
            return Err(HistoryError::RetryParentNotTerminal);
        }
        if parents[0]["session_id"].as_str() != Some(request.session_id.as_str()) {
            return Ok(false);
        }
        if parents[0]["requester_did"].as_str() != request.requester_did.as_deref() {
            return Err(HistoryError::RetryRequesterScope);
        }
        if !rows["AgentToolCall"].as_array().is_some_and(Vec::is_empty) {
            return Err(HistoryError::RetryToolUnsettled);
        }
        let prompts = rows["AgentMessage"]
            .as_array()
            .ok_or(HistoryError::RetryInputRowsOmitted)?;
        if prompts.len() > 1 {
            return Err(HistoryError::RetryAmbiguousInput);
        }
        if !prompts.is_empty() {
            return Ok(true);
        }
        match parents[0]["retry_parent_request_doc_id"]
            .as_str()
            .filter(|id| !id.is_empty())
        {
            Some(id) => parent_id = id.to_owned(),
            None => return Ok(false),
        }
    }
}

#[cfg(test)]
pub(super) async fn load_history_projection(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    current_input_request_doc_id: Option<&str>,
) -> Result<Vec<Message>> {
    Ok(load_sequenced_history_projection(
        node,
        session_id,
        node_did,
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
    node_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    current_input_request_doc_id: Option<&str>,
) -> Result<Vec<SequencedMessage>> {
    let history = super::output::load_sequenced_messages(
        node,
        session_id,
        node_did,
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
