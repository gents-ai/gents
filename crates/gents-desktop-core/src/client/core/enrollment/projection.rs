use std::collections::BTreeMap;

use anyhow::{Context, Result};
use defra_node::QueryResponse;
use gents::agent::p2p_reconcile::enrollment::{
    DurableEnrollmentDocuments, EnrollmentOffer as PureOffer, EnrollmentRequest as PureRequest,
    NetworkAdminPin as PureAdminPin,
};
use gents::store::graphql::rows;
use gents::NodeIdentity;
use gents_protocol::enrollment::{
    decode_offer, enrollment_schema_fingerprint, EnrollmentDecisionRecord, EnrollmentRequestRecord,
};
use p2p::iroh::parse_public_peer_addr;
use serde_json::Value;

use super::super::super::principal_identity::PrincipalIdentity;
use super::reconcile::{ApprovedStatusEnrollment, DesktopApprovalProjection};
use super::rows::{
    to_pure_decision, to_pure_offer, to_pure_receipt, to_pure_request, to_pure_revision,
    EnrollmentDecisionRow, EnrollmentRequestRow, EnrollmentRevisionRow, EnrollmentRouteReceiptRow,
};

#[expect(
    clippy::too_many_arguments,
    reason = "the projection reads each enrollment row set separately"
)]
pub(super) async fn project_desktop_approval(
    request_row: &EnrollmentRequestRow,
    request_rows: &[EnrollmentRequestRow],
    decision_rows: &[EnrollmentDecisionRow],
    revision_rows: &[EnrollmentRevisionRow],
    receipt_rows: &[EnrollmentRouteReceiptRow],
    pins: &BTreeMap<String, Vec<String>>,
    principal: &PrincipalIdentity,
    local_peer_id: &str,
) -> Result<DesktopApprovalProjection> {
    let request = request_row.to_record()?;
    anyhow::ensure!(
        request.candidate_did == principal.did() && request.candidate_peer == local_peer_id,
        "enrollment request is not owned by this desktop identity and transport"
    );
    let [admin_did] = pins
        .get(&request.network_id)
        .map(Vec::as_slice)
        .unwrap_or_default()
    else {
        anyhow::bail!("network has no unique durable admin pin");
    };
    anyhow::ensure!(
        admin_did == &request.admin_did,
        "request admin does not match the durable network pin"
    );

    let offer =
        decode_offer(&request.offer_token).context("decoding persisted enrollment offer")?;
    request
        .validate_against_offer(&offer)
        .context("validating persisted enrollment request against offer")?;
    anyhow::ensure!(
        offer.schema_fingerprint == enrollment_schema_fingerprint(),
        "persisted enrollment offer has an incompatible schema"
    );
    let (server_ticket_peer, _) = parse_public_peer_addr(&offer.server_ticket)
        .context("persisted enrollment offer contains an invalid server ticket")?;
    anyhow::ensure!(
        server_ticket_peer.to_string() == request.server_peer,
        "persisted server ticket is bound to another transport peer"
    );
    anyhow::ensure!(
        principal
            .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
            .await?,
        "persisted enrollment offer signature is invalid"
    );
    anyhow::ensure!(
        principal
            .verify(
                &request.candidate_did,
                &request.signing_payload(),
                &request.candidate_sig,
            )
            .await?,
        "persisted enrollment request signature is invalid"
    );

    let mut projection = assemble_durable_enrollment_documents(
        &offer,
        &request,
        admin_did,
        request_rows,
        decision_rows,
        revision_rows,
        principal,
        local_peer_id,
    )
    .await?;

    let mut receipts = Vec::new();
    for row in receipt_rows.iter().filter(|row| {
        row.network_id == request.network_id
            && row.request_id == request.request_id
            && (row.member_did == principal.did() || row.member_peer == local_peer_id)
    }) {
        let receipt = row.to_record()?;
        let verified = principal
            .verify(
                &receipt.signer_did,
                &receipt.signing_payload(),
                &receipt.admin_sig,
            )
            .await
            .unwrap_or(false);
        projection
            .documents
            .route_receipts
            .insert(to_pure_receipt(&receipt, verified));
        receipts.push((receipt, verified));
    }

    let mut current_without_route = false;
    for (decision, decision_verified) in projection.decisions {
        let pure_decision = to_pure_decision(&decision, decision_verified);
        if !projection.documents.current_approval(
            &projection.offer,
            &projection.request,
            &pure_decision,
        ) {
            continue;
        }
        let has_current_receipt = receipts.iter().any(|(receipt, verified)| {
            projection.documents.current_server_route_receipt(
                &projection.offer,
                &projection.request,
                &pure_decision,
                &to_pure_receipt(receipt, *verified),
            )
        });
        if has_current_receipt {
            return Ok(DesktopApprovalProjection::Routed(Box::new(
                ApprovedStatusEnrollment {
                    network_id: request.network_id,
                    request_id: request.request_id,
                    server_peer: request.server_peer,
                    server_ticket: offer.server_ticket,
                    admin_did: request.admin_did,
                    owner_node: request.owner_agent,
                    request_digest: request.request_digest,
                    authorization_sequence: decision.authorization_sequence,
                    authorization_expires_at: decision.authorization_expires_at,
                    decided_at: decision.decided_at,
                },
            )));
        }
        current_without_route = true;
    }
    Ok(if current_without_route {
        DesktopApprovalProjection::CurrentWithoutRoute
    } else {
        DesktopApprovalProjection::Absent
    })
}

