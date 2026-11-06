//! Receipt reconciliation for owner-authored chat submissions.
//!
//! A submitted request is committed with a stable, signed identity. When the
//! create response is ambiguous the owner must read that identity back before
//! it may repost, and it must never re-sign: the retained signed payload is
//! replayed byte-for-byte. Everything in this module therefore treats the
//! mutation text and the admission signature as opaque, non-loggable data.

use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents::request::lifecycle::ClientSubmissionError;
use gents::store::config_client::ConfigAccess;
use gents_protocol::graphql::{escape_graphql_string, GraphqlRequestOptions};
use gents_protocol::request_admission::AgentRequestCreate;
#[cfg(test)]
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

use super::SubmittedRequest;

pub(crate) mod correlated;
pub(crate) mod delivery;
pub(crate) mod goal_delivery;
pub(crate) mod journal;

/// Confirmed rejection of the original caller-bound signed preparation.
/// The original identity remains recoverable until the caller acknowledges it.
/// The journal retains the outcome, not the native error chain; a recovery
/// after a lost response can therefore lack the original error detail.
#[derive(Debug, thiserror::Error)]
pub enum SubmissionNotDelivered {
    #[error("request {} was not delivered", .request.request_id)]
    Rejected {
        request: SubmittedRequest,
        #[source]
        cause: Option<anyhow::Error>,
    },
}

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
#[derive(Debug)]
pub struct SubmissionOutcomeUnknown {
    pub request_id: String,
    pub session_id: String,
    pub pending: UnresolvedSubmission,
    pub cause: Option<anyhow::Error>,
}

#[derive(Debug, thiserror::Error)]
pub enum SubmissionAttemptError {
    #[error(transparent)]
    Unknown(Box<SubmissionOutcomeUnknown>),
    #[error(transparent)]
    Rejected(ClientSubmissionError),
}

#[derive(Debug, thiserror::Error)]
pub enum SubmissionError {
    #[error(transparent)]
    Unknown(#[from] Box<SubmissionOutcomeUnknown>),
    #[error(transparent)]
    Cleanup(#[from] SubmissionCleanupIncomplete),
    #[error(transparent)]
    NotDelivered(#[from] SubmissionNotDelivered),
    #[error(transparent)]
    Unconfirmed(#[from] DeliveryUnconfirmed),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<SubmissionOutcomeUnknown> for SubmissionError {
    fn from(error: SubmissionOutcomeUnknown) -> Self {
        Self::Unknown(Box::new(error))
    }
}

/// A confirmed delivery outcome whose retained local journal could not be retired.
#[derive(Debug, thiserror::Error)]
pub enum SubmissionCleanupIncomplete {
    #[error("request {} was delivered; local journal cleanup failed: {cause}", .request.request_id)]
    Accepted {
        request: SubmittedRequest,
        #[source]
        cause: anyhow::Error,
    },
    #[error("request {} was not delivered; local journal cleanup failed: {cause}", .request.request_id)]
    Rejected {
        request: SubmittedRequest,
        #[source]
        cause: anyhow::Error,
        rejection: anyhow::Error,
    },
}

pub(crate) fn report_cleanup(
    pending: &UnresolvedSubmission,
    delivery: std::result::Result<SubmittedRequest, SubmissionAttemptError>,
    cleanup: Result<()>,
) -> std::result::Result<SubmittedRequest, SubmissionError> {
    let delivery = match delivery {
        Err(SubmissionAttemptError::Unknown(error)) => return Err(error.into()),
        Err(SubmissionAttemptError::Rejected(error)) => Err(anyhow::Error::from(error)),
        Ok(request) => Ok(request),
    };
    match cleanup {
        Ok(()) => delivery.map_err(SubmissionError::Other),
        Err(cause) => match delivery {
            Ok(request) => Err(SubmissionCleanupIncomplete::Accepted { request, cause }.into()),
            Err(rejection) => Err(SubmissionCleanupIncomplete::Rejected {
                request: pending.result.clone(),
                cause,
                rejection,
            }
            .into()),
        },
    }
}

pub(crate) fn report_rejection(
    pending: &UnresolvedSubmission,
    rejection: anyhow::Error,
    cleanup: Result<()>,
) -> SubmissionError {
    match cleanup {
        Err(cause) => SubmissionCleanupIncomplete::Rejected {
            request: pending.result.clone(),
            cause,
            rejection,
        }
        .into(),
        Ok(()) => SubmissionNotDelivered::Rejected {
            request: pending.result.clone(),
            cause: Some(rejection),
        }
        .into(),
    }
}

impl std::fmt::Display for SubmissionOutcomeUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "submission outcome unknown for request {} in session {}: it may have been delivered; check the session before resending",
            self.request_id, self.session_id
        )
    }
}

impl std::error::Error for SubmissionOutcomeUnknown {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_ref().map(|cause| cause.as_ref())
    }
}

