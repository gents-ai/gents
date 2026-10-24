//! Receipt reconciliation for owner-authored chat submissions.
//!
//! A submitted request is committed with a stable, signed identity. When the
//! create response is ambiguous the owner must read that identity back before
//! it may repost, and it must never re-sign: the retained signed payload is
//! replayed byte-for-byte. Everything in this module therefore treats the
//! mutation text and the admission signature as opaque, non-loggable data.

use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents::config_client::ConfigAccess;
use gents_protocol::graphql::{escape_graphql_string, GraphqlRequestOptions};
use gents_protocol::request_admission::AgentRequestCreate;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

use super::{ChatMutationError, SubmittedRequest};

pub(crate) mod journal;

/// A prepared owner submission whose commit outcome could not be determined.
///
/// The original signed create payload and its rendered mutation are retained so
/// a later reconciliation can replay the exact same bytes. Neither is exposed
/// through `Debug`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedSubmission {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub(crate) create: AgentRequestCreate,
    pub(crate) mutation: String,
    pub result: SubmittedRequest,
}

impl std::fmt::Debug for UnresolvedSubmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnresolvedSubmission")
            .field("request_id", &self.request_id)
            .field("session_id", &self.session_id)
            .field("node_did", &self.node_did)
            .finish_non_exhaustive()
    }
}

/// The owner could not prove whether its signed request reached the node.
///
/// The request may already be durable, so callers must reconcile rather than
/// resend: resending a freshly signed request would duplicate the turn.
#[derive(Debug, thiserror::Error)]
#[error("submission outcome unknown for request {request_id} in session {session_id}: it may have been delivered; check the session before resending")]
pub struct SubmissionOutcomeUnknown {
    pub request_id: String,
    pub session_id: String,
    pub pending: UnresolvedSubmission,
}

/// An earlier send whose delivery cannot be decided without its signed
/// payload (for example after a restart).
///
/// Carries identity only. An absent owner receipt does not prove the original
/// will never commit, because that write may still be in flight, so callers
/// must keep this identity unresolved instead of allowing a fresh send.
#[derive(Debug, thiserror::Error)]
#[error("delivery of request {request_id} in session {session_id} is unconfirmed: it may still be delivered; check the session before resending")]
pub struct DeliveryUnconfirmed {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
}

/// Error context attached to every receipt read, so an unreadable receipt is
/// recognisable after `write_with_receipt` wraps it.
const RECEIPT_READ_CONTEXT: &str = "reading owner signed submission receipt";

