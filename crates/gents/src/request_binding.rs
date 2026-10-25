//! Every caller shares this limit-two lookup so missing and ambiguous labels
//! cannot become half-bound writes.

use defra_node::EmbeddedNode;
use gents_protocol::row::AgentRequestRow;

use crate::config_client::ConfigClientError;
use crate::graphql::{
    escape_graphql_string, graphql_with_transaction_retry, rows, GraphqlResponseError,
};
use crate::watcher::{AgentRequest, WatcherError};

#[derive(Debug, thiserror::Error)]
pub(crate) enum RequestBindingError {
    #[error(transparent)]
    Config(#[from] ConfigClientError),
    #[error(transparent)]
    Rows(#[from] GraphqlResponseError),
    #[error(transparent)]
    Watcher(#[from] WatcherError),
    #[error("AgentRequest request_id={request_id} has no _docID")]
    MissingDocId { request_id: String },
    #[error("AgentRequest request_id={request_id} is ambiguous across {count} documents")]
    Ambiguous { request_id: String, count: usize },
    #[error("AgentRequest request_id={request_id} not found")]
    NotFound { request_id: String },
    #[error(
        "AgentRequest {request_doc_id} changed logical request binding while loading {request_id}"
    )]
    LogicalBindingChanged {
        request_doc_id: String,
        request_id: String,
    },
    #[error("AgentRequest physical binding changed while loading {request_doc_id}")]
    PhysicalBindingChanged { request_doc_id: String },
}

type Result<T, E = RequestBindingError> = std::result::Result<T, E>;

pub(crate) async fn resolve_request_doc_id(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<String>> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 2
            ) {{ _docID request_id }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "resolve_agent_request_document_binding")
            .await?;
    let mut matches = rows::<AgentRequestRow>(&response, "AgentRequest")?;
    match matches.len() {
        0 => Ok(None),
        1 => matches
            .pop()
            .and_then(|row| row.doc_id)
            .map(Some)
            .ok_or_else(|| RequestBindingError::MissingDocId {
                request_id: request_id.to_owned(),
            }),
        count => Err(RequestBindingError::Ambiguous {
            request_id: request_id.to_owned(),
            count,
        }),
    }
}

pub(crate) async fn require_request_doc_id(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<String> {
    resolve_request_doc_id(node, request_id)
        .await?
        .ok_or_else(|| RequestBindingError::NotFound {
            request_id: request_id.to_owned(),
        })
}

pub(crate) async fn load_agent_request(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<AgentRequest>> {
    let Some(request_doc_id) = resolve_request_doc_id(node, request_id).await? else {
        return Ok(None);
    };
    let request = load_agent_request_by_doc_id(node, &request_doc_id).await?;
    if let Some(request) = request.as_ref() {
        if request.request_id != request_id {
            return Err(RequestBindingError::LogicalBindingChanged {
                request_doc_id: request_doc_id.clone(),
                request_id: request_id.to_owned(),
            });
        }
    }
    Ok(request)
}

pub(crate) async fn load_agent_request_by_doc_id(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<Option<AgentRequest>> {
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                _docID
                request_id
                purpose
                node_did
                requester_did
                agent_id
                session_id
                content
                max_total_tokens
                input
                execution_origin
                created_at
                deadline
                request_hop
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
                caused_by_trigger_id
                caused_by_trigger_kind
                caused_by_source_doc_id
                caused_by_correlation
                caused_by_trigger_context
                workspace_id
                workspace_authority
                workspace_owner_node_did
                workspace_seal_hash
            }}
        }}"#,
        escape_graphql_string(request_doc_id)
    );
    let response =
        graphql_with_transaction_retry(node, &query, "load_agent_request_by_document").await?;
    let mut matches = rows::<AgentRequestRow>(&response, "AgentRequest")?;
    let Some(row) = matches.pop() else {
        return Ok(None);
    };
    if row.doc_id.as_deref() != Some(request_doc_id) {
        return Err(RequestBindingError::PhysicalBindingChanged {
            request_doc_id: request_doc_id.to_owned(),
        });
    }
    Ok(Some(AgentRequest::try_from(row)?))
}