// The reconciler and the active-request listing must derive approval from
// this same assembly, or they can disagree about the same signed documents.
pub(super) struct DurableEnrollmentProjection {
    pub(super) documents: DurableEnrollmentDocuments,
    pub(super) offer: PureOffer,
    pub(super) request: PureRequest,
    pub(super) decisions: Vec<(EnrollmentDecisionRecord, bool)>,
}

pub(super) fn listing_revision_rows(
    response: &QueryResponse,
) -> Result<Vec<EnrollmentRevisionRow>> {
    Ok(rows::<Value>(response, "NetworkAuthorizationRevision")?
        .into_iter()
        .filter_map(|raw| {
            let row = serde_json::from_value::<EnrollmentRevisionRow>(raw).ok()?;
            row.to_record().ok()?;
            Some(row)
        })
        .collect())
}

// A foreign candidate request that fails to decode or verify must not fail
// the whole listing; the assembly propagates same-server rows so the
// reconciler can turn them into per-scope conflicts.
pub(super) async fn listing_assembly_requests(
    request_rows: &[EnrollmentRequestRow],
    principal: &PrincipalIdentity,
) -> Vec<EnrollmentRequestRow> {
    let mut contained = Vec::new();
    for row in request_rows {
        let Ok(candidate) = row.to_record() else {
            continue;
        };
        if principal
            .verify(
                &candidate.candidate_did,
                &candidate.signing_payload(),
                &candidate.candidate_sig,
            )
            .await
            .is_err()
        {
            continue;
        }
        contained.push(row.clone());
    }
    contained
}

#[expect(
    clippy::too_many_arguments,
    reason = "the projection reads each enrollment row set separately"
)]
pub(super) async fn assemble_durable_enrollment_documents(
    offer: &gents_protocol::enrollment::EnrollmentOfferRecord,
    request: &EnrollmentRequestRecord,
    admin_did: &str,
    request_rows: &[EnrollmentRequestRow],
    decision_rows: &[EnrollmentDecisionRow],
    revision_rows: &[EnrollmentRevisionRow],
    principal: &PrincipalIdentity,
    local_peer_id: &str,
) -> Result<DurableEnrollmentProjection> {
    let pure_offer = to_pure_offer(offer, true);
    let pure_request = to_pure_request(request, true, true);
    let mut documents = DurableEnrollmentDocuments::default();
    documents.offers.insert(pure_offer.clone());
    documents.admin_pins.insert(PureAdminPin {
        network_id: request.network_id.clone(),
        admin_did: admin_did.to_string(),
    });
    for row in request_rows {
        let candidate = match row.to_record() {
            Ok(candidate) => candidate,
            Err(error) if row.server_peer == request.server_peer => return Err(error),
            Err(_) => continue,
        };
        let verified = principal
            .verify(
                &candidate.candidate_did,
                &candidate.signing_payload(),
                &candidate.candidate_sig,
            )
            .await;
        let verified = match verified {
            Ok(verified) => verified,
            Err(error) if candidate.server_peer == request.server_peer => return Err(error.into()),
            Err(_) => continue,
        };
        if verified || candidate.server_peer == request.server_peer {
            documents
                .requests
                .insert(to_pure_request(&candidate, verified, true));
        }
    }

    let mut decisions = Vec::new();
    for row in decision_rows
        .iter()
        .filter(|row| row.request_id == request.request_id)
    {
        let decision = row.to_record()?;
        let verified = principal
            .verify(
                &decision.signer_did,
                &decision.signing_payload(),
                &decision.admin_sig,
            )
            .await?;
        documents
            .decisions
            .insert(to_pure_decision(&decision, verified));
        decisions.push((decision, verified));
    }

    for row in revision_rows.iter().filter(|row| {
        row.network_id == request.network_id
            && (row.member_did == principal.did() || row.member_peer == local_peer_id)
    }) {
        let revision = row.to_record()?;
        let verified = principal
            .verify(
                &revision.signer_did,
                &revision.signing_payload(),
                &revision.admin_sig,
            )
            .await
            .unwrap_or(false);
        documents
            .revisions
            .insert(to_pure_revision(&revision, verified));
    }

    Ok(DurableEnrollmentProjection {
        documents,
        offer: pure_offer,
        request: pure_request,
        decisions,
    })
}