#[derive(Debug, thiserror::Error)]
pub(super) enum ReceiptReadError {
    #[error("reading owner signed submission receipt")]
    Unreadable(#[source] Box<ReceiptReadError>),
    #[error(transparent)]
    Query(#[from] gents::config_client::ConfigClientError),
    #[error("request receipt query omitted rows")]
    RowsOmitted,
    #[error("ambiguous scoped submission receipt")]
    Ambiguous,
    #[error("decoding submission receipt row")]
    DecodeRow(#[source] serde_json::Error),
    #[error("submission receipt crossed exact request scope")]
    CrossedScope,
    #[error("submission receipt differs from prepared signed request")]
    SignatureMismatch,
}

impl UnresolvedSubmission {
    pub(crate) fn accepts_composer_content(&self, content: &str) -> bool {
        if content.trim().is_empty() {
            return true;
        }
        let selected = gents::skills::prompt_slash_skill_selection(content.trim());
        selected.prompt.trim() == self.create.content.trim()
            && (selected.selected_skill_ids.is_empty()
                || selected.selected_skill_ids == self.create.input.selected_skill_ids)
    }

    /// Retain a prepared signed create and the identity it will produce.
    pub(crate) fn new(
        create: AgentRequestCreate,
        result: SubmittedRequest,
    ) -> Result<Self, ChatMutationError> {
        let mutation = create
            .graphql_mutation()
            .map_err(|reason| ChatMutationError::CreateMutation { reason })?;
        Ok(Self {
            request_id: create.request_id.clone(),
            session_id: create.session_id.clone(),
            node_did: create.node_did.clone(),
            create,
            mutation,
            result,
        })
    }

    fn outcome_unknown(&self) -> anyhow::Error {
        tracing::warn!(
            action = "chat_submit_receipt_unresolved",
            request_id = %self.request_id,
            session_id = %self.session_id,
            node_did = %self.node_did,
            "owner chat submission outcome unknown; retaining signed payload for identical replay"
        );
        SubmissionOutcomeUnknown {
            request_id: self.request_id.clone(),
            session_id: self.session_id.clone(),
            pending: self.clone(),
        }
        .into()
    }
}

/// Read the durable signed receipt for exactly this prepared request.
///
/// Returns `Ok(true)` only when the scoped row matches the request identity and
/// the prepared admission signature; a row that matches the identity but not
/// the signature is an error, never an absence. Non-GraphQL access has no
/// remote owner to reconcile against, so it reports absence.
pub(super) async fn owner_receipt(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
) -> Result<bool, ReceiptReadError> {
    read_owner_receipt(access, create)
        .await
        .map_err(|error| ReceiptReadError::Unreadable(Box::new(error)))
}

async fn read_owner_receipt(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
) -> Result<bool, ReceiptReadError> {
    let ConfigAccess::Graphql(endpoint) = access else {
        return Ok(false);
    };
    let scope = gents::session::session_scope_filter(
        &create.node_did,
        &create.session_id,
        (!create.requester_did.is_empty()).then_some(create.requester_did.as_str()),
    );
    let logical = escape_graphql_string(&create.request_id);
    let response = gents::config_client::query_graphql_with_options(
        endpoint,
        &format!(
            "{{AgentRequest(filter:{{{scope},request_id:{{_eq:\"{logical}\"}}}}){{{}}}}}",
            gents::SIGNED_REQUEST_FIELDS
        ),
        GraphqlRequestOptions {
            timeout: Duration::from_secs(30),
            max_attempts: 1,
            retry_backoff: Duration::ZERO,
        },
    )
    .await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .ok_or(ReceiptReadError::RowsOmitted)?;
    if rows.len() > 1 {
        return Err(ReceiptReadError::Ambiguous);
    }
    let Some(value) = rows.first() else {
        return Ok(false);
    };
    let row: AgentRequestRow =
        serde_json::from_value(value.clone()).map_err(ReceiptReadError::DecodeRow)?;
    if !(row.request_id == create.request_id
        && row.node_did.as_deref() == Some(create.node_did.as_str())
        && row.session_id.as_deref() == Some(create.session_id.as_str())
        && row.requester_did.as_deref()
            == (!create.requester_did.is_empty()).then_some(create.requester_did.as_str()))
    {
        return Err(ReceiptReadError::CrossedScope);
    }
    let signature = bs58::encode(&create.admission.signature).into_string();
    if !(row.admission_signer_did.as_deref() == Some(create.admission.signer_did.as_str())
        && row.admission_signature.as_deref() == Some(signature.as_str()))
    {
        return Err(ReceiptReadError::SignatureMismatch);
    }
    Ok(true)
}

/// Commit a retained signed submission, reconciling ambiguous outcomes.
///
/// Embedded (local) writes are atomic, so their errors are definite. An owner
/// write is only a definite rejection when the owner answered with a GraphQL
/// `errors` body and the receipt proves it holds no such request; every other
/// post-send failure (HTTP status, transport, undecodable body, unreadable
/// receipt, or an "already exists" rejection with no visible receipt) returns
/// [`SubmissionOutcomeUnknown`], because the owner may hold or still be
/// committing the request.
pub(super) async fn commit_signed_submission(
    access: &ConfigAccess,
    pending: UnresolvedSubmission,
) -> Result<SubmittedRequest> {
    let written = access
        .write_with_receipt("desktop.chat.submit", &pending.mutation, || {
            owner_receipt(access, &pending.create)
        })
        .await
        .map_err(anyhow::Error::from);
    let Err(error) = written else {
        return Ok(pending.result);
    };
    if !matches!(access, ConfigAccess::Graphql(_)) {
        return Err(error);
    }
    let receipt_unreadable = error
        .chain()
        .any(|cause| cause.to_string() == RECEIPT_READ_CONTEXT);
    if !receipt_unreadable && owner_rejected(&error) {
        match owner_receipt(access, &pending.create).await {
            Ok(true) => return Ok(pending.result),
            Ok(false) if !reports_existing_request(&error) => return Err(error),
            _ => {}
        }
    }
    Err(pending.outcome_unknown())
}

/// The owner processed the mutation and answered with GraphQL errors.
fn owner_rejected(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().starts_with("graphql returned errors:"))
}

/// A rejection that implies the owner already holds a request with this id.
fn reports_existing_request(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string().to_ascii_lowercase();
        text.contains("already exists") || text.contains("duplicate")
    })
}

