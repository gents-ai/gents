use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents::agent::p2p_reconcile::enrollment::{
    AuthorizationRevision as PureRevision, AuthorizationRevisionKind as PureRevisionKind,
    EnrollmentDecision as PureDecision, EnrollmentDecisionKind as PureDecisionKind,
    EnrollmentOffer as PureOffer, EnrollmentRequest as PureRequest,
    EnrollmentRouteDirection as PureRouteDirection, EnrollmentRouteReceipt as PureRouteReceipt,
};
use gents_protocol::enrollment::{
    enrollment_schema_fingerprint, AuthorizationRevisionKind, AuthorizationRevisionRecord,
    EnrollmentDecisionKind, EnrollmentDecisionRecord, EnrollmentRequestRecord,
    EnrollmentRouteReceiptDirection, EnrollmentRouteReceiptRecord, ENROLLMENT_PROTOCOL_VERSION,
};
use serde::Deserialize;

pub(super) const STATUS_ENROLLMENT_QUERY: &str = r#"{
  NetworkAdminPin { network_id admin_did }
  NetworkEnrollmentRequest {
    _docID protocol_version request_id request_digest offer_id offer_token challenge network_id
    admin_did server_peer candidate_did candidate_peer candidate_ticket owner_agent profile
    client_nonce issued_at expires_at candidate_sig
  }
  NetworkEnrollmentDecision {
    protocol_version decision_id request_id request_digest network_id admin_did candidate_did
    candidate_peer owner_agent decision authorization_sequence authorization_expires_at
    decided_at signer_did admin_sig
  }
  NetworkAuthorizationRevision {
    protocol_version revision_id request_id request_digest network_id admin_did member_did
    member_peer owner_agent sequence authorization_expires_at kind issued_at signer_did admin_sig
  }
  NetworkEnrollmentRouteReceipt {
    protocol_version receipt_id request_id request_digest network_id admin_did member_did
    member_peer server_peer owner_agent authorization_sequence authorization_expires_at
    direction applied_at signer_did
    admin_sig
  }
}"#;

#[derive(Deserialize)]
pub(super) struct EnrollmentPinRow {
    pub(super) network_id: String,
    pub(super) admin_did: String,
}

#[derive(Clone, Deserialize)]
pub(super) struct EnrollmentRequestRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) protocol_version: i64,
    pub(super) request_id: String,
    pub(super) request_digest: String,
    pub(super) offer_id: String,
    pub(super) offer_token: String,
    pub(super) challenge: String,
    pub(super) network_id: String,
    pub(super) admin_did: String,
    pub(super) server_peer: String,
    pub(super) candidate_did: String,
    pub(super) candidate_peer: String,
    pub(super) candidate_ticket: String,
    pub(super) owner_agent: String,
    pub(super) profile: String,
    pub(super) client_nonce: String,
    pub(super) issued_at: String,
    pub(super) expires_at: String,
    pub(super) candidate_sig: String,
}

impl EnrollmentRequestRow {
    pub(super) fn to_record(&self) -> Result<EnrollmentRequestRecord> {
        Ok(EnrollmentRequestRecord {
            protocol_version: enrollment_version(self.protocol_version)?,
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            offer_id: self.offer_id.clone(),
            offer_token: self.offer_token.clone(),
            challenge: self.challenge.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            server_peer: self.server_peer.clone(),
            candidate_did: self.candidate_did.clone(),
            candidate_peer: self.candidate_peer.clone(),
            candidate_ticket: self.candidate_ticket.clone(),
            owner_agent: self.owner_agent.clone(),
            profile: self.profile.clone(),
            client_nonce: self.client_nonce.clone(),
            issued_at: self.issued_at.clone(),
            expires_at: self.expires_at.clone(),
            candidate_sig: enrollment_signature("request", &self.candidate_sig)?,
        })
    }
}

#[derive(Deserialize)]
pub(super) struct EnrollmentDecisionRow {
    pub(super) protocol_version: i64,
    pub(super) decision_id: String,
    pub(super) request_id: String,
    pub(super) request_digest: String,
    pub(super) network_id: String,
    pub(super) admin_did: String,
    pub(super) candidate_did: String,
    pub(super) candidate_peer: String,
    pub(super) owner_agent: String,
    pub(super) decision: String,
    pub(super) authorization_sequence: i64,
    pub(super) authorization_expires_at: String,
    pub(super) decided_at: String,
    pub(super) signer_did: String,
    pub(super) admin_sig: String,
}