/// An earlier send whose delivery cannot be decided without its signed
/// payload (for example after a restart).
///
/// Carries identity only. An absent owner receipt does not prove the original
/// will never commit, because that write may still be in flight, so callers
/// must keep this identity unresolved instead of allowing a fresh send.
#[derive(Debug)]
pub struct DeliveryUnconfirmed {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
}

impl std::fmt::Display for DeliveryUnconfirmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "delivery of request {} in session {} is unconfirmed: it may still be delivered; check the session before resending",
            self.request_id, self.session_id
        )
    }
}

impl std::error::Error for DeliveryUnconfirmed {}

const RECEIPT_READ_CONTEXT: &str = "reading owner signed submission receipt";

impl UnresolvedSubmission {
    fn validate_binding(&self, request_id: &str) -> Result<()> {
        ensure!(
            self.request_id == request_id
                && self.request_id == self.create.request_id
                && self.node_did == self.create.node_did
                && self.session_id == self.create.session_id
                && self.result.request_id == self.request_id
                && self.result.node_did == self.node_did
                && self.result.session_id == self.session_id
                && self.mutation
                    == self
                        .create
                        .graphql_client_publication_mutation()
                        .map_err(anyhow::Error::msg)?,
            "retained submission is inconsistent with its signed request"
        );
        Ok(())
    }

    pub(crate) fn accepts_composer_content(&self, content: &str) -> bool {
        if content.trim().is_empty() {
            return true;
        }
        let selected = gents::documents::skills::prompt_slash_skill_selection(content.trim());
        selected.prompt.trim() == self.create.content.trim()
            && (selected.selected_skill_ids.is_empty()
                || selected.selected_skill_ids == self.create.input.selected_skill_ids)
    }

    /// Retain a prepared signed create and the identity it will produce.
    pub(crate) fn new(create: AgentRequestCreate, result: SubmittedRequest) -> Result<Self> {
        let mutation = create
            .graphql_client_publication_mutation()
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            request_id: create.request_id.clone(),
            session_id: create.session_id.clone(),
            node_did: create.node_did.clone(),
            create,
            mutation,
            result,
        })
    }

    fn unknown(&self, cause: Option<anyhow::Error>) -> SubmissionOutcomeUnknown {
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
            cause,
        }
    }

    #[cfg(test)]
    fn outcome_unknown(&self, cause: Option<anyhow::Error>) -> anyhow::Error {
        self.unknown(cause).into()
    }
}

/// Read the durable signed receipt for exactly this prepared request.
///
/// Returns `Ok(true)` only when the scoped row matches the request identity and
/// the prepared admission signature; a row that matches the identity but not
/// the signature is an error, never an absence. Local access reads the same
/// receipt from the embedded owner.
#[cfg(test)]
async fn owner_receipt(access: &ConfigAccess, create: &AgentRequestCreate) -> Result<bool> {
    read_owner_receipt(access, create)
        .await
        .context(RECEIPT_READ_CONTEXT)
}

