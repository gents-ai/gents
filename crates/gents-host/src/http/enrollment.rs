use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use gents::defra_node::Node as EmbeddedNode;
use gents_protocol::enrollment::{
    EnrollmentOfferRecord, EnrollmentOperatorDecisionCommand, EnrollmentOperatorQueryCommand,
};
use gents_store::identity::NodeIdentity;
use serde::Serialize;
use tokio::sync::RwLock;

pub type EnrollmentOfferIssuerHandle =
    Arc<RwLock<Option<gents_p2p::p2p_reconcile::EnrollmentOfferIssuer>>>;
pub type EnrollmentDecisionServiceHandle = Arc<RwLock<Option<EnrollmentDecisionService>>>;

#[derive(Clone)]
pub struct EnrollmentDecisionService {
    identity: Arc<dyn NodeIdentity>,
    node: Arc<EmbeddedNode>,
    store: gents_p2p::p2p_reconcile::GraphqlEnrollmentStore,
}

impl EnrollmentDecisionService {
    pub fn new(identity: Arc<dyn NodeIdentity>, node: Arc<EmbeddedNode>) -> Self {
        Self {
            store: gents_p2p::p2p_reconcile::GraphqlEnrollmentStore::new(
                node.clone(),
                identity.clone(),
            ),
            identity,
            node,
        }
    }

    pub async fn decide(
        &self,
        command: &EnrollmentOperatorDecisionCommand,
    ) -> Result<gents_p2p::p2p_reconcile::EnrollmentDecisionOutcome> {
        gents_p2p::p2p_reconcile::decide_operator_command(
            self.identity.as_ref(),
            &self.node,
            &self.store,
            command,
        )
        .await
    }

    pub async fn query(
        &self,
        command: &EnrollmentOperatorQueryCommand,
    ) -> Result<serde_json::Value> {
        gents_store::local::document_acp::ensure_enrollment(
            &self.node,
            gents_store::local::document_acp::EnrollmentRequest::Pending,
        )
        .await?;
        let now = Utc::now();
        gents_p2p::p2p_reconcile::authenticate_operator_query(self.identity.as_ref(), command, now)
            .await?;
        gents_p2p::p2p_reconcile::consume_operator_nonce(
            &self.node,
            &command.admin_did,
            &command.nonce,
            now,
        )
        .await?;
        let projection = self.store.load_projection().await?;
        gents_p2p::p2p_reconcile::operator_query_view(projection, &command.query, now)
    }
}

impl EnrollmentDecisionService {
    pub async fn authenticate_node_reader(
        &self,
        signer_did: &str,
        payload: &[u8],
        sig: &[u8],
        nonce: &str,
    ) -> Result<()> {
        anyhow::ensure!(
            self.identity.verify(signer_did, payload, sig).await?,
            "operator command signature is invalid"
        );
        let access = gents_store::config_client::ConfigAccess::local_as(
            self.node.clone(),
            identity::Did::new(signer_did)?,
        );
        let rows = access
            .read_view(&gents_documents::views::config::NodeList::new(self.identity.did()).limit(2))
            .await?;
        anyhow::ensure!(rows.len() == 1, "the client cannot read this Node");
        gents_p2p::p2p_reconcile::consume_operator_nonce(&self.node, signer_did, nonce, Utc::now())
            .await
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EnrollmentStatus {
    Available {
        token: String,
        offer: Box<EnrollmentOfferRecord>,
    },
    Unavailable {
        reason: &'static str,
    },
}

/// Why the status offers no enrollment while no offer issuer is installed: a
/// home that serves without an enrollment root never installs one
/// (`Identity.NodePublication.enrollment`).
pub fn unavailable_reason(provider: gents_store::local::document_acp::DocumentAcp) -> &'static str {
    use gents_store::local::document_acp::{enrollment, EnrollmentAnswer, EnrollmentRequest};
    match enrollment(provider, EnrollmentRequest::Offer) {
        EnrollmentAnswer::Served => "runtime_not_ready",
        EnrollmentAnswer::PeerNetworkingUnavailable => "peer_networking_unavailable",
    }
}

/// The HTTP status of a refused enrollment request: one a home cannot serve
/// at all is not a malformed request.
pub fn refusal_status(error: &anyhow::Error) -> axum::http::StatusCode {
    use gents_store::local::document_acp::DocumentAcpError;
    match DocumentAcpError::in_chain(error.as_ref()) {
        Some(DocumentAcpError::TransportGrantsAwaitProvider) => {
            axum::http::StatusCode::NOT_IMPLEMENTED
        }
        Some(DocumentAcpError::TransportMembership(_)) => {
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        }
        _ => axum::http::StatusCode::BAD_REQUEST,
    }
}

pub fn empty_issuer_handle() -> EnrollmentOfferIssuerHandle {
    Arc::new(RwLock::new(None))
}

pub fn empty_decision_service_handle() -> EnrollmentDecisionServiceHandle {
    Arc::new(RwLock::new(None))
}

#[cfg(test)]
mod tests {
    use gents_store::local::document_acp::{DocumentAcp, DocumentAcpError};

    /// Lean `Identity.NodePublication.a_vera_home_refuses_every_enrollment_request`
    /// and `a_home_on_the_node_store_enrolls_as_before`, as the runtime's HTTP
    /// surface reports them.
    #[test]
    fn a_vera_home_reports_enrollment_unavailable_and_a_local_home_as_before() {
        assert_eq!(
            super::unavailable_reason(DocumentAcp::Vera),
            "peer_networking_unavailable"
        );
        assert_eq!(
            super::unavailable_reason(DocumentAcp::Local),
            "runtime_not_ready"
        );
        let refused = anyhow::Error::new(DocumentAcpError::TransportGrantsAwaitProvider)
            .context("deciding an enrollment request");
        assert_eq!(
            super::refusal_status(&refused),
            axum::http::StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(
            super::refusal_status(&anyhow::anyhow!("operator command signature is invalid")),
            axum::http::StatusCode::BAD_REQUEST
        );
    }
}
