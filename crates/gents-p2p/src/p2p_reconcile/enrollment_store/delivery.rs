use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_p2p_adapter::P2pDocumentRequest;
use gents_protocol::enrollment::{
    derive_route_receipt_id, EnrollmentRequestRecord,
    EnrollmentRouteReceiptDirection as WireReceiptDirection, EnrollmentRouteReceiptRecord,
    ENROLLMENT_PROTOCOL_VERSION,
};
use tokio::time::timeout;

use super::graphql::route_receipt_mutation;
use super::projection::{ActiveEnrollment, EnrollmentProjection};
use super::GraphqlEnrollmentStore;

impl GraphqlEnrollmentStore {
    pub(crate) async fn record_applied_route(
        &self,
        active: &ActiveEnrollment,
        applied_at: DateTime<Utc>,
    ) -> Result<String> {
        if let Some(doc_id) = &active.route_receipt_doc_id {
            return Ok(doc_id.clone());
        }
        let current = self.load_projection().await?;
        anyhow::ensure!(
            current.active.iter().any(|candidate| {
                candidate.request == active.request
                    && candidate.decision == active.decision
                    && candidate.revision == active.revision
            }),
            "authorization generation expired or changed before route receipt publication"
        );
        let direction = WireReceiptDirection::ClientToServer;
        let mut receipt = EnrollmentRouteReceiptRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            receipt_id: derive_route_receipt_id(
                &active.request.request_id,
                &active.request.request_digest,
                active.decision.authorization_sequence,
                &direction,
            ),
            request_id: active.request.request_id.clone(),
            request_digest: active.request.request_digest.clone(),
            network_id: active.request.network_id.clone(),
            admin_did: active.request.admin_did.clone(),
            member_did: active.request.candidate_did.clone(),
            member_peer: active.request.candidate_peer.clone(),
            server_peer: active.request.server_peer.clone(),
            owner_node_did: active.request.owner_node_did.clone(),
            authorization_sequence: active.decision.authorization_sequence,
            authorization_expires_at: active.decision.authorization_expires_at.clone(),
            direction,
            applied_at: applied_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            signer_did: self.identity.did().to_string(),
            admin_sig: Vec::new(),
        };
        receipt.admin_sig = self.identity.sign(&receipt.signing_payload()).await?;
        receipt.validate_against_approval(&active.request, &active.decision)?;
        let recipient = if self.identity.did() == active.request.admin_did {
            &active.request.candidate_did
        } else {
            anyhow::ensure!(
                self.identity.did() == active.request.candidate_did,
                "route receipt author is not an enrollment participant"
            );
            &active.request.admin_did
        };
        let mutation = route_receipt_mutation(
            &receipt,
            recipient,
            &super::super::publisher_field(&self.node)?,
        );
        let committed = self
            .publish_records(
                "p2p.write_enrollment_route_receipt",
                &mutation,
                &["NetworkEnrollmentRouteReceipt"],
                recipient,
                None,
            )
            .await;
        match committed {
            Ok(response) => Ok(gents_protocol::graphql::extract_mutation_doc_id(
                &response,
                "NetworkEnrollmentRouteReceipt",
            )?),
            Err(commit_error) => {
                let projection = self.load_projection().await?;
                projection
                    .active
                    .iter()
                    .find(|candidate| candidate.route_receipt.as_ref() == Some(&receipt))
                    .and_then(|candidate| candidate.route_receipt_doc_id.clone())
                    .with_context(|| {
                        format!(
                            "route receipt commit was not observably recovered after: {commit_error:#}"
                        )
                    })
            }
        }
    }

    pub(crate) async fn deliver_terminal(
        &self,
        request: &EnrollmentRequestRecord,
        decision_doc_id: &str,
        revision_doc_id: Option<&str>,
        route_receipt_doc_id: Option<&str>,
    ) -> bool {
        let current = match self.load_projection().await {
            Ok(projection) => projection,
            Err(error) => {
                tracing::warn!(error = %error, request_id = %request.request_id,
                    "enrollment terminal delivery fence reload failed");
                return true;
            }
        };
        let exact_current = terminal_is_exact_current(
            &current,
            request,
            decision_doc_id,
            revision_doc_id,
            route_receipt_doc_id,
        );
        if !exact_current {
            tracing::warn!(request_id = %request.request_id,
                "suppressed stale enrollment terminal delivery");
            return true;
        }
        let Some(p2p) = crate::node_access::operations(&self.node) else {
            return true;
        };
        let documents = terminal_documents(decision_doc_id, revision_doc_id, route_receipt_doc_id);
        let delivery = async {
            p2p.connect_peer(&request.candidate_ticket)
                .await
                .map_err(anyhow::Error::msg)?;
            p2p.push_documents_to_peer(&request.candidate_peer, documents)
                .await
                .map_err(anyhow::Error::msg)
        };
        match timeout(Duration::from_secs(20), delivery).await {
            Ok(Ok(())) => false,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, request_id = %request.request_id, "enrollment terminal committed; direct delivery will retry");
                true
            }
            Err(_) => {
                tracing::warn!(request_id = %request.request_id, "enrollment terminal committed; direct delivery timed out and will retry");
                true
            }
        }
    }
}

fn terminal_documents(
    decision_doc_id: &str,
    revision_doc_id: Option<&str>,
    route_receipt_doc_id: Option<&str>,
) -> Vec<P2pDocumentRequest> {
    let mut documents = vec![P2pDocumentRequest {
        collection: "NetworkEnrollmentDecision".to_string(),
        doc_id: decision_doc_id.to_string(),
    }];
    if let Some(revision_doc_id) = revision_doc_id {
        documents.push(P2pDocumentRequest {
            collection: "NetworkAuthorizationRevision".to_string(),
            doc_id: revision_doc_id.to_string(),
        });
    }
    if let Some(route_receipt_doc_id) = route_receipt_doc_id {
        documents.push(P2pDocumentRequest {
            collection: "NetworkEnrollmentRouteReceipt".to_string(),
            doc_id: route_receipt_doc_id.to_string(),
        });
    }
    documents
}

fn terminal_is_exact_current(
    projection: &EnrollmentProjection,
    request: &EnrollmentRequestRecord,
    decision_doc_id: &str,
    revision_doc_id: Option<&str>,
    route_receipt_doc_id: Option<&str>,
) -> bool {
    projection.active.iter().any(|active| {
        active.request == *request
            && active.decision_doc_id == decision_doc_id
            && revision_doc_id == Some(active.revision_doc_id.as_str())
            && route_receipt_doc_id == active.route_receipt_doc_id.as_deref()
    }) || projection.denied.iter().any(|denied| {
        denied.request == *request
            && denied.decision_doc_id == decision_doc_id
            && revision_doc_id.is_none()
            && route_receipt_doc_id.is_none()
    }) || projection.revoked.iter().any(|revoked| {
        revoked.request == *request
            && revoked.decision_doc_id == decision_doc_id
            && revision_doc_id == Some(revoked.revision_doc_id.as_str())
            && route_receipt_doc_id.is_none()
    })
}