#[cfg(test)]
async fn read_owner_receipt(access: &ConfigAccess, create: &AgentRequestCreate) -> Result<bool> {
    let scope = gents::session::session_scope_filter(
        &create.node_did,
        &create.session_id,
        (!create.requester_did.is_empty()).then_some(create.requester_did.as_str()),
    );
    let logical = escape_graphql_string(&create.request_id);
    let query = format!(
        "{{AgentRequest(filter:{{{scope},request_id:{{_eq:\"{logical}\"}}}}){{{}}}}}",
        gents::SIGNED_REQUEST_FIELDS
    );
    let response = match access {
        ConfigAccess::Graphql(endpoint) => {
            gents::store::config_client::query_graphql_with_options(
                endpoint,
                &query,
                GraphqlRequestOptions {
                    timeout: Duration::from_secs(30),
                    max_attempts: 1,
                    retry_backoff: Duration::ZERO,
                },
            )
            .await?
        }
        ConfigAccess::Local(_) => access.execute(&query).await?,
    };
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("request receipt query omitted rows")?;
    ensure!(rows.len() <= 1, "ambiguous scoped submission receipt");
    let Some(value) = rows.first() else {
        return Ok(false);
    };
    let row: AgentRequestRow =
        serde_json::from_value(value.clone()).context("decoding submission receipt row")?;
    ensure!(
        row.request_id == create.request_id
            && row.node_did.as_deref() == Some(create.node_did.as_str())
            && row.session_id.as_deref() == Some(create.session_id.as_str())
            && row.requester_did.as_deref()
                == (!create.requester_did.is_empty()).then_some(create.requester_did.as_str()),
        "submission receipt crossed exact request scope"
    );
    let signature = bs58::encode(&create.admission.signature).into_string();
    ensure!(
        row.admission_signer_did.as_deref() == Some(create.admission.signer_did.as_str())
            && row.admission_signature.as_deref() == Some(signature.as_str()),
        "submission receipt differs from prepared signed request"
    );
    Ok(true)
}

/// Commit a retained signed submission, reconciling ambiguous outcomes.
///
/// Full-storage errors retain uncertain delivery even for embedded writes:
/// failure to sync or roll back a WAL is not proof that nothing committed. An owner
/// write is only a definite rejection when the owner answered with a GraphQL
/// `errors` body and the receipt proves it holds no such request; every other
/// post-send failure (HTTP status, transport, undecodable body, unreadable
/// receipt, or an "already exists" rejection with no visible receipt) returns
/// [`SubmissionOutcomeUnknown`], because the owner may hold or still be
/// committing the request.
pub(super) async fn commit_signed_submission(
    access: &ConfigAccess,
    pending: UnresolvedSubmission,
) -> std::result::Result<SubmittedRequest, SubmissionAttemptError> {
    let written = gents::request::lifecycle::submit_client_request(access, &pending.create).await;
    let Err(error) = written else {
        return Ok(pending.result);
    };
    reconcile_commit_failure(access, pending, error).await
}

async fn reconcile_commit_failure(
    access: &ConfigAccess,
    pending: UnresolvedSubmission,
    error: ClientSubmissionError,
) -> std::result::Result<SubmittedRequest, SubmissionAttemptError> {
    if gents::store::graphql::is_storage_full(&error) {
        return Err(SubmissionAttemptError::Unknown(Box::new(
            pending.unknown(Some(error.into())),
        )));
    }
    match gents::request::lifecycle::reconcile_client_request(access, &pending.create).await {
        Ok(true) => Ok(pending.result),
        Ok(false) if !error.is_unconfirmed() && !reports_existing_request(&error) => {
            Err(SubmissionAttemptError::Rejected(error))
        }
        _ => Err(SubmissionAttemptError::Unknown(Box::new(
            pending.unknown(Some(error.into())),
        ))),
    }
}

/// A rejection that implies the owner already holds a request with this id.
/// External premise: DefraDB reports a duplicate document only in its error
/// wording; its GraphQL errors carry no structured code for it.
fn reports_existing_request(error: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(Some(error), |cause| cause.source()).any(|cause| {
        let text = cause.to_string().to_ascii_lowercase();
        text.contains("already exists") || text.contains("duplicate")
    })
}

