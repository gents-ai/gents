use serde::Deserialize;
use serde_json::json;

use super::{ConfigClientError, GraphqlEndpoint};

#[derive(Debug, Clone, Copy)]
pub struct DocumentAccessQuestion<'a> {
    pub actor: &'a str,
    pub permission: &'a str,
    pub policy_id: &'a str,
    pub resource_name: &'a str,
    pub doc_id: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentDecideError {
    #[error("{0}")]
    Authorize(#[source] ConfigClientError),
    #[error("requesting DefraDB ACP decision: {0}")]
    Request(#[source] reqwest::Error),
    #[error("reading DefraDB ACP decision response: {0}")]
    ReadBody(#[source] reqwest::Error),
    #[error("DefraDB decision endpoint returned {status}: {body}")]
    Refused {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("decoding DefraDB ACP decision: {0}")]
    Decode(#[source] serde_json::Error),
}

#[derive(Debug, Deserialize)]
struct DecisionResponse {
    allowed: bool,
}

/// Ask DefraDB's `POST {api_base}/acp/document/decide` whether `question.actor`
/// holds `question.permission` on the document. `authorization` is applied
/// to the request when present; callers own the client and its timeout.
pub async fn decide_document_access(
    client: &reqwest::Client,
    api_base: &str,
    authorization: Option<&GraphqlEndpoint>,
    question: &DocumentAccessQuestion<'_>,
) -> Result<bool, DocumentDecideError> {
    let url = format!("{}/acp/document/decide", api_base.trim_end_matches('/'));
    let mut request = client.post(url);
    if let Some(endpoint) = authorization {
        request = endpoint
            .authorize(request)
            .map_err(DocumentDecideError::Authorize)?;
    }
    let response = request
        .json(&json!({
            "actor": question.actor,
            "permission": question.permission,
            "policyID": question.policy_id,
            "resourceName": question.resource_name,
            "docID": question.doc_id,
        }))
        .send()
        .await
        .map_err(DocumentDecideError::Request)?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(DocumentDecideError::ReadBody)?;
    if !status.is_success() {
        return Err(DocumentDecideError::Refused { status, body });
    }
    let decision: DecisionResponse =
        serde_json::from_str(&body).map_err(DocumentDecideError::Decode)?;
    Ok(decision.allowed)
}
