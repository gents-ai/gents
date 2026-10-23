use super::*;
use crate::graphql::{cursor_after_arg, cursor_page};
use gents_protocol::session_hydration::SessionHydrationCollection;

pub(super) const PAGE_SIZE: usize = 64;

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

pub(super) fn page_query(
    collection: SessionHydrationCollection,
    filter: &str,
    after: Option<&str>,
) -> String {
    let name = hydration_collection_name(collection);
    let fields = fields(collection);
    let after = cursor_after_arg(after);
    format!(
        r#"{{ _cursor {{ {name}(first: {PAGE_SIZE}{after},
        filter: {{ {filter} }}, order: [{{ _docID: ASC }}]) {{ {fields} }}
        _pageInfo {{ hasNext endCursor }} }} }}"#
    )
}

/// The database cursor, not a short page, establishes enumeration completion.
/// All pages retain the caller's read snapshot and document-read identity.
pub(super) async fn load_rows(
    txn: &ConfigApplyTxn<'_>,
    collection: SessionHydrationCollection,
    filter: &str,
) -> Result<Vec<serde_json::Value>> {
    let name = hydration_collection_name(collection);
    let mut after = None;
    let mut cursors = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut rows = Vec::new();
    loop {
        let response = txn
            .execute_local_response(&page_query(collection, filter, after.as_deref()))
            .await?;
        let (page, next) = cursor_page::<serde_json::Value>(&response, name)?;
        anyhow::ensure!(
            page.len() <= PAGE_SIZE,
            "hydration page exceeded its native row bound"
        );
        for row in page {
            let id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .context("hydration cursor row omitted physical identity")?;
            anyhow::ensure!(
                ids.insert(id.to_owned()),
                "hydration cursor repeated a physical document"
            );
            rows.push(row);
        }
        let Some(next) = next else { break };
        anyhow::ensure!(
            !next.is_empty() && cursors.insert(next.clone()),
            "hydration cursor did not advance"
        );
        after = Some(next);
    }
    Ok(rows)
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