/// Reconcile an earlier unknown send whose signed payload is no longer retained.
///
/// A visible owner request with a completed publication marker proves delivery. Absence, or an
/// unreadable lookup, proves nothing: the original write may still be in
/// flight. Both return [`DeliveryUnconfirmed`] so the identity stays
/// unresolved and no fresh request is minted.
pub async fn reconcile_unretained_submission(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    request_id: &str,
) -> std::result::Result<SubmittedRequest, SubmissionError> {
    match submitted_request_exists(access, node_did, session_id, request_id).await {
        Ok(true) => Ok(SubmittedRequest {
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
            agent_did: None,
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
    let response = gents::store::config_client::query_graphql_with_options(
        endpoint,
        &format!(
            "{{AgentRequest(filter:{{{scope},request_id:{{_eq:\"{logical}\"}}}}){{request_id relationships_pending}}}}"
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
    Ok(rows
        .first()
        .is_some_and(|row| row["relationships_pending"] == false))
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
) -> std::result::Result<SubmittedRequest, SubmissionAttemptError> {
    match gents::request::lifecycle::reconcile_client_request(access, &pending.create).await {
        Ok(true) => Ok(pending.result.clone()),
        Ok(false) => commit_signed_submission(access, pending.clone()).await,
        Err(error) => Err(SubmissionAttemptError::Unknown(Box::new(
            pending.unknown(Some(error.into())),
        ))),
    }
}

/// Endpoint helper kept private to the tests: a GraphQL access at `url`.
#[cfg(test)]
fn test_access(url: &str) -> ConfigAccess {
    ConfigAccess::Graphql(gents::store::config_client::GraphqlEndpoint::anonymous(url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use defra_node::Node as EmbeddedNode;
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SIGNER_DID: &str = "did:key:z6MkownerReceiptTest";
    const REQUEST_ID: &str = "request-receipt-1";
    const SESSION_ID: &str = "session-receipt-1";
    const AGENT_DID: &str = "agent-receipt-1";

    fn test_create() -> AgentRequestCreate {
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            REQUEST_ID,
            SIGNER_DID,
            SIGNER_DID,
            AGENT_DID,
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
            .graphql_client_publication_mutation()
            .expect("test create renders a canonical mutation")
    }

    fn test_result() -> SubmittedRequest {
        SubmittedRequest {
            request_id: REQUEST_ID.to_string(),
            session_id: SESSION_ID.to_string(),
            node_did: SIGNER_DID.to_string(),
            agent_did: Some(AGENT_DID.to_string()),
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

    #[tokio::test]
    async fn cleanup_observation_preserves_the_modeled_delivery_and_identity() {
        let snapshot = tokio::task::spawn_blocking(
            gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
        )
        .await
        .unwrap()
        .unwrap();
        for case in snapshot["client_submission_cleanup_cases"]
            .as_array()
            .unwrap()
        {
            let request = case["prepared"]["request"].as_u64().unwrap().to_string();
            let mut pending = test_pending();
            pending.request_id = request.clone();
            pending.create.request_id = request.clone();
            pending.result.request_id = request.clone();
            let outcome = case["outcome"].as_str().unwrap();
            let delivery = match outcome {
                "accepted" => Ok(pending.result.clone()),
                "notDelivered" => Err(SubmissionAttemptError::Rejected(
                    ClientSubmissionError::Rejected(anyhow::anyhow!("confirmed rejection")),
                )),
                "unresolved" => Err(SubmissionAttemptError::Unknown(Box::new(
                    pending.unknown(None),
                ))),
                _ => panic!("unknown modeled delivery"),
            };
            let cleanup = if case["cleanup_succeeded"].as_bool().unwrap() {
                Ok(())
            } else {
                Err(anyhow::anyhow!("native cleanup denial"))
            };
            let result = report_cleanup(&pending, delivery, cleanup);
            assert_eq!(
                case["expected_prepared"]["request"]
                    .as_u64()
                    .unwrap()
                    .to_string(),
                request
            );
            assert_eq!(case["expected_outcome"].as_str(), Some(outcome));
            if case["cleanup_failed"].as_bool().unwrap() {
                let SubmissionError::Cleanup(error) = result.unwrap_err() else {
                    panic!("cleanup failure must retain its outcome");
                };
                let (observed, accepted, cause) = match error {
                    SubmissionCleanupIncomplete::Accepted { request, cause } => {
                        (request, true, cause)
                    }
                    SubmissionCleanupIncomplete::Rejected { request, cause, .. } => {
                        (request, false, cause)
                    }
                };
                assert_eq!(observed.request_id, request);
                assert_eq!(accepted, outcome == "accepted");
                assert!(cause.to_string().contains("native cleanup denial"));
            } else if outcome == "accepted" {
                assert_eq!(result.unwrap().request_id, request);
            } else if outcome == "unresolved" {
                assert!(matches!(result.unwrap_err(), SubmissionError::Unknown(_)));
            } else {
                assert!(!matches!(result.unwrap_err(), SubmissionError::Cleanup(_)));
            }
        }
    }

    #[tokio::test]
    async fn rejected_recovery_retains_modeled_identity_and_native_error() {
        let snapshot = tokio::task::spawn_blocking(
            gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
        )
        .await
        .unwrap()
        .unwrap();
        for case in snapshot["client_submission_rejection_cases"]
            .as_array()
            .unwrap()
        {
            if case["outcome"] != "notDelivered" {
                continue;
            }
            let mut pending = test_pending();
            pending.result.request_id = case["request"].as_u64().unwrap().to_string();
            let native_error = case["native_error"].as_u64();
            let error = if let Some(token) = native_error {
                report_rejection(&pending, anyhow::anyhow!("native denial {token}"), Ok(()))
            } else {
                SubmissionNotDelivered::Rejected {
                    request: pending.result.clone(),
                    cause: None,
                }
                .into()
            };
            let SubmissionError::NotDelivered(SubmissionNotDelivered::Rejected { request, cause }) =
                error
            else {
                panic!("rejection expected")
            };
            assert_eq!(
                request.request_id,
                case["expected_request"].as_u64().unwrap().to_string()
            );
            assert_eq!(
                cause.is_some(),
                case["expected_native_error"].as_u64().is_some()
            );
            if let Some(cause) = cause {
                assert_eq!(
                    cause.to_string(),
                    format!(
                        "native denial {}",
                        case["expected_native_error"].as_u64().unwrap()
                    )
                );
            }
        }
    }

    #[tokio::test]
    async fn uncertain_recovery_retains_modeled_identity_and_access_error() {
        let snapshot = tokio::task::spawn_blocking(
            gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
        )
        .await
        .unwrap()
        .unwrap();
        for case in snapshot["client_submission_uncertain_error_cases"]
            .as_array()
            .unwrap()
        {
            let mut pending = test_pending();
            pending.request_id = case["request"].as_u64().unwrap().to_string();
            let cause = case["access_error"]
                .as_u64()
                .map(|token| anyhow::anyhow!("native access error {token}"));
            let error = pending
                .outcome_unknown(cause)
                .downcast::<SubmissionOutcomeUnknown>()
                .unwrap();
            assert_eq!(
                error.request_id,
                case["expected_request"].as_u64().unwrap().to_string()
            );
            assert_eq!(case["expected_outcome"], "unresolved");
            assert_eq!(
                error.cause.is_some(),
                case["expected_access_error"].as_u64().is_some()
            );
            if let Some(cause) = error.cause {
                assert_eq!(
                    cause.to_string(),
                    format!(
                        "native access error {}",
                        case["expected_access_error"].as_u64().unwrap()
                    )
                );
            }
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
    async fn client_submission_refuses_a_different_local_signer_before_journal_write() {
        use gents::NodeIdentity as _;
        let home = tempfile::tempdir().unwrap();
        let client =
            gents::KeyIdentity::load_or_create(home.path().join("client.key"), None).unwrap();
        let other =
            gents::KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
        let node = std::sync::Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(client.did())
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let snapshot = tokio::task::spawn_blocking(
            gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
        )
        .await
        .unwrap()
        .unwrap();
        for case in snapshot["identity_private_journal_policy"]["signer_checks"]
            .as_array()
            .unwrap()
        {
            let actor = match case["role"].as_str().unwrap() {
                "client" => client.did(),
                "other" => other.did(),
                "empty" => "",
                _ => panic!("unknown modeled signer"),
            };
            assert_eq!(
                journal::require_client_signer(&node, actor).is_ok(),
                case["expected"].as_bool().unwrap()
            );
        }
        let access = ConfigAccess::local_as(node.clone(), other.did().parse().unwrap());
        let error = super::super::submit_request(
            &node,
            &crate::client::store::ClientStore::default(),
            &access,
            "session",
            "node",
            other.did(),
            &other,
            AgentRequestAdmissionRecord::client(other.did()),
            "message",
            Some("agent"),
            super::super::SubmitRequestOptions::default(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("local client signer"));
        let own = ConfigAccess::local_as(node.clone(), client.did().parse().unwrap());
        let rows = own
            .execute("{ PendingChatSubmission { _docID } }")
            .await
            .unwrap();
        assert!(rows["data"]["PendingChatSubmission"]
            .as_array()
            .unwrap()
            .is_empty());
        node.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn private_submission_reopens_only_for_its_client() {
        use gents::NodeIdentity as _;
        let home = tempfile::tempdir().unwrap();
        let client =
            gents::KeyIdentity::load_or_create(home.path().join("client.key"), None).unwrap();
        let other =
            gents::KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
        let open = || async {
            std::sync::Arc::new(
                EmbeddedNode::builder()
                    .with_node_identity_did(client.did())
                    .data_path(home.path())
                    .build()
                    .await
                    .unwrap(),
            )
        };
        let node = open().await;
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let pending = test_pending();
        journal::save(&node, client.did(), &pending).await.unwrap();
        node.shutdown().await.unwrap();
        drop(node);
        let reopened = open().await;
        assert!(journal::load(&reopened, other.did(), REQUEST_ID)
            .await
            .unwrap()
            .is_none());
        let restored = journal::load(&reopened, client.did(), REQUEST_ID)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.mutation, pending.mutation);
        assert_eq!(restored.create, pending.create);
        journal::forget(&reopened, client.did(), REQUEST_ID)
            .await
            .unwrap();
        assert!(journal::load(&reopened, client.did(), REQUEST_ID)
            .await
            .unwrap()
            .is_none());
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retained_submission_survives_reopen_and_replays_exact_bytes() {
        let home = tempfile::tempdir().unwrap();
        use gents::NodeIdentity as _;
        let client =
            gents::KeyIdentity::load_or_create(home.path().join("client.key"), None).unwrap();
        let node = std::sync::Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(client.did())
                .data_path(home.path())
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let pending = test_pending();
        journal::save(&node, client.did(), &pending).await.unwrap();
        node.shutdown().await.unwrap();
        drop(node);

        let reopened = std::sync::Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(client.did())
                .data_path(home.path())
                .build()
                .await
                .unwrap(),
        );
        let restored = journal::load(&reopened, client.did(), REQUEST_ID)
            .await
            .unwrap()
            .unwrap();
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
        journal::forget(&reopened, client.did(), REQUEST_ID)
            .await
            .unwrap();
        assert!(journal::load(&reopened, client.did(), REQUEST_ID)
            .await
            .unwrap()
            .is_none());
        reopened.shutdown().await.unwrap();
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
        let SubmissionAttemptError::Unknown(unknown) = &error else {
            panic!("unknown outcome is a typed error: {error:#}");
        };
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
    async fn generated_storage_failures_retain_signed_submissions_without_replay() {
        let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        for case in contract["storage_failure_cases"].as_array().unwrap() {
            let server = MockServer::start().await;
            mount_create(
                &server,
                ResponseTemplate::new(200).set_body_json(json!({
                    "errors": [{"message": case["message"], "extensions": {"code": case["code"]}}]
                })),
            )
            .await;
            mount_receipt(&server, Vec::new()).await;
            let access = test_access(&format!("{}/api/v0/graphql", server.uri()));
            let pending = test_pending();
            let error = commit_signed_submission(&access, pending.clone())
                .await
                .unwrap_err();
            let unknown = match &error {
                SubmissionAttemptError::Unknown(unknown) => Some(unknown),
                SubmissionAttemptError::Rejected(_) => None,
            };
            assert_eq!(
                unknown.is_some(),
                case["outcome_unknown"].as_bool().unwrap()
            );
            if let Some(unknown) = unknown {
                assert_eq!(unknown.request_id, pending.request_id);
                assert_eq!(unknown.session_id, pending.session_id);
                assert_eq!(unknown.pending.mutation, pending.mutation);
                assert_eq!(
                    unknown.pending.create.admission.signature,
                    pending.create.admission.signature
                );
                assert!(gents::store::graphql::is_storage_full(error.as_ref()));
                assert_eq!(
                    server.received_requests().await.unwrap().len(),
                    1,
                    "storage failure must not trigger another write or receipt probe"
                );
                assert!(case["same_request"].as_bool().unwrap());
                assert!(case["same_bytes"].as_bool().unwrap());
            }
        }
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
            matches!(error, SubmissionAttemptError::Rejected(_)),
            "a definite rejection must not be reported as possibly delivered: {error:#}"
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
    async fn local_receipt_confirms_the_exact_committed_signed_request() {
        let home = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            EmbeddedNode::builder()
                .data_path(home.path())
                .build()
                .await
                .unwrap(),
        );
        node.add_collection(gents_protocol::schemas::AGENT_REQUEST)
            .await
            .unwrap();
        let access = ConfigAccess::local(std::sync::Arc::clone(&node));
        let pending = test_pending();

        access
            .write("test.local_submission_receipt", &pending.mutation)
            .await
            .unwrap();

        let resolved = resolve_unresolved_submission(&access, &pending)
            .await
            .expect("the embedded signed receipt proves the original commit");
        assert_eq!(resolved, pending.result);
        let rows = access
            .execute(&format!(
                "{{AgentRequest(filter:{{request_id:{{_eq:\"{}\"}}}}){{request_id}}}}",
                escape_graphql_string(&pending.request_id)
            ))
            .await
            .unwrap();
        assert_eq!(rows["data"]["AgentRequest"].as_array().unwrap().len(), 1);

        let mut different_signature = pending.create.clone();
        different_signature.admission.signature[0] ^= 1;
        assert!(owner_receipt(&access, &different_signature).await.is_err());
        node.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn local_non_graphql_failure_preserves_unknown_delivery() {
        let home = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            EmbeddedNode::builder()
                .data_path(home.path())
                .build()
                .await
                .unwrap(),
        );
        let access = ConfigAccess::local(std::sync::Arc::clone(&node));
        let pending = test_pending();

        let error = reconcile_commit_failure(
            &access,
            pending.clone(),
            ClientSubmissionError::Unconfirmed(
                gents::store::config_client::ConfigClientError::StoreClaimHeld.into(),
            ),
        )
        .await
        .expect_err("a local post-send failure does not prove rejection");

        let SubmissionAttemptError::Unknown(unknown) = &error else {
            panic!("local non-GraphQL errors remain uncertain: {error:#}");
        };
        assert_eq!(unknown.request_id, pending.request_id);
        assert_eq!(unknown.pending.mutation, pending.mutation);
        node.shutdown().await.unwrap();
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

            let SubmissionAttemptError::Unknown(unknown) = &error else {
                panic!("HTTP {status} must be an unknown outcome: {error:#}");
            };
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
            matches!(error, SubmissionAttemptError::Unknown(_)),
            "an existing-request rejection with no visible receipt may still be delivered: {error:#}"
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

        let SubmissionError::Unconfirmed(unconfirmed) = &error else {
            panic!("absence must stay unresolved: {error:#}");
        };
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

        assert!(matches!(error, SubmissionError::Unconfirmed(_)));
    }

    #[tokio::test]
    async fn unretained_present_receipt_resolves_to_original_identity() {
        let server = MockServer::start().await;
        let result = test_result();
        mount_receipt(
            &server,
            vec![json!({"request_id": result.request_id, "relationships_pending": false})],
        )
        .await;
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
