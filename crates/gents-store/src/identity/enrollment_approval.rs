use std::future::Future;

use gents_protocol::enrollment::{decode_offer, EnrollmentError, EnrollmentRequestRecord};

use super::enrollment_grant::{
    grant_enrollment_authority, EnrollmentGrantError, EnrollmentGrantOutcome,
};
use super::{IdentityError, NodeIdentity};
use crate::config_client::{ConfigAccess, ConfigClientError};

/// What an approval does once its grant has run as the approver (Lean
/// `Identity.EnrollmentGrant.ApprovalStep`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStep {
    Approve,
    Refuse,
}

impl ApprovalStep {
    /// Lean `afterGrant`.
    pub fn after_grant(grant_committed: bool) -> Self {
        if grant_committed {
            Self::Approve
        } else {
            Self::Refuse
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnrollmentApprovalError {
    #[error("enrollment request {0} is not pending; nothing was granted or approved")]
    NotPending(String),
    #[error("enrollment request is not approvable: {0}; nothing was granted or approved")]
    Unapprovable(&'static str),
    #[error("enrollment request record is invalid; nothing was granted or approved")]
    Record(#[source] EnrollmentError),
    #[error("verifying an enrollment signature failed; nothing was granted or approved")]
    Verify(#[source] IdentityError),
    #[error("the approver cannot read this runtime's Node; nothing was granted or approved")]
    NodeRead(#[source] ConfigClientError),
    #[error("the approver sees {0} Nodes for this runtime, not one; nothing was granted or approved")]
    NodeCount(usize),
    #[error(
        "the approver may not grant this enrollment's Node administration, which only the Node \
         owner or a Node administrator may grant; nothing was approved"
    )]
    Grant(#[source] EnrollmentGrantError),
    #[error(
        "access was granted under this request but its approval did not commit; approve it \
         again to finish, or deny it to withdraw the access"
    )]
    Approve(#[source] anyhow::Error),
}

/// The authority one approval is decided with.
pub struct EnrollmentApproval<'a> {
    /// The approving actor's own access: a User's signed client or an Agent's
    /// managed identity. ACP decides whether it may grant Node administration.
    pub approver: &'a ConfigAccess,
    /// The runtime principal that records the receipt and issues node permissions.
    pub runtime: &'a ConfigAccess,
    /// The runtime's identity, which signed the offer and owns the enrollment.
    pub node: &'a dyn NodeIdentity,
    /// The signed pending request, as the runtime's enrollment projection reports it.
    pub request: &'a EnrollmentRequestRecord,
    /// Managed pairing binds the approval to this Node and candidate.
    pub expected: Option<(&'a str, &'a str)>,
}

/// One enrollment approval for every surface (Lean
/// `Identity.EnrollmentGrant.approval`): verify the signed request, grant its
/// Node administration as the approver, then run `decide`, the runtime's
/// Node-signed approval. ACP refusing the approver's grant approves nothing.
pub async fn approve_enrollment<T, D, F>(
    approval: EnrollmentApproval<'_>,
    decide: D,
) -> Result<(EnrollmentGrantOutcome, T), EnrollmentApprovalError>
where
    D: FnOnce() -> F,
    F: Future<Output = anyhow::Result<T>>,
{
    let granted = grant_as_approver(&approval).await;
    match (ApprovalStep::after_grant(granted.is_ok()), granted) {
        (ApprovalStep::Approve, Ok(outcome)) => {
            let decided = decide().await.map_err(EnrollmentApprovalError::Approve)?;
            Ok((outcome, decided))
        }
        (_, Err(refused)) => Err(refused),
        (ApprovalStep::Refuse, Ok(_)) => Err(EnrollmentApprovalError::Unapprovable(
            "its grant committed but the approval step refused it",
        )),
    }
}

/// The grant half of [`approve_enrollment`], which `gents p2p enrollment
/// grant` also runs to finish a grant whose approval did not commit.
pub async fn grant_as_approver(
    approval: &EnrollmentApproval<'_>,
) -> Result<EnrollmentGrantOutcome, EnrollmentApprovalError> {
    verify_pending_client_request(approval).await?;
    let request = approval.request;
    let candidate = identity::Did::new(&request.candidate_did)
        .map_err(|_| EnrollmentApprovalError::Unapprovable("candidate DID is invalid"))?;
    let rendered = {
        use gents_protocol::document_query::{fields::node as fields, Filter, Query, Selection};
        Query::select(Selection::doc_id())
            .filter(Filter::eq(fields::NODE_DID, approval.node.did()))
            .render()
    };
    let response = approval
        .approver
        .read(&rendered)
        .await
        .map_err(EnrollmentApprovalError::NodeRead)?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "Node");
    let [node] = rows.as_slice() else {
        return Err(EnrollmentApprovalError::NodeCount(rows.len()));
    };
    let node_doc_id = node["_docID"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or(EnrollmentApprovalError::NodeCount(0))?;
    grant_enrollment_authority(
        approval.approver,
        approval.runtime,
        node_doc_id,
        &request.request_id,
        &candidate,
    )
    .await
    .map_err(EnrollmentApprovalError::Grant)
}

/// The runtime's own offer, a candidate signature over it, a current request
/// for this runtime, and the client profile, before any authority is granted.
pub async fn verify_pending_client_request(
    approval: &EnrollmentApproval<'_>,
) -> Result<(), EnrollmentApprovalError> {
    let (node, request) = (approval.node, approval.request);
    let offer = decode_offer(&request.offer_token).map_err(EnrollmentApprovalError::Record)?;
    request
        .validate_against_offer(&offer)
        .map_err(EnrollmentApprovalError::Record)?;
    let refuse = |reason| Err(EnrollmentApprovalError::Unapprovable(reason));
    if offer.admin_did != node.did() {
        return refuse("its offer has a different administrator");
    }
    if request.owner_node_did != node.did() {
        return refuse("it belongs to another runtime");
    }
    if request.profile != "client" {
        return refuse("only a client enrollment receives Node administration");
    }
    if let Some((node_did, client_did)) = approval.expected {
        if node.did() != node_did
            || request.owner_node_did != node_did
            || request.candidate_did != client_did
        {
            return refuse("it does not match the selected Node and desktop User");
        }
    }
    if !node
        .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
        .await
        .map_err(EnrollmentApprovalError::Verify)?
    {
        return refuse("its offer signature is invalid");
    }
    if !node
        .verify(
            &request.candidate_did,
            &request.signing_payload(),
            &request.candidate_sig,
        )
        .await
        .map_err(EnrollmentApprovalError::Verify)?
    {
        return refuse("its candidate signature is invalid");
    }
    let now = chrono::Utc::now();
    let current = chrono::DateTime::parse_from_rfc3339(&request.issued_at)
        .ok()
        .zip(chrono::DateTime::parse_from_rfc3339(&request.expires_at).ok())
        .is_some_and(|(issued, expires)| issued <= now && now <= expires);
    if !current {
        return refuse("it is not current");
    }
    Ok(())
}
