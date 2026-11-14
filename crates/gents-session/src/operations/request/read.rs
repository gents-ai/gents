use gents_protocol::document_query::fields::agent_request::{self as fields, Doc};
use gents_protocol::document_query::{
    get as field, rows, session_scope, Collection, Filter, Query, Rendered, Selection,
};
use gents_protocol::operations::request::{RequestOperationError, RequestReceipt};
use gents_protocol::request_admission::{AgentRequestCreate, SIGNED_REQUEST_FIELDS};
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;

#[derive(Debug, thiserror::Error)]
#[error("request id {0} is already held on this node")]
struct RequestIdHeld(String);

pub(super) fn request_selection() -> Selection<Doc> {
    Selection::owned(SIGNED_REQUEST_FIELDS)
        .field(fields::FAILURE_REASON)
        .field(fields::TERMINAL_OUTPUT)
        .field(fields::TERMINALIZED_AT)
        .field(fields::TERMINAL_REDRIVE_ATTEMPTS)
        .field(fields::SUPERSEDED_BY_REQUEST)
        .field(fields::SUPERSEDED_BY_REQUEST_DOC_ID)
        .field(fields::INTERRUPT_REQUESTED_AT)
        .field(fields::CLAIMED_AT)
}

pub(super) async fn read_rows(
    access: &ConfigAccess,
    query: &Rendered,
) -> Result<Vec<AgentRequestRow>, RequestOperationError> {
    let response = access
        .read(query)
        .await
        .map_err(RequestOperationError::passthrough)?;
    rows(&response, Doc::NAME)
        .iter()
        .map(|row| serde_json::from_value(row.clone()).map_err(RequestOperationError::passthrough))
        .collect()
}

pub(super) fn scoped(node_did: &str, requester_did: Option<&str>, query: Query<Doc>) -> Query<Doc> {
    let query = query.filter(Filter::eq(fields::NODE_DID, node_did));
    match requester_did {
        Some(requester) => query.filter(Filter::eq(fields::REQUESTER_DID, requester)),
        None => query,
    }
}

pub(super) async fn requests_by_id(
    access: &ConfigAccess,
    node_did: &str,
    requester_did: Option<&str>,
    request_id: &str,
) -> Result<Vec<AgentRequestRow>, RequestOperationError> {
    let query = scoped(
        node_did,
        requester_did,
        Query::select(request_selection()).filter(Filter::eq(fields::REQUEST_ID, request_id)),
    )
    .limit(2)
    .render();
    read_rows(access, &query).await
}

pub(super) async fn receipt(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: &str,
    request_id: &str,
    signed: Option<&AgentRequestCreate>,
) -> Result<RequestReceipt, RequestOperationError> {
    let query = Query::select(Selection::owned(SIGNED_REQUEST_FIELDS))
        .filter(session_scope(node_did, session_id, Some(requester_did)))
        .filter(Filter::eq(fields::REQUEST_ID, request_id))
        .limit(2)
        .render();
    let mut found = read_rows(access, &query).await?;
    if found.len() > 1 {
        return Err(RequestOperationError::Ambiguous {
            node_did: node_did.to_owned(),
            request_id: request_id.to_owned(),
        });
    }
    let row = found.pop().ok_or_else(|| RequestOperationError::NotFound {
        node_did: node_did.to_owned(),
        request_id: request_id.to_owned(),
    })?;
    let mismatch = |reason| RequestOperationError::ReceiptMismatch {
        request_id: request_id.to_owned(),
        reason,
    };
    if row.request_id != request_id
        || row.node_did.as_deref() != Some(node_did)
        || row.session_id.as_deref() != Some(session_id)
        || row.requester_did.as_deref() != Some(requester_did)
    {
        return Err(mismatch(
            "it is outside the send's node, session or requester",
        ));
    }
    gents_store::identity::request_receipt::verify_request_receipt_signature(&row)
        .map_err(RequestOperationError::passthrough)?;
    if let Some(create) = signed {
        if row.admission_signer_did.as_deref() != Some(create.admission.signer_did.as_str()) {
            return Err(mismatch("another identity signed it"));
        }
    }
    let request_doc_id = row
        .doc_id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| mismatch("it has no document identity"))?;
    Ok(RequestReceipt {
        request_id: row.request_id,
        request_doc_id,
        session_id: session_id.to_owned(),
        node_did: node_did.to_owned(),
        requester_did: row.requester_did,
        agent_did: row.agent_did,
        input: row.input,
    })
}

pub(super) async fn latest_session_request(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: &str,
) -> Result<Option<String>, RequestOperationError> {
    let query = Query::select(
        Selection::new()
            .field(fields::REQUEST_ID)
            .field(fields::CREATED_AT),
    )
    .filter(session_scope(node_did, session_id, Some(requester_did)))
    .render();
    let response = access
        .read(&query)
        .await
        .map_err(RequestOperationError::passthrough)?;
    Ok(rows(&response, Doc::NAME)
        .iter()
        .filter_map(|row| {
            Some((
                field(row, fields::CREATED_AT).unwrap_or_default(),
                field(row, fields::REQUEST_ID)?,
            ))
        })
        .max()
        .map(|(_, request_id)| request_id.to_owned()))
}

pub(super) async fn ensure_request_id_fresh(
    access: &ConfigAccess,
    node_did: &str,
    request_id: &str,
) -> Result<(), RequestOperationError> {
    let query = Query::select(Selection::doc_id())
        .filter(Filter::eq(fields::NODE_DID, node_did))
        .filter(Filter::eq(fields::REQUEST_ID, request_id))
        .limit(1)
        .render();
    let response = access
        .read(&query)
        .await
        .map_err(RequestOperationError::passthrough)?;
    if rows(&response, Doc::NAME).is_empty() {
        return Ok(());
    }
    Err(RequestOperationError::IdentityConflict {
        request_id: request_id.to_owned(),
        source: Box::new(RequestIdHeld(request_id.to_owned())),
    })
}
