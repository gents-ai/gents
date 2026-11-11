use super::*;
use anyhow::Context;
use gents_protocol::request_lifecycle::RequestLifecycleState;

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

#[cfg(test)]
pub(crate) async fn load_history_with_access(
    access: &crate::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<Message>> {
    Ok(super::output::sequenced::load_sequenced_messages_using(
        super::output::access::ReadAccess::Config(access),
        session_id,
        node_did,
        requester_did,
        None,
        None,
        None,
        None,
    )
    .await?
    .into_iter()
    .map(|row| row.message)
    .collect())
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
    node: &std::sync::Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequest,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    provider_profile: crate::turn::provider_input::ProviderInputProfile,
) -> Result<Vec<SequencedMessage>> {
    let access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(request.agent_did.clone())?,
    );
    super::output::sequenced::load_sequenced_messages_using(
        super::output::access::ReadAccess::Config(&access),
        &request.session_id,
        &request.node_did,
        request.requester_did.as_deref(),
        through_sequence,
        after_sequence,
        Some(request.doc_id.as_str()),
        Some(provider_profile),
    )
    .await
}

pub(crate) async fn retry_has_published_input(
    node: &std::sync::Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequest,
) -> Result<bool> {
    let access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(request.agent_did.clone())?,
    );
    let Some(mut parent_id) = request.retry_parent_request_doc_id.clone() else {
        return Ok(false);
    };
    let mut visited = rapidhash::RapidHashSet::default();
    loop {
        anyhow::ensure!(
            visited.insert(parent_id.clone()),
            "retry parent chain contains a cycle"
        );
        let rendered = {
            use gents_protocol::document_query::{
                fields::{
                    agent_message as message_fields, agent_request as request_fields,
                    agent_tool_call as tool_call_fields,
                },
                session_scope, Filter, Query, QuerySet, Selection,
            };
            let key = gents_protocol::output::rows::authored_message_key(&parent_id, "prompt");
            let parent_request = Query::select(
                Selection::doc_id()
                    .field(request_fields::NODE_DID)
                    .field(request_fields::REQUESTER_DID)
                    .field(request_fields::SESSION_ID)
                    .field(request_fields::LIFECYCLE_STATE)
                    .field(request_fields::RETRY_PARENT_REQUEST_DOC_ID),
            )
            .filter(Filter::eq(request_fields::DOC_ID, parent_id.as_str()))
            .limit(2);
            let prompts = Query::select(Selection::doc_id())
                .filter(session_scope(
                    &request.node_did,
                    &request.session_id,
                    request.requester_did.as_deref(),
                ))
                .filter(Filter::eq(message_fields::MESSAGE_KEY, key))
                .limit(2);
            let foreground_tool_calls = Query::select(Selection::doc_id())
                .filter(Filter::eq(
                    tool_call_fields::REQUEST_DOC_ID,
                    parent_id.as_str(),
                ))
                .filter(Filter::any_of(
                    tool_call_fields::LIFECYCLE_STATE,
                    ["pending", "running"],
                ))
                .filter(Filter::any([
                    Filter::eq(tool_call_fields::AWAIT_MODE, "foreground"),
                    Filter::is_null(tool_call_fields::AWAIT_MODE),
                ]))
                .limit(1);
            QuerySet::new()
                .plain(&parent_request)
                .plain(&prompts)
                .plain(&foreground_tool_calls)
                .render()
        };
        let response = access.read(&rendered).await?;
        let rows = response
            .get("data")
            .context("retry frontier query omitted data")?;
        let parents = rows["AgentRequest"]
            .as_array()
            .context("retry parent query omitted rows")?;
        anyhow::ensure!(
            parents.len() == 1
                && parents[0]["node_did"].as_str() == Some(request.node_did.as_str())
                && matches!(
                    RequestLifecycleState::parse_opt(parents[0]["lifecycle_state"].as_str()),
                    Some(RequestLifecycleState::Failed | RequestLifecycleState::Dead)
                ),
            "retry parent must be an exact terminal request in the same agent scope"
        );
        if parents[0]["session_id"].as_str() != Some(request.session_id.as_str()) {
            return Ok(false);
        }
        anyhow::ensure!(
            parents[0]["requester_did"].as_str() == request.requester_did.as_deref(),
            "retry continuation cannot cross requester scope"
        );
        anyhow::ensure!(
            rows["AgentToolCall"].as_array().is_some_and(Vec::is_empty),
            "retry parent still has unsettled foreground tool execution; wait for tool recovery before retrying"
        );
        let prompts = rows["AgentMessage"]
            .as_array()
            .context("retry input query omitted rows")?;
        anyhow::ensure!(
            prompts.len() <= 1,
            "retry parent has ambiguous authored input"
        );
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

pub(crate) async fn load_sequenced_history_projection(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    current_input_request_doc_id: Option<&str>,
) -> Result<Vec<SequencedMessage>> {
    let history = super::output::sequenced::load_sequenced_messages(
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

pub(crate) async fn load_sequenced_history_projection_on(
    access: &crate::store::config_client::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    current_input_request_doc_id: Option<&str>,
) -> Result<Vec<SequencedMessage>> {
    super::output::sequenced::load_sequenced_messages_using(
        super::output::access::ReadAccess::Config(access),
        session_id,
        node_did,
        requester_did,
        through_sequence,
        after_sequence,
        current_input_request_doc_id,
        None,
    )
    .await
}
