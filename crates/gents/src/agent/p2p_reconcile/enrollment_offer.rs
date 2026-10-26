use std::sync::Arc;

use chrono::{Duration, SecondsFormat, Utc};
use defra_p2p_adapter::P2POperations;
use gents_protocol::enrollment::{
    derive_enrollment_id, encode_offer, enrollment_schema_fingerprint, EnrollmentOfferRecord,
    ENROLLMENT_PROTOCOL_VERSION,
};
use p2p::iroh::parse_public_peer_addr;
use serde::Serialize;
use uuid::Uuid;

use super::error::{P2pReconcileError, Result};
use crate::NodeIdentity;

/// The `/status` offer keeps a short window, because each holder can file a
/// pending request until it expires; an approver relaying offers by hand may
/// ask for up to [`MAX_OFFER_VALIDITY`]. Approval remains the only grant.
pub const DEFAULT_OFFER_VALIDITY: Duration = Duration::minutes(5);
pub const MAX_OFFER_VALIDITY: Duration = Duration::hours(1);

#[derive(Clone)]
pub struct EnrollmentOfferIssuer {
    identity: Arc<dyn NodeIdentity>,
    p2p: Arc<dyn P2POperations>,
    network_id: String,
    owner_agent: String,
    profile: String,
}

#[derive(Debug, Serialize)]
pub struct EnrollmentOfferStatus {
    pub token: String,
    pub offer: EnrollmentOfferRecord,
}

impl EnrollmentOfferIssuer {
    pub fn new(
        identity: Arc<dyn NodeIdentity>,
        p2p: Arc<dyn P2POperations>,
        network_id: String,
        owner_agent: String,
        profile: String,
    ) -> Self {
        Self {
            identity,
            p2p,
            network_id,
            owner_agent,
            profile,
        }
    }

    pub async fn mint(&self) -> Result<EnrollmentOfferStatus> {
        self.mint_valid_for(DEFAULT_OFFER_VALIDITY).await
    }

    pub async fn mint_valid_for(&self, validity: Duration) -> Result<EnrollmentOfferStatus> {
        if validity <= Duration::zero() || validity > MAX_OFFER_VALIDITY {
            return Err(P2pReconcileError::OfferValidityOutOfRange);
        }
        let server_peer = self
            .p2p
            .local_peer_id()
            .await
            .map_err(|source| P2pReconcileError::ReadServerPeerId { source })?;
        let server_ticket = self
            .p2p
            .shareable_address()
            .await
            .map_err(|source| P2pReconcileError::ReadServerTicket { source })?
            .ok_or(P2pReconcileError::ServerHasNoShareableAddress)?;
        let (ticket_peer, _) = parse_public_peer_addr(&server_ticket)
            .map_err(|source| P2pReconcileError::InvalidServerTicket { source })?;
        if ticket_peer.to_string() != server_peer {
            return Err(P2pReconcileError::ServerTicketPeerMismatch);
        }
        let issued = Utc::now();
        let issued_at = issued.to_rfc3339_opts(SecondsFormat::Secs, true);
        let expires_at = (issued + validity).to_rfc3339_opts(SecondsFormat::Secs, true);
        let challenge = Uuid::new_v4().simple().to_string();
        let offer_id = format!(
            "offer-{}",
            derive_enrollment_id(
                "gents-enrollment-offer-v1",
                &[
                    &self.network_id,
                    self.identity.did(),
                    &server_peer,
                    &challenge,
                    &issued_at,
                ],
            )
        );
        let mut offer = EnrollmentOfferRecord {
            version: ENROLLMENT_PROTOCOL_VERSION,
            offer_id,
            challenge,
            network_id: self.network_id.clone(),
            admin_did: self.identity.did().to_string(),
            server_peer,
            server_ticket,
            owner_agent: self.owner_agent.clone(),
            profile: self.profile.clone(),
            schema_fingerprint: enrollment_schema_fingerprint(),
            issued_at,
            expires_at,
            admin_sig: Vec::new(),
        };
        offer.admin_sig = self
            .identity
            .sign(&offer.signing_payload())
            .await
            .map_err(|source| P2pReconcileError::SignEnrollmentOffer { source })?;
        let token = encode_offer(&offer)?;
        Ok(EnrollmentOfferStatus { token, offer })
    }
}