impl EnrollmentDecisionRow {
    pub(super) fn to_record(&self) -> Result<EnrollmentDecisionRecord> {
        Ok(EnrollmentDecisionRecord {
            protocol_version: enrollment_version(self.protocol_version)?,
            decision_id: self.decision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            candidate_did: self.candidate_did.clone(),
            candidate_peer: self.candidate_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            decision: match self.decision.as_str() {
                "approved" => EnrollmentDecisionKind::Approved,
                "denied" => EnrollmentDecisionKind::Denied,
                other => anyhow::bail!("unknown enrollment decision {other:?}"),
            },
            authorization_sequence: u64::try_from(self.authorization_sequence)
                .context("negative enrollment decision sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            decided_at: self.decided_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: enrollment_signature("decision", &self.admin_sig)?,
        })
    }
}

#[derive(Deserialize)]
pub(super) struct EnrollmentRevisionRow {
    pub(super) protocol_version: i64,
    pub(super) revision_id: String,
    pub(super) request_id: String,
    pub(super) request_digest: String,
    pub(super) network_id: String,
    pub(super) admin_did: String,
    pub(super) member_did: String,
    pub(super) member_peer: String,
    pub(super) owner_agent: String,
    pub(super) sequence: i64,
    pub(super) authorization_expires_at: String,
    pub(super) kind: String,
    pub(super) issued_at: String,
    pub(super) signer_did: String,
    pub(super) admin_sig: String,
}

#[derive(Deserialize)]
pub(super) struct EnrollmentRouteReceiptRow {
    pub(super) protocol_version: i64,
    pub(super) receipt_id: String,
    pub(super) request_id: String,
    pub(super) request_digest: String,
    pub(super) network_id: String,
    pub(super) admin_did: String,
    pub(super) member_did: String,
    pub(super) member_peer: String,
    pub(super) server_peer: String,
    pub(super) owner_agent: String,
    pub(super) authorization_sequence: i64,
    pub(super) authorization_expires_at: String,
    pub(super) direction: String,
    pub(super) applied_at: String,
    pub(super) signer_did: String,
    pub(super) admin_sig: String,
}

impl EnrollmentRouteReceiptRow {
    pub(super) fn to_record(&self) -> Result<EnrollmentRouteReceiptRecord> {
        Ok(EnrollmentRouteReceiptRecord {
            protocol_version: enrollment_version(self.protocol_version)?,
            receipt_id: self.receipt_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            member_did: self.member_did.clone(),
            member_peer: self.member_peer.clone(),
            server_peer: self.server_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            authorization_sequence: u64::try_from(self.authorization_sequence)
                .context("negative enrollment route receipt sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            direction: match self.direction.as_str() {
                "client_to_server" => EnrollmentRouteReceiptDirection::ClientToServer,
                other => anyhow::bail!("unknown enrollment route receipt direction {other:?}"),
            },
            applied_at: self.applied_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: enrollment_signature("route receipt", &self.admin_sig)?,
        })
    }
}

impl EnrollmentRevisionRow {
    pub(super) fn to_record(&self) -> Result<AuthorizationRevisionRecord> {
        Ok(AuthorizationRevisionRecord {
            protocol_version: enrollment_version(self.protocol_version)?,
            revision_id: self.revision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            member_did: self.member_did.clone(),
            member_peer: self.member_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            sequence: u64::try_from(self.sequence)
                .context("negative enrollment revision sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            kind: match self.kind.as_str() {
                "active" => AuthorizationRevisionKind::Active,
                "revoked" => AuthorizationRevisionKind::Revoked,
                other => anyhow::bail!("unknown enrollment revision kind {other:?}"),
            },
            issued_at: self.issued_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: enrollment_signature("revision", &self.admin_sig)?,
        })
    }
}

pub(super) fn to_pure_offer(
    offer: &gents_protocol::enrollment::EnrollmentOfferRecord,
    verified: bool,
) -> PureOffer {
    PureOffer {
        offer_id: offer.offer_id.clone(),
        challenge: offer.challenge.clone(),
        network_id: offer.network_id.clone(),
        admin_did: offer.admin_did.clone(),
        server_peer: offer.server_peer.clone(),
        server_ticket_peer: offer.server_peer.clone(),
        resolved_server_did: if verified {
            offer.admin_did.clone()
        } else {
            Default::default()
        },
        owner_node: offer.owner_agent.clone(),
        profile: offer.profile.clone(),
        schema_compatible: offer.schema_fingerprint == enrollment_schema_fingerprint(),
        admin_signed: verified,
        fresh: verified,
    }
}

pub(super) fn to_pure_request(
    request: &EnrollmentRequestRecord,
    verified: bool,
    fresh: bool,
) -> PureRequest {
    PureRequest {
        request_id: request.request_id.clone(),
        digest: request.request_digest.clone(),
        offer_id: request.offer_id.clone(),
        challenge: request.challenge.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        server_peer: request.server_peer.clone(),
        candidate_did: request.candidate_did.clone(),
        candidate_peer: request.candidate_peer.clone(),
        observed_candidate_peer: if verified {
            request.candidate_peer.clone()
        } else {
            Default::default()
        },
        resolved_candidate_did: if verified {
            request.candidate_did.clone()
        } else {
            Default::default()
        },
        candidate_ticket_peer: request.candidate_peer.clone(),
        owner_node: request.owner_agent.clone(),
        profile: request.profile.clone(),
        client_nonce: request.client_nonce.clone(),
        issued_at: request.issued_at.clone(),
        expires_at: request.expires_at.clone(),
        candidate_signed: verified,
        fresh,
    }
}

pub(super) fn to_pure_decision(
    decision: &EnrollmentDecisionRecord,
    verified: bool,
) -> PureDecision {
    PureDecision {
        request_id: decision.request_id.clone(),
        request_digest: decision.request_digest.clone(),
        network_id: decision.network_id.clone(),
        admin_did: decision.admin_did.clone(),
        candidate_did: decision.candidate_did.clone(),
        candidate_peer: decision.candidate_peer.clone(),
        owner_node: decision.owner_agent.clone(),
        kind: match decision.decision {
            EnrollmentDecisionKind::Approved => PureDecisionKind::Approved,
            EnrollmentDecisionKind::Denied => PureDecisionKind::Denied,
        },
        authorization_sequence: decision.authorization_sequence as usize,
        authorization_expires_at: decision.authorization_expires_at.clone(),
        signer_did: decision.signer_did.clone(),
        admin_signed: verified,
        fresh: verified
            && DateTime::parse_from_rfc3339(&decision.authorization_expires_at)
                .map(|expires| Utc::now() < expires.with_timezone(&Utc))
                .unwrap_or(false),
    }
}

pub(super) fn to_pure_revision(
    revision: &AuthorizationRevisionRecord,
    verified: bool,
) -> PureRevision {
    PureRevision {
        request_id: revision.request_id.clone(),
        request_digest: revision.request_digest.clone(),
        network_id: revision.network_id.clone(),
        admin_did: revision.admin_did.clone(),
        member_did: revision.member_did.clone(),
        member_peer: revision.member_peer.clone(),
        owner_node: revision.owner_agent.clone(),
        sequence: revision.sequence as usize,
        authorization_expires_at: revision.authorization_expires_at.clone(),
        kind: match revision.kind {
            AuthorizationRevisionKind::Active => PureRevisionKind::Active,
            AuthorizationRevisionKind::Revoked => PureRevisionKind::Revoked,
        },
        signer_did: revision.signer_did.clone(),
        admin_signed: verified,
    }
}

pub(super) fn to_pure_receipt(
    receipt: &EnrollmentRouteReceiptRecord,
    verified: bool,
) -> PureRouteReceipt {
    PureRouteReceipt {
        request_id: receipt.request_id.clone(),
        request_digest: receipt.request_digest.clone(),
        network_id: receipt.network_id.clone(),
        admin_did: receipt.admin_did.clone(),
        member_did: receipt.member_did.clone(),
        member_peer: receipt.member_peer.clone(),
        server_peer: receipt.server_peer.clone(),
        owner_node: receipt.owner_agent.clone(),
        authorization_sequence: receipt.authorization_sequence as usize,
        authorization_expires_at: receipt.authorization_expires_at.clone(),
        direction: PureRouteDirection::ClientToServer,
        signer_did: receipt.signer_did.clone(),
        admin_signed: verified,
        applied: verified,
    }
}

fn enrollment_version(value: i64) -> Result<u8> {
    let value = u8::try_from(value).context("invalid enrollment protocol version")?;
    anyhow::ensure!(
        value == ENROLLMENT_PROTOCOL_VERSION,
        "unsupported enrollment protocol version {value}"
    );
    Ok(value)
}

fn enrollment_signature(kind: &str, value: &str) -> Result<Vec<u8>> {
    let signature = bs58::decode(value)
        .into_vec()
        .with_context(|| format!("decode enrollment {kind} signature"))?;
    anyhow::ensure!(signature.len() == 64, "invalid enrollment {kind} signature");
    Ok(signature)
}
