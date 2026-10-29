use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{Duration, SecondsFormat, Utc};
use defra_p2p_adapter::P2POperations;
use gents_protocol::enrollment::{
    derive_enrollment_id, encode_offer, enrollment_schema_fingerprint, EnrollmentOfferRecord,
    ENROLLMENT_PROTOCOL_VERSION,
};
use p2p::iroh::parse_public_peer_addr;
use serde::Serialize;
use uuid::Uuid;

use crate::NodeIdentity;

/// The `/status` offer keeps a short window, because each holder can file a
/// pending request until it expires; an approver relaying offers by hand may
/// ask for up to [`MAX_OFFER_VALIDITY`]. Approval remains the only grant.
pub const DEFAULT_OFFER_VALIDITY: Duration = Duration::minutes(5);
/// Issuers and candidates enforce the same cap on the time an offer holder may
/// file pending requests; approval is still required to grant access.
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
        anyhow::ensure!(
            validity > Duration::zero() && validity <= MAX_OFFER_VALIDITY,
            "enrollment offer validity must be between 1 and {} seconds",
            MAX_OFFER_VALIDITY.num_seconds()
        );
        let server_peer = self
            .p2p
            .local_peer_id()
            .await
            .map_err(anyhow::Error::msg)
            .context("reading live server peer ID for enrollment offer")?;
        let server_ticket = self
            .p2p
            .shareable_address()
            .await
            .map_err(anyhow::Error::msg)
            .context("reading live server ticket for enrollment offer")?
            .context("server has no shareable P2P address for enrollment")?;
        let (ticket_peer, _) = parse_public_peer_addr(&server_ticket)
            .context("server produced an invalid shareable P2P ticket")?;
        anyhow::ensure!(
            ticket_peer.to_string() == server_peer,
            "server shareable ticket does not match its live peer ID"
        );
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
            .context("signing authenticated enrollment offer")?;
        let token = encode_offer(&offer)?;
        Ok(EnrollmentOfferStatus { token, offer })
    }
}
