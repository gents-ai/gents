use super::*;
use crate::graphql::CURSOR_PAGE_SIZE;
use gents_protocol::session_hydration::SessionHydrationCollection;

pub(super) const PAGE_SIZE: usize = CURSOR_PAGE_SIZE;

fn fields(collection: SessionHydrationCollection) -> &'static str {
    match collection {
        SessionHydrationCollection::AgentRequest => {
            "_docID request_id requester_did node_did session_id lifecycle_state terminal_output"
        }
        SessionHydrationCollection::AgentMessage => AGENT_MESSAGE_FIELDS,
        SessionHydrationCollection::AgentToolCall => HYDRATION_TOOL_CALL_FIELDS,
        SessionHydrationCollection::AgentOutputSegment => AGENT_OUTPUT_SEGMENT_FIELDS,
        SessionHydrationCollection::CompactionEntry => "_docID requester_did node_did session_id",
    }
}

#[cfg(test)]
pub(super) fn page_query(
    collection: SessionHydrationCollection,
    filter: &str,
    after: Option<&str>,
) -> String {
    crate::graphql::cursor_query(
        hydration_collection_name(collection),
        fields(collection),
        filter,
        after,
    )
}

pub(super) async fn load_rows(
    txn: &ConfigApplyTxn<'_>,
    collection: SessionHydrationCollection,
    filter: &str,
) -> Result<Vec<serde_json::Value>> {
    txn.cursor_rows_local(
        hydration_collection_name(collection),
        fields(collection),
        filter,
    )
    .await
}

pub(super) async fn load_session_rows(
    txn: &ConfigApplyTxn<'_>,
    request: &HydrationRequest,
) -> Result<defra_node::QueryResponse> {
    let filter = crate::session::session_scope_filter(
        &request.node_did,
        &request.session_id,
        Some(&request.requester_did),
    );
    let mut data = serde_json::Map::new();
    for collection in [
        SessionHydrationCollection::AgentRequest,
        SessionHydrationCollection::AgentMessage,
        SessionHydrationCollection::AgentToolCall,
        SessionHydrationCollection::AgentOutputSegment,
        SessionHydrationCollection::CompactionEntry,
    ] {
        let rows = load_rows(txn, collection, &filter).await?;
        data.insert(
            hydration_collection_name(collection).to_owned(),
            serde_json::Value::Array(rows),
        );
    }
    Ok(defra_node::QueryResponse::success(
        serde_json::Value::Object(data),
    ))
}