/// Reconcile an earlier unknown send whose signed payload is no longer retained.
///
/// A visible owner request with this identity proves delivery. Absence, or an
/// unreadable lookup, proves nothing: the original write may still be in
/// flight. Both return [`DeliveryUnconfirmed`] so the identity stays
/// unresolved and no fresh request is minted.
pub async fn reconcile_unretained_submission(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    request_id: &str,
) -> Result<SubmittedRequest> {
    match submitted_request_exists(access, node_did, session_id, request_id).await {
        Ok(true) => Ok(SubmittedRequest {
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
            agent_id: None,
        }),
        Ok(false) | Err(_) => Err(DeliveryUnconfirmed {
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
        }
        .into()),
    }
}

/// Whether the owner currently shows a request with this identity in its scope.
async fn submitted_request_exists(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    request_id: &str,
) -> Result<bool> {
    let ConfigAccess::Graphql(endpoint) = access else {
        anyhow::bail!("only an owner connection can prove whether a send was delivered");
    };
    // Scoped by node and session only: the requester authority may have changed
    // since the send, and an exact requester match would then prove a false
    // absence.
    let scope = format!(
        r#"node_did:{{_eq:"{}"}},session_id:{{_eq:"{}"}}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id)
    );
    let logical = escape_graphql_string(request_id);
    let response = gents::config_client::query_graphql_with_options(
        endpoint,
        &format!(
            "{{AgentRequest(filter:{{{scope},request_id:{{_eq:\"{logical}\"}}}}){{request_id}}}}"
        ),
        GraphqlRequestOptions {
            timeout: Duration::from_secs(30),
            max_attempts: 1,
            retry_backoff: Duration::ZERO,
        },
    )
    .await
    .context(RECEIPT_READ_CONTEXT)?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("request receipt query omitted rows")?;
    ensure!(rows.len() <= 1, "ambiguous scoped submission receipt");
    Ok(!rows.is_empty())
}

/// Resolve an earlier unknown submission outcome without re-preparing it.
///
/// The retained signed payload is never re-signed: reconciliation first reads
/// the owner receipt, and only replays the identical mutation when the receipt
/// proves absence. A still-unknown outcome returns [`SubmissionOutcomeUnknown`]
/// again so the caller can retry later.
pub async fn resolve_unresolved_submission(
    access: &ConfigAccess,
    pending: &UnresolvedSubmission,
) -> Result<SubmittedRequest> {
    match owner_receipt(access, &pending.create).await {
        Ok(true) => Ok(pending.result.clone()),
        Ok(false) => commit_signed_submission(access, pending.clone()).await,
        Err(_) => Err(pending.outcome_unknown()),
    }
}

