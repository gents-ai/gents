//! Every caller shares this limit-two lookup so missing and ambiguous labels
//! cannot become half-bound writes.

use anyhow::Result;
use defra_node::Node as EmbeddedNode;
use gents_protocol::row::AgentRequestRow;

use gents_protocol::row::AgentRequest;
use gents_store::graphql::{read_rendered, rows};

pub async fn load_agent_request_by_doc_id_with_access(
    access: &gents_store::config_client::ConfigAccess,
    request_doc_id: &str,
) -> Result<Option<AgentRequest>> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let query = Query::select(Selection::owned(gents_protocol::row::AGENT_REQUEST_FIELDS))
        .filter(Filter::eq(fields::DOC_ID, request_doc_id))
        .limit(2)
        .render();
    let response = access.read(&query).await?;
    let mut matches: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    anyhow::ensure!(
        matches.len() <= 1,
        "AgentRequest physical binding is ambiguous"
    );
    let Some(row) = matches.pop() else {
        return Ok(None);
    };
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(request_doc_id),
        "AgentRequest physical binding changed"
    );
    Ok(Some(AgentRequest::try_from(row)?))
}

pub async fn load_agent_request_with_access(
    access: &gents_store::config_client::ConfigAccess,
    request_id: &str,
) -> Result<Option<AgentRequest>> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let query = Query::select(Selection::owned(gents_protocol::row::AGENT_REQUEST_FIELDS))
        .filter(Filter::eq(fields::REQUEST_ID, request_id))
        .limit(2)
        .render();
    let response = access.read(&query).await?;
    let mut matches: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    anyhow::ensure!(
        matches.len() <= 1,
        "AgentRequest logical binding is ambiguous"
    );
    let Some(row) = matches.pop() else {
        return Ok(None);
    };
    let request = AgentRequest::try_from(row)?;
    anyhow::ensure!(
        request.request_id == request_id,
        "AgentRequest logical binding changed"
    );
    Ok(Some(request))
}

pub async fn resolve_request_doc_id(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<String>> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let query = Query::select(Selection::doc_id().field(fields::REQUEST_ID))
        .filter(Filter::eq(fields::REQUEST_ID, request_id))
        .limit(2)
        .render();
    let response = read_rendered(node, &query, "resolve_agent_request_document_binding").await?;
    let mut matches = rows::<AgentRequestRow>(&response, "AgentRequest")?;
    match matches.len() {
        0 => Ok(None),
        1 => matches
            .pop()
            .and_then(|row| row.doc_id)
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("AgentRequest request_id={request_id} has no _docID")),
        count => anyhow::bail!(
            "AgentRequest request_id={request_id} is ambiguous across {count} documents"
        ),
    }
}

pub(crate) async fn require_request_doc_id(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<String> {
    resolve_request_doc_id(node, request_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("AgentRequest request_id={request_id} not found"))
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn load_agent_request(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<AgentRequest>> {
    let Some(request_doc_id) = resolve_request_doc_id(node, request_id).await? else {
        return Ok(None);
    };
    let request = load_agent_request_by_doc_id(node, &request_doc_id).await?;
    if let Some(request) = request.as_ref() {
        anyhow::ensure!(
            request.request_id == request_id,
            "AgentRequest {request_doc_id} changed logical request binding while loading {request_id}"
        );
    }
    Ok(request)
}

/// Reads without an actor, which ACP hides Agent-owned requests from; runtime
/// readers use [`load_agent_request_by_doc_id_with_access`] as the owner.
#[cfg(any(test, feature = "test-fixtures"))]
pub async fn load_agent_request_by_doc_id(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<Option<AgentRequest>> {
    use gents_protocol::document_query::{
        fields::agent_request as fields, Filter, Query, Selection,
    };
    let query = Query::select(Selection::owned(
        "_docID request_id purpose node_did requester_did agent_did session_id content max_total_tokens input execution_origin created_at deadline request_hop caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id caused_by_trigger_id caused_by_trigger_kind caused_by_source_doc_id caused_by_correlation caused_by_trigger_context workspace_id workspace_authority workspace_owner_node_did workspace_seal_hash",
    ))
    .filter(Filter::eq(fields::DOC_ID, request_doc_id))
    .limit(1)
    .render();
    let response = read_rendered(node, &query, "load_agent_request_by_document").await?;
    let mut matches = rows::<AgentRequestRow>(&response, "AgentRequest")?;
    let Some(row) = matches.pop() else {
        return Ok(None);
    };
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(request_doc_id),
        "AgentRequest physical binding changed while loading {request_doc_id}"
    );
    Ok(Some(AgentRequest::try_from(row)?))
}
