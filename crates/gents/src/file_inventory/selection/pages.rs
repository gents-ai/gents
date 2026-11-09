use super::*;
use crate::file_inventory::SessionCollection;
use crate::store::graphql::CURSOR_PAGE_SIZE;

pub(super) const PAGE_SIZE: usize = CURSOR_PAGE_SIZE;

fn fields(collection: SessionCollection) -> &'static str {
    match collection {
        SessionCollection::AgentRequest => {
            "_docID request_id requester_did node_did session_id lifecycle_state terminal_output"
        }
        SessionCollection::AgentMessage => AGENT_MESSAGE_FIELDS,
        SessionCollection::AgentToolCall => SESSION_TOOL_CALL_FIELDS,
        SessionCollection::AgentOutputSegment => AGENT_OUTPUT_SEGMENT_FIELDS,
        SessionCollection::CompactionEntry => "_docID requester_did node_did session_id",
    }
}

#[cfg(test)]
pub(super) fn page_query(
    collection: SessionCollection,
    filter: &str,
    after: Option<&str>,
) -> String {
    crate::store::graphql::cursor_query(
        session_collection_name(collection),
        fields(collection),
        filter,
        after,
    )
}

pub(super) async fn load_rows(
    txn: &ConfigApplyTxn<'_>,
    collection: SessionCollection,
    filter: &str,
) -> Result<Vec<serde_json::Value>> {
    Ok(txn
        .cursor_rows(
            session_collection_name(collection),
            fields(collection),
            filter,
        )
        .await?)
}

/// Additional scope fields beside `session_id` make DefraDB choose the node
/// index and scan the node's whole segment history. The equivalent nested
/// conjunction keeps this bounded cursor on the session index.
pub(super) fn session_filter(
    collection: SessionCollection,
    scope: &SessionScope,
) -> Result<String> {
    let requester = scope
        .requester_did
        .as_deref()
        .filter(|value| !value.is_empty())
        .context("canonical page scope omitted requester")?;
    Ok(if collection == SessionCollection::AgentOutputSegment {
        format!(
            r#"session_id: {{ _eq: "{}" }}, _or: [{{ node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }} }}]"#,
            escape_graphql_string(&scope.session_id),
            escape_graphql_string(&scope.node_did),
            escape_graphql_string(requester)
        )
    } else {
        gents_protocol::session::session_scope_filter(
            &scope.node_did,
            &scope.session_id,
            Some(requester),
        )
    })
}

pub(super) async fn load_session_rows(
    txn: &ConfigApplyTxn<'_>,
    scope: &SessionScope,
) -> Result<defra_node::QueryResponse> {
    let mut data = serde_json::Map::new();
    for collection in [
        SessionCollection::AgentRequest,
        SessionCollection::AgentMessage,
        SessionCollection::AgentToolCall,
        SessionCollection::AgentOutputSegment,
        SessionCollection::CompactionEntry,
    ] {
        let rows = load_rows(txn, collection, &session_filter(collection, scope)?).await?;
        data.insert(
            session_collection_name(collection).to_owned(),
            serde_json::Value::Array(rows),
        );
    }
    Ok(defra_node::QueryResponse::success(
        serde_json::Value::Object(data),
    ))
}