/// Endpoint helper kept private to the tests: a GraphQL access at `url`.
#[cfg(test)]
fn test_access(url: &str) -> ConfigAccess {
    ConfigAccess::Graphql(gents::config_client::GraphqlEndpoint::anonymous(url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use defra_node::EmbeddedNode;
    use gents_protocol::error_chain::ErrorChain;
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SIGNER_DID: &str = "did:key:z6MkownerReceiptTest";
    const REQUEST_ID: &str = "request-receipt-1";
    const SESSION_ID: &str = "session-receipt-1";
    const AGENT_ID: &str = "agent-receipt-1";

    fn test_create() -> AgentRequestCreate {
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            REQUEST_ID,
            SIGNER_DID,
            SIGNER_DID,
            AGENT_ID,
            SESSION_ID,
            "receipt reconciliation",
            "interactive",
            "2026-01-01T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(SIGNER_DID),
        );
        create.admission.signature = vec![7u8; 64];
        create
    }

    fn test_mutation(create: &AgentRequestCreate) -> String {
        create
            .graphql_mutation()
            .expect("test create renders a canonical mutation")
    }

    fn test_result() -> SubmittedRequest {
        SubmittedRequest {
            request_id: REQUEST_ID.to_string(),
            session_id: SESSION_ID.to_string(),
            node_did: SIGNER_DID.to_string(),
            agent_id: Some(AGENT_ID.to_string()),
        }
    }

    fn test_pending() -> UnresolvedSubmission {
        let create = test_create();
        let mutation = test_mutation(&create);
        UnresolvedSubmission {
            request_id: create.request_id.clone(),
            session_id: create.session_id.clone(),
            node_did: create.node_did.clone(),
            create,
            mutation,
            result: test_result(),
        }
    }

    #[test]
    fn slash_skill_reconciliation_matches_the_original_prepared_request() {
        let mut create = test_create();
        create.content = "check foo".into();
        create.input.selected_skill_ids = vec!["review".into()];
        let pending = UnresolvedSubmission::new(create, test_result()).unwrap();
        assert!(pending.accepts_composer_content("/skill review check foo"));
        assert!(!pending.accepts_composer_content("/skill other check foo"));
        assert!(!pending.accepts_composer_content("/skill review check something else"));
    }

    #[tokio::test]
    async fn retained_submission_survives_reopen_and_replays_exact_bytes() {
        let home = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            EmbeddedNode::builder()
                .data_path(home.path())
                .build()
                .await
                .unwrap(),
        );
        node.add_schema(gents_protocol::schemas::PENDING_CHAT_SUBMISSION)
            .await
            .unwrap();
        let pending = test_pending();
        journal::save(&node, &pending).await.unwrap();
        node.shutdown().await;
        drop(node);

        let reopened = std::sync::Arc::new(
            EmbeddedNode::builder()
                .data_path(home.path())
                .build()
                .await
                .unwrap(),
        );
        let restored = journal::load(&reopened, REQUEST_ID).await.unwrap().unwrap();
        assert_eq!(restored.mutation, pending.mutation);
        assert_eq!(restored.create, pending.create);
        let server = MockServer::start().await;
        mount_receipt(&server, Vec::new()).await;
        mount_create(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({
                "data": {"create_AgentRequest": [{"request_id": REQUEST_ID}]}
            })),
        )
        .await;
        let result = resolve_unresolved_submission(
            &test_access(&format!("{}/api/v0/graphql", server.uri())),
            &restored,
        )
        .await
        .unwrap();
        assert_eq!(result, pending.result);
        let requests = create_requests(&server).await;
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["query"], pending.mutation);
        journal::forget(&reopened, REQUEST_ID).await;
        assert!(journal::load(&reopened, REQUEST_ID)
            .await
            .unwrap()
            .is_none());
        reopened.shutdown().await;
    }

    fn receipt_row(create: &AgentRequestCreate) -> Value {
        json!({
            "request_id": create.request_id.as_str(),
            "node_did": create.node_did.as_str(),
            "session_id": create.session_id.as_str(),
            "requester_did": create.requester_did.as_str(),
            "admission_signer_did": create.admission.signer_did.as_str(),
            "admission_signature": bs58::encode(&create.admission.signature).into_string(),
        })
    }

    async fn mount_receipt(server: &MockServer, rows: Vec<Value>) {
        Mock::given(method("POST"))
            .and(path("/api/v0/graphql"))
            .and(body_string_contains("AgentRequest(filter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"AgentRequest": rows}
            })))
            .mount(server)
            .await;
    }

    async fn mount_create(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/api/v0/graphql"))
            .and(body_string_contains("create_AgentRequest"))
            .respond_with(response)
            .mount(server)
            .await;
    }

    async fn create_requests(server: &MockServer) -> Vec<wiremock::Request> {
        server
            .received_requests()
            .await
            .expect("wiremock records requests")
            .into_iter()
            .filter(|request| {
                String::from_utf8_lossy(&request.body).contains("create_AgentRequest")
            })
            .collect()
    }

    #[tokio::test]
    async fn accepted_then_response_lost_returns_original_identity() {
        let server = MockServer::start().await;
        let create = test_create();
        // The owner committed, then the response body was lost in transit.
        mount_create(
            &server,
            ResponseTemplate::new(200).set_body_string("truncated response body"),
        )
        .await;
        mount_receipt(&server, vec![receipt_row(&create)]).await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));

        let submitted = commit_signed_submission(&access, test_pending())
            .await
            .expect("a confirmed receipt recovers the ambiguous commit");

        assert_eq!(submitted, test_result());
        assert_eq!(
            create_requests(&server).await.len(),
            1,
            "a confirmed receipt must not repost the create"
        );
        let pending = test_pending();
        let resolved = resolve_unresolved_submission(&access, &pending)
            .await
            .expect("resolving against a matching receipt succeeds");
        assert_eq!(resolved, test_result());
        assert_eq!(create_requests(&server).await.len(), 1);
    }

    #[tokio::test]
    async fn unresolved_outcome_carries_identity_without_payload() {
        let server = MockServer::start().await;
        mount_create(
            &server,
            ResponseTemplate::new(200).set_body_string("truncated response body"),
        )
        .await;
        mount_receipt(&server, Vec::new()).await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));
        let pending = test_pending();

        let error = resolve_unresolved_submission(&access, &pending)
            .await
            .expect_err("an absent receipt for every attempt is an unknown outcome");
        let unknown = error
            .downcast_ref::<SubmissionOutcomeUnknown>()
            .expect("unknown outcome is a typed error");
        assert_eq!(unknown.request_id, REQUEST_ID);
        assert_eq!(unknown.session_id, SESSION_ID);
        assert_eq!(unknown.pending.request_id, REQUEST_ID);

        let signature = bs58::encode(&pending.create.admission.signature).into_string();
        let display = format!("{error}");
        let debug = format!("{pending:?}");
        for text in [&display, &debug] {
            assert!(!text.contains(&pending.mutation), "payload leaked: {text}");
            assert!(
                !text.contains("create_AgentRequest"),
                "payload leaked: {text}"
            );
            assert!(!text.contains(&signature), "signature leaked: {text}");
        }
        assert!(display.contains(REQUEST_ID) && display.contains(SESSION_ID));
    }

    #[tokio::test]
    async fn resolve_after_accept_sends_no_create() {
        let server = MockServer::start().await;
        let create = test_create();
        mount_receipt(&server, vec![receipt_row(&create)]).await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));
        let pending = test_pending();

        let resolved = resolve_unresolved_submission(&access, &pending)
            .await
            .expect("a matching receipt resolves the submission");

        assert_eq!(resolved.request_id, pending.result.request_id);
        assert_eq!(resolved, pending.result);
        assert!(
            create_requests(&server).await.is_empty(),
            "a resolved submission must never repost the create"
        );
    }

    #[tokio::test]
    async fn resolve_replays_identical_signed_mutation() {
        let server = MockServer::start().await;
        mount_receipt(&server, Vec::new()).await;
        mount_create(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({
                "data": {"create_AgentRequest": [{"_docID": "doc-receipt-1"}]}
            })),
        )
        .await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));
        let pending = test_pending();

        let resolved = resolve_unresolved_submission(&access, &pending)
            .await
            .expect("a proven-absent receipt allows one identical replay");

        assert_eq!(resolved, pending.result);
        let creates = create_requests(&server).await;
        assert_eq!(creates.len(), 1, "exactly one replay");
        let body: Value =
            serde_json::from_slice(&creates[0].body).expect("create body is a JSON envelope");
        assert_eq!(
            body.get("query").and_then(Value::as_str),
            Some(pending.mutation.as_str()),
            "the replay must be the retained signed mutation byte-for-byte"
        );
    }

    #[tokio::test]
    async fn definite_owner_rejection_stays_an_ordinary_error() {
        let server = MockServer::start().await;
        mount_create(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({
                "errors": [{"message": "request admission rejected"}]
            })),
        )
        .await;
        mount_receipt(&server, Vec::new()).await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));

        let error = commit_signed_submission(&access, test_pending())
            .await
            .expect_err("a definite rejection with no receipt fails");

        assert!(
            error.downcast_ref::<SubmissionOutcomeUnknown>().is_none(),
            "a definite rejection must not be reported as possibly delivered: {}",
            ErrorChain(error.as_ref())
        );
        assert_eq!(create_requests(&server).await.len(), 1);
    }

    #[tokio::test]
    async fn rejected_repost_after_earlier_commit_recovers_identity() {
        let server = MockServer::start().await;
        let create = test_create();
        mount_create(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({
                "errors": [{"message": "document already exists"}]
            })),
        )
        .await;
        mount_receipt(&server, vec![receipt_row(&create)]).await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));

        let submitted = commit_signed_submission(&access, test_pending())
            .await
            .expect("a matching receipt proves the earlier commit");

        assert_eq!(submitted, test_result());
    }

    #[tokio::test]
    async fn owner_http_status_failure_after_send_is_unknown_not_rejected() {
        for status in [502, 504] {
            let server = MockServer::start().await;
            mount_create(&server, ResponseTemplate::new(status)).await;
            mount_receipt(&server, Vec::new()).await;
            let access = test_access(&format!("{}/api/v0/graphql", server.uri()));

            let error = commit_signed_submission(&access, test_pending())
                .await
                .expect_err("an unconfirmed post-send failure is not a success");

            let unknown = error
                .downcast_ref::<SubmissionOutcomeUnknown>()
                .unwrap_or_else(|| {
                    panic!(
                        "HTTP {status} must be an unknown outcome: {}",
                        ErrorChain(error.as_ref())
                    )
                });
            assert_eq!(unknown.request_id, test_result().request_id);
        }
    }

    #[tokio::test]
    async fn already_exists_rejection_without_visible_receipt_is_unknown() {
        let server = MockServer::start().await;
        mount_create(
            &server,
            ResponseTemplate::new(200).set_body_json(json!({
                "errors": [{"message": "document already exists"}]
            })),
        )
        .await;
        mount_receipt(&server, Vec::new()).await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));

        let error = commit_signed_submission(&access, test_pending())
            .await
            .expect_err("an unconfirmed duplicate rejection is not a success");

        assert!(
            error.downcast_ref::<SubmissionOutcomeUnknown>().is_some(),
            "an existing-request rejection with no visible receipt may still be delivered: {}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn unretained_absent_receipt_stays_unknown_and_sends_nothing() {
        let server = MockServer::start().await;
        mount_receipt(&server, Vec::new()).await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));
        let result = test_result();

        let error = reconcile_unretained_submission(
            &access,
            &result.node_did,
            &result.session_id,
            &result.request_id,
        )
        .await
        .expect_err("an absent receipt does not prove the original never commits");

        let unconfirmed = error
            .downcast_ref::<DeliveryUnconfirmed>()
            .unwrap_or_else(|| {
                panic!(
                    "absence must stay unresolved: {}",
                    ErrorChain(error.as_ref())
                )
            });
        assert_eq!(unconfirmed.request_id, result.request_id);
        assert_eq!(unconfirmed.session_id, result.session_id);
        assert!(
            create_requests(&server).await.is_empty(),
            "reconciliation must not create a fresh request"
        );
    }

    #[tokio::test]
    async fn unretained_unreadable_receipt_stays_unknown() {
        let server = MockServer::start().await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));
        let result = test_result();

        let error = reconcile_unretained_submission(
            &access,
            &result.node_did,
            &result.session_id,
            &result.request_id,
        )
        .await
        .expect_err("an unreadable receipt proves nothing");

        assert!(error.downcast_ref::<DeliveryUnconfirmed>().is_some());
    }

    #[tokio::test]
    async fn unretained_present_receipt_resolves_to_original_identity() {
        let server = MockServer::start().await;
        let result = test_result();
        mount_receipt(&server, vec![json!({"request_id": result.request_id})]).await;
        let access = test_access(&format!("{}/api/v0/graphql", server.uri()));

        let resolved = reconcile_unretained_submission(
            &access,
            &result.node_did,
            &result.session_id,
            &result.request_id,
        )
        .await
        .expect("a visible owner request proves delivery");

        assert_eq!(resolved.request_id, result.request_id);
        assert_eq!(resolved.session_id, result.session_id);
        assert_eq!(resolved.node_did, result.node_did);
    }
}
