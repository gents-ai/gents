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
        let parent = crate::store::graphql::escape_graphql_string(&parent_id);
        let scope = gents_protocol::session::session_scope_filter(
            &request.node_did,
            &request.session_id,
            request.requester_did.as_deref(),
        );
        let key = crate::store::graphql::escape_graphql_string(
            &gents_protocol::output::rows::authored_message_key(&parent_id, "prompt"),
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
        let response = access.execute(&query).await?;
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
