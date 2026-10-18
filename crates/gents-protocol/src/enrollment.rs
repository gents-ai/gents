use std::io::Cursor;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub const ENROLLMENT_PROTOCOL_VERSION: u8 = 2;
pub const DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS: u64 = 90 * 24 * 60 * 60;
pub const ENROLLMENT_DIGEST_DOMAIN: &str = "gents-enrollment-request-v1";
pub const ENROLLMENT_DIGEST_PREFIX: &str = "utf8hex-v1:";
const OFFER_SIGNATURE_DOMAIN: &str = "gents-enrollment-offer-signature-v1";
const REQUEST_SIGNATURE_DOMAIN: &str = "gents-enrollment-request-signature-v1";
const DECISION_SIGNATURE_DOMAIN: &str = "gents-enrollment-decision-signature-v1";
const REVISION_SIGNATURE_DOMAIN: &str = "gents-network-authorization-signature-v1";
const ROUTE_RECEIPT_SIGNATURE_DOMAIN: &str = "gents-enrollment-route-receipt-signature-v1";
const OPERATOR_DECISION_COMMAND_SIGNATURE_DOMAIN: &str =
    "gents-enrollment-operator-decision-command-v1";
const OPERATOR_QUERY_COMMAND_SIGNATURE_DOMAIN: &str = "gents-enrollment-operator-query-command-v1";
const MAX_OFFER_TOKEN_BYTES: usize = 32 * 1024;
const MAX_ENROLLMENT_FIELD_BYTES: usize = 16 * 1024;
const ENROLLMENT_SCHEMA_DOMAIN: &str = "gents-enrollment-schema-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentOfferRecord {
    pub version: u8,
    pub offer_id: String,
    pub challenge: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub server_ticket: String,
    pub owner_agent: String,
    pub profile: String,
    pub schema_fingerprint: String,
    pub issued_at: String,
    pub expires_at: String,
    pub admin_sig: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentRequestRecord {
    pub protocol_version: u8,
    pub request_id: String,
    pub request_digest: String,
    pub offer_id: String,
    pub offer_token: String,
    pub challenge: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub candidate_ticket: String,
    pub owner_agent: String,
    pub profile: String,
    pub client_nonce: String,
    pub issued_at: String,
    pub expires_at: String,
    pub candidate_sig: Vec<u8>,
}

impl EnrollmentRequestRecord {
    pub fn canonical_text_fields(&self) -> [&str; 13] {
        [
            &self.request_id,
            &self.offer_id,
            &self.challenge,
            &self.network_id,
            &self.admin_did,
            &self.server_peer,
            &self.candidate_did,
            &self.candidate_peer,
            &self.owner_agent,
            &self.profile,
            &self.client_nonce,
            &self.issued_at,
            &self.expires_at,
        ]
    }

    pub fn computed_digest(&self) -> String {
        canonical_enrollment_digest(self.canonical_text_fields())
    }

    pub fn validate_against_offer(&self, offer: &EnrollmentOfferRecord) -> Result<()> {
        anyhow::ensure!(
            self.protocol_version == ENROLLMENT_PROTOCOL_VERSION,
            "unsupported enrollment request version {}",
            self.protocol_version
        );
        anyhow::ensure!(self.offer_id == offer.offer_id, "request offer_id mismatch");
        anyhow::ensure!(
            self.challenge == offer.challenge,
            "request challenge mismatch"
        );
        anyhow::ensure!(
            self.network_id == offer.network_id,
            "request network_id mismatch"
        );
        anyhow::ensure!(
            self.admin_did == offer.admin_did,
            "request admin_did mismatch"
        );
        anyhow::ensure!(
            self.server_peer == offer.server_peer,
            "request server_peer mismatch"
        );
        anyhow::ensure!(
            self.owner_agent == offer.owner_agent,
            "request owner_agent mismatch"
        );
        anyhow::ensure!(self.profile == offer.profile, "request profile mismatch");
        anyhow::ensure!(
            self.expires_at == offer.expires_at,
            "request cannot extend the signed offer lifetime"
        );
        anyhow::ensure!(
            self.request_digest == self.computed_digest(),
            "request digest mismatch"
        );
        let expected_id = format!(
            "enroll-{}",
            derive_enrollment_id(
                "gents-enrollment-request-id-v1",
                &[
                    &self.offer_id,
                    &self.candidate_did,
                    &self.candidate_peer,
                    &self.client_nonce,
                ],
            )
        );
        anyhow::ensure!(self.request_id == expected_id, "request_id mismatch");
        for (name, value) in [
            ("request_id", self.request_id.as_str()),
            ("request_digest", self.request_digest.as_str()),
            ("offer_id", self.offer_id.as_str()),
            ("offer_token", self.offer_token.as_str()),
            ("challenge", self.challenge.as_str()),
            ("network_id", self.network_id.as_str()),
            ("admin_did", self.admin_did.as_str()),
            ("server_peer", self.server_peer.as_str()),
            ("candidate_did", self.candidate_did.as_str()),
            ("candidate_peer", self.candidate_peer.as_str()),
            ("candidate_ticket", self.candidate_ticket.as_str()),
            ("owner_agent", self.owner_agent.as_str()),
            ("profile", self.profile.as_str()),
            ("client_nonce", self.client_nonce.as_str()),
            ("issued_at", self.issued_at.as_str()),
            ("expires_at", self.expires_at.as_str()),
        ] {
            validate_exact_field(name, value)?;
        }
        anyhow::ensure!(
            self.candidate_sig.len() == 64,
            "invalid enrollment request signature length"
        );
        let offer_issued = parse_canonical_timestamp("offer issued_at", &offer.issued_at)?;
        let request_issued = parse_canonical_timestamp("request issued_at", &self.issued_at)?;
        let offer_expires = parse_canonical_timestamp("offer expires_at", &offer.expires_at)?;
        anyhow::ensure!(
            offer_issued <= request_issued && request_issued <= offer_expires,
            "request issuance is outside the signed offer window"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentDecisionKind {
    Approved,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentOperatorAction {
    Approve,
    Deny,
    Revoke,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentOperatorQuery {
    Pending,
}

impl EnrollmentOperatorQuery {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
        }
    }
}

impl EnrollmentOperatorAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Deny => "deny",
            Self::Revoke => "revoke",
        }
    }
}

impl EnrollmentDecisionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
        }
    }
}

/// Short-lived, admin-signed command accepted by the live runtime operator
/// surface; the durable decision and authorization revision remain the only
/// enrollment authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentOperatorDecisionCommand {
    pub protocol_version: u8,
    pub request_id: String,
    pub action: EnrollmentOperatorAction,
    /// Must be zero for deny/revoke.
    pub lease_seconds: u64,
    pub admin_did: String,
    pub issued_at: String,
    pub nonce: String,
    pub admin_sig: Vec<u8>,
}

impl EnrollmentOperatorDecisionCommand {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        canonical_domain_payload(
            OPERATOR_DECISION_COMMAND_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.request_id,
                self.action.as_str(),
                &self.lease_seconds.to_string(),
                &self.admin_did,
                &self.issued_at,
                &self.nonce,
            ],
        )
    }

    pub fn validate_at(&self, now: DateTime<Utc>) -> Result<()> {
        anyhow::ensure!(
            self.protocol_version == ENROLLMENT_PROTOCOL_VERSION,
            "unsupported operator decision command version {}",
            self.protocol_version
        );
        for (name, value) in [
            ("request_id", self.request_id.as_str()),
            ("admin_did", self.admin_did.as_str()),
            ("issued_at", self.issued_at.as_str()),
            ("nonce", self.nonce.as_str()),
        ] {
            validate_exact_field(name, value)?;
            anyhow::ensure!(
                value.chars().all(|character| !character.is_control()),
                "operator decision command {name} contains control characters"
            );
        }
        anyhow::ensure!(
            self.admin_sig.len() == 64,
            "invalid operator decision command signature length"
        );
        match self.action {
            EnrollmentOperatorAction::Approve => anyhow::ensure!(
                (1..=MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS).contains(&self.lease_seconds),
                "approval lease_seconds is outside the supported range"
            ),
            EnrollmentOperatorAction::Deny | EnrollmentOperatorAction::Revoke => anyhow::ensure!(
                self.lease_seconds == 0,
                "deny/revoke command cannot allocate a lease"
            ),
        }
        let issued = parse_canonical_timestamp("operator command issued_at", &self.issued_at)?
            .with_timezone(&Utc);
        anyhow::ensure!(
            issued <= now + chrono::Duration::seconds(5),
            "operator decision command is issued in the future"
        );
        anyhow::ensure!(
            now - issued <= chrono::Duration::seconds(60),
            "operator decision command expired"
        );
        Ok(())
    }
}

/// Short-lived signed reads keep pending enrollment identities off unauthenticated endpoints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentOperatorQueryCommand {
    pub protocol_version: u8,
    pub query: EnrollmentOperatorQuery,
    pub admin_did: String,
    pub issued_at: String,
    pub nonce: String,
    pub admin_sig: Vec<u8>,
}

impl EnrollmentOperatorQueryCommand {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        canonical_domain_payload(
            OPERATOR_QUERY_COMMAND_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                self.query.as_str(),
                &self.admin_did,
                &self.issued_at,
                &self.nonce,
            ],
        )
    }

    pub fn validate_at(&self, now: DateTime<Utc>) -> Result<()> {
        anyhow::ensure!(
            self.protocol_version == ENROLLMENT_PROTOCOL_VERSION,
            "unsupported operator query command version {}",
            self.protocol_version
        );
        for (name, value) in [
            ("admin_did", self.admin_did.as_str()),
            ("issued_at", self.issued_at.as_str()),
            ("nonce", self.nonce.as_str()),
        ] {
            validate_exact_field(name, value)?;
            anyhow::ensure!(
                value.chars().all(|character| !character.is_control()),
                "operator query command {name} contains control characters"
            );
        }
        anyhow::ensure!(
            self.admin_sig.len() == 64,
            "invalid operator query command signature length"
        );
        let issued = parse_canonical_timestamp("operator query issued_at", &self.issued_at)?
            .with_timezone(&Utc);
        anyhow::ensure!(
            issued <= now + chrono::Duration::seconds(5),
            "operator query command is issued in the future"
        );
        anyhow::ensure!(
            now - issued <= chrono::Duration::seconds(60),
            "operator query command expired"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentDecisionRecord {
    pub protocol_version: u8,
    pub decision_id: String,
    pub request_id: String,
    pub request_digest: String,
    pub network_id: String,
    pub admin_did: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub owner_agent: String,
    pub decision: EnrollmentDecisionKind,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
    pub decided_at: String,
    pub signer_did: String,
    pub admin_sig: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationRevisionKind {
    Active,
    Revoked,
}

impl AuthorizationRevisionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationRevisionRecord {
    pub protocol_version: u8,
    pub revision_id: String,
    pub request_id: String,
    pub request_digest: String,
    pub network_id: String,
    pub admin_did: String,
    pub member_did: String,
    pub member_peer: String,
    pub owner_agent: String,
    pub sequence: u64,
    pub authorization_expires_at: String,
    pub kind: AuthorizationRevisionKind,
    pub issued_at: String,
    pub signer_did: String,
    pub admin_sig: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentRouteReceiptRecord {
    pub protocol_version: u8,
    pub receipt_id: String,
    pub request_id: String,
    pub request_digest: String,
    pub network_id: String,
    pub admin_did: String,
    pub member_did: String,
    pub member_peer: String,
    pub server_peer: String,
    pub owner_agent: String,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
    pub direction: EnrollmentRouteReceiptDirection,
    pub applied_at: String,
    pub signer_did: String,
    pub admin_sig: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentRouteReceiptDirection {
    ClientToServer,
}

impl EnrollmentRouteReceiptDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClientToServer => "client_to_server",
        }
    }
}

impl EnrollmentOfferRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.version.to_string();
        canonical_domain_payload(
            OFFER_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.offer_id,
                &self.challenge,
                &self.network_id,
                &self.admin_did,
                &self.server_peer,
                &self.server_ticket,
                &self.owner_agent,
                &self.profile,
                &self.schema_fingerprint,
                &self.issued_at,
                &self.expires_at,
            ],
        )
    }
}

impl EnrollmentRequestRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        canonical_domain_payload(
            REQUEST_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.request_id,
                &self.request_digest,
                &self.offer_id,
                &self.offer_token,
                &self.challenge,
                &self.network_id,
                &self.admin_did,
                &self.server_peer,
                &self.candidate_did,
                &self.candidate_peer,
                &self.candidate_ticket,
                &self.owner_agent,
                &self.profile,
                &self.client_nonce,
                &self.issued_at,
                &self.expires_at,
            ],
        )
    }
}

impl EnrollmentDecisionRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        let sequence = self.authorization_sequence.to_string();
        canonical_domain_payload(
            DECISION_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.decision_id,
                &self.request_id,
                &self.request_digest,
                &self.network_id,
                &self.admin_did,
                &self.candidate_did,
                &self.candidate_peer,
                &self.owner_agent,
                self.decision.as_str(),
                sequence.as_str(),
                &self.authorization_expires_at,
                &self.decided_at,
                &self.signer_did,
            ],
        )
    }

    pub fn validate_against_request(&self, request: &EnrollmentRequestRecord) -> Result<()> {
        anyhow::ensure!(
            self.protocol_version == ENROLLMENT_PROTOCOL_VERSION,
            "unsupported enrollment decision version {}",
            self.protocol_version
        );
        anyhow::ensure!(
            self.request_id == request.request_id,
            "decision request_id mismatch"
        );
        anyhow::ensure!(
            self.request_digest == request.request_digest,
            "decision request_digest mismatch"
        );
        anyhow::ensure!(
            self.network_id == request.network_id,
            "decision network_id mismatch"
        );
        anyhow::ensure!(
            self.admin_did == request.admin_did,
            "decision admin_did mismatch"
        );
        anyhow::ensure!(
            self.candidate_did == request.candidate_did,
            "decision candidate_did mismatch"
        );
        anyhow::ensure!(
            self.candidate_peer == request.candidate_peer,
            "decision candidate_peer mismatch"
        );
        anyhow::ensure!(
            self.owner_agent == request.owner_agent,
            "decision owner_agent mismatch"
        );
        anyhow::ensure!(
            self.signer_did == request.admin_did,
            "decision signer mismatch"
        );
        anyhow::ensure!(
            self.decision_id == derive_decision_id(&request.request_id, &request.request_digest),
            "decision_id mismatch"
        );
        match self.decision {
            EnrollmentDecisionKind::Approved => {
                anyhow::ensure!(
                    self.authorization_sequence > 0,
                    "approved decision requires a positive authorization sequence"
                );
                let expires = parse_canonical_timestamp(
                    "decision authorization_expires_at",
                    &self.authorization_expires_at,
                )?;
                let decided = parse_canonical_timestamp("decision decided_at", &self.decided_at)?;
                anyhow::ensure!(
                    expires > decided,
                    "approval lease must expire after its decision"
                );
                anyhow::ensure!(
                    expires - decided
                        <= chrono::Duration::seconds(
                            MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS as i64
                        ),
                    "approval lease exceeds the supported maximum"
                );
            }
            EnrollmentDecisionKind::Denied => {
                anyhow::ensure!(
                    self.authorization_sequence == 0,
                    "denied decision cannot allocate an authorization sequence"
                );
                anyhow::ensure!(
                    self.authorization_expires_at == self.decided_at,
                    "denied decision must use its decision time as its non-leased boundary"
                );
                parse_canonical_timestamp(
                    "denied decision authorization_expires_at",
                    &self.authorization_expires_at,
                )?;
            }
        }
        anyhow::ensure!(
            self.authorization_sequence <= i64::MAX as u64,
            "decision authorization sequence exceeds the DefraDB Int range"
        );
        anyhow::ensure!(
            self.admin_sig.len() == 64,
            "invalid decision signature length"
        );
        let request_issued = parse_canonical_timestamp("request issued_at", &request.issued_at)?;
        let request_expires = parse_canonical_timestamp("request expires_at", &request.expires_at)?;
        let decided = parse_canonical_timestamp("decision decided_at", &self.decided_at)?;
        anyhow::ensure!(
            request_issued <= decided && decided <= request_expires,
            "decision is outside the signed request window"
        );
        Ok(())
    }
}

impl AuthorizationRevisionRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        let sequence = self.sequence.to_string();
        canonical_domain_payload(
            REVISION_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.revision_id,
                &self.request_id,
                &self.request_digest,
                &self.network_id,
                &self.admin_did,
                &self.member_did,
                &self.member_peer,
                &self.owner_agent,
                sequence.as_str(),
                &self.authorization_expires_at,
                self.kind.as_str(),
                &self.issued_at,
                &self.signer_did,
            ],
        )
    }

    pub fn validate_against_approval(
        &self,
        request: &EnrollmentRequestRecord,
        decision: &EnrollmentDecisionRecord,
    ) -> Result<()> {
        anyhow::ensure!(
            self.protocol_version == ENROLLMENT_PROTOCOL_VERSION,
            "unsupported authorization revision version {}",
            self.protocol_version
        );
        anyhow::ensure!(
            decision.decision == EnrollmentDecisionKind::Approved,
            "authorization revision requires an approved decision"
        );
        anyhow::ensure!(
            self.request_id == request.request_id,
            "revision request_id mismatch"
        );
        anyhow::ensure!(
            self.request_digest == request.request_digest,
            "revision request_digest mismatch"
        );
        anyhow::ensure!(
            self.network_id == request.network_id,
            "revision network_id mismatch"
        );
        anyhow::ensure!(
            self.admin_did == request.admin_did,
            "revision admin_did mismatch"
        );
        anyhow::ensure!(
            self.member_did == request.candidate_did,
            "revision member_did mismatch"
        );
        anyhow::ensure!(
            self.member_peer == request.candidate_peer,
            "revision member_peer mismatch"
        );
        anyhow::ensure!(
            self.owner_agent == request.owner_agent,
            "revision owner_agent mismatch"
        );
        anyhow::ensure!(
            self.signer_did == request.admin_did,
            "revision signer mismatch"
        );
        anyhow::ensure!(
            self.sequence > 0,
            "authorization revision sequence must be positive"
        );
        anyhow::ensure!(
            self.sequence <= i64::MAX as u64,
            "authorization revision sequence exceeds the DefraDB Int range"
        );
        anyhow::ensure!(
            self.authorization_expires_at == decision.authorization_expires_at,
            "revision authorization lease mismatch"
        );
        anyhow::ensure!(
            self.revision_id
                == derive_revision_id(
                    &self.network_id,
                    &self.member_did,
                    self.sequence,
                    &self.kind,
                    &self.request_digest,
                ),
            "revision_id mismatch"
        );
        match self.kind {
            AuthorizationRevisionKind::Active => {
                anyhow::ensure!(
                    self.sequence == decision.authorization_sequence,
                    "active revision sequence must equal its approval"
                );
                anyhow::ensure!(
                    self.issued_at == decision.decided_at,
                    "active revision timestamp must equal its approval"
                );
            }
            AuthorizationRevisionKind::Revoked => {
                anyhow::ensure!(
                    self.sequence > decision.authorization_sequence,
                    "revocation must dominate its approval"
                );
                let decision_time =
                    parse_canonical_timestamp("decision decided_at", &decision.decided_at)?;
                let revision_time =
                    parse_canonical_timestamp("revision issued_at", &self.issued_at)?;
                anyhow::ensure!(
                    revision_time >= decision_time,
                    "revocation cannot predate its approval"
                );
            }
        }
        anyhow::ensure!(
            self.admin_sig.len() == 64,
            "invalid revision signature length"
        );
        parse_canonical_timestamp("revision issued_at", &self.issued_at)?;
        Ok(())
    }
}

impl EnrollmentRouteReceiptRecord {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.protocol_version.to_string();
        let sequence = self.authorization_sequence.to_string();
        canonical_domain_payload(
            ROUTE_RECEIPT_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.receipt_id,
                &self.request_id,
                &self.request_digest,
                &self.network_id,
                &self.admin_did,
                &self.member_did,
                &self.member_peer,
                &self.server_peer,
                &self.owner_agent,
                sequence.as_str(),
                &self.authorization_expires_at,
                self.direction.as_str(),
                &self.applied_at,
                &self.signer_did,
            ],
        )
    }

    pub fn validate_against_approval(
        &self,
        request: &EnrollmentRequestRecord,
        decision: &EnrollmentDecisionRecord,
    ) -> Result<()> {
        anyhow::ensure!(
            self.protocol_version == ENROLLMENT_PROTOCOL_VERSION,
            "unsupported enrollment route receipt version {}",
            self.protocol_version
        );
        decision.validate_against_request(request)?;
        anyhow::ensure!(
            decision.decision == EnrollmentDecisionKind::Approved,
            "route receipt requires an approved decision"
        );
        anyhow::ensure!(
            self.request_id == request.request_id,
            "receipt request_id mismatch"
        );
        anyhow::ensure!(
            self.request_digest == request.request_digest,
            "receipt request_digest mismatch"
        );
        anyhow::ensure!(
            self.network_id == request.network_id,
            "receipt network_id mismatch"
        );
        anyhow::ensure!(
            self.admin_did == request.admin_did,
            "receipt admin_did mismatch"
        );
        anyhow::ensure!(
            self.member_did == request.candidate_did,
            "receipt member_did mismatch"
        );
        anyhow::ensure!(
            self.member_peer == request.candidate_peer,
            "receipt member_peer mismatch"
        );
        anyhow::ensure!(
            self.server_peer == request.server_peer,
            "receipt server_peer mismatch"
        );
        anyhow::ensure!(
            self.owner_agent == request.owner_agent,
            "receipt owner_agent mismatch"
        );
        anyhow::ensure!(
            self.authorization_sequence == decision.authorization_sequence,
            "receipt authorization sequence mismatch"
        );
        anyhow::ensure!(
            self.authorization_expires_at == decision.authorization_expires_at,
            "receipt authorization lease mismatch"
        );
        anyhow::ensure!(
            self.signer_did == request.admin_did,
            "receipt signer mismatch"
        );
        anyhow::ensure!(
            self.receipt_id
                == derive_route_receipt_id(
                    &self.request_id,
                    &self.request_digest,
                    self.authorization_sequence,
                    &self.direction,
                ),
            "receipt_id mismatch"
        );
        let decided = parse_canonical_timestamp("decision decided_at", &decision.decided_at)?;
        let applied = parse_canonical_timestamp("receipt applied_at", &self.applied_at)?;
        anyhow::ensure!(applied >= decided, "route receipt cannot predate approval");
        let expires = parse_canonical_timestamp(
            "receipt authorization_expires_at",
            &self.authorization_expires_at,
        )?;
        anyhow::ensure!(
            applied < expires,
            "route receipt cannot be issued after lease expiry"
        );
        anyhow::ensure!(
            self.admin_sig.len() == 64,
            "invalid route receipt signature length"
        );
        Ok(())
    }
}

pub fn derive_decision_id(request_id: &str, request_digest: &str) -> String {
    format!(
        "decision-{}",
        derive_enrollment_id(
            "gents-enrollment-decision-id-v1",
            &[request_id, request_digest],
        )
    )
}

pub fn derive_revision_id(
    network_id: &str,
    member_did: &str,
    sequence: u64,
    kind: &AuthorizationRevisionKind,
    request_digest: &str,
) -> String {
    let sequence = sequence.to_string();
    format!(
        "authorization-{}",
        derive_enrollment_id(
            "gents-network-authorization-id-v1",
            &[
                network_id,
                member_did,
                &sequence,
                kind.as_str(),
                request_digest
            ],
        )
    )
}

pub fn derive_route_receipt_id(
    request_id: &str,
    request_digest: &str,
    authorization_sequence: u64,
    direction: &EnrollmentRouteReceiptDirection,
) -> String {
    let sequence = authorization_sequence.to_string();
    format!(
        "route-receipt-{}",
        derive_enrollment_id(
            "gents-enrollment-route-receipt-id-v1",
            &[request_id, request_digest, &sequence, direction.as_str()],
        )
    )
}

pub fn canonical_enrollment_payload<'a>(fields: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    canonical_domain_payload(ENROLLMENT_DIGEST_DOMAIN, fields)
}

pub fn canonical_domain_payload<'a>(
    domain: &'a str,
    fields: impl IntoIterator<Item = &'a str>,
) -> Vec<u8> {
    let fields = std::iter::once(domain).chain(fields).collect::<Vec<_>>();
    let mut encoded = encode_wire_length(fields.len());
    for field in fields {
        encoded.extend(frame_enrollment_field(field));
    }
    encoded
}

pub fn frame_enrollment_field(value: &str) -> Vec<u8> {
    let mut encoded = encode_wire_length(value.len());
    encoded.extend_from_slice(value.as_bytes());
    encoded
}

pub fn canonical_enrollment_digest<'a>(fields: impl IntoIterator<Item = &'a str>) -> String {
    format!(
        "{ENROLLMENT_DIGEST_PREFIX}{}",
        lower_hex(&canonical_enrollment_payload(fields))
    )
}

pub fn derive_enrollment_id(domain: &str, fields: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(canonical_domain_payload(domain, fields.iter().copied()));
    bs58::encode(&hash.finalize()[..20]).into_string()
}

/// Shared observation-time interpretation of a signed authorization lease.
/// Invalid or non-canonical timestamps fail closed.
pub fn authorization_lease_is_fresh_at(expires_at: &str, now: DateTime<Utc>) -> bool {
    parse_canonical_timestamp("authorization_expires_at", expires_at)
        .ok()
        .is_some_and(|expires_at| now < expires_at.with_timezone(&Utc))
}

/// Fingerprint of the exact durable enrollment wire schemas: carried in signed
/// offers so a client cannot write a request whose immutable fields the server
/// interprets under a different schema.
pub fn enrollment_schema_fingerprint() -> String {
    let mut hash = Sha256::new();
    hash.update(canonical_domain_payload(
        ENROLLMENT_SCHEMA_DOMAIN,
        [
            gents_schemas::NETWORK_ADMIN_PIN,
            gents_schemas::NETWORK_ENROLLMENT_REQUEST,
            gents_schemas::NETWORK_ENROLLMENT_DECISION,
            gents_schemas::NETWORK_AUTHORIZATION_REVISION,
            gents_schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT,
            OFFER_SIGNATURE_DOMAIN,
            REQUEST_SIGNATURE_DOMAIN,
            DECISION_SIGNATURE_DOMAIN,
            REVISION_SIGNATURE_DOMAIN,
            ROUTE_RECEIPT_SIGNATURE_DOMAIN,
        ],
    ));
    format!("sha256:{}", lower_hex(&hash.finalize()))
}

pub fn encode_offer(offer: &EnrollmentOfferRecord) -> Result<String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(offer, &mut bytes).context("encoding enrollment offer")?;
    Ok(bs58::encode(bytes).into_string())
}

pub fn decode_offer(encoded: &str) -> Result<EnrollmentOfferRecord> {
    anyhow::ensure!(!encoded.is_empty(), "enrollment offer is empty");
    anyhow::ensure!(encoded == encoded.trim(), "enrollment offer has whitespace");
    anyhow::ensure!(
        encoded.len() <= MAX_OFFER_TOKEN_BYTES,
        "enrollment offer exceeds the size limit"
    );
    let bytes = bs58::decode(encoded)
        .into_vec()
        .context("decoding enrollment offer")?;
    anyhow::ensure!(
        bytes.len() <= MAX_OFFER_TOKEN_BYTES,
        "decoded enrollment offer exceeds the size limit"
    );
    let mut cursor = Cursor::new(bytes.as_slice());
    let offer: EnrollmentOfferRecord =
        ciborium::de::from_reader(&mut cursor).context("parsing enrollment offer")?;
    anyhow::ensure!(
        cursor.position() == bytes.len() as u64,
        "enrollment offer contains trailing data"
    );
    anyhow::ensure!(
        offer.version == ENROLLMENT_PROTOCOL_VERSION,
        "unsupported enrollment offer version {}",
        offer.version
    );
    for (name, value) in [
        ("offer_id", offer.offer_id.as_str()),
        ("challenge", offer.challenge.as_str()),
        ("network_id", offer.network_id.as_str()),
        ("admin_did", offer.admin_did.as_str()),
        ("server_peer", offer.server_peer.as_str()),
        ("server_ticket", offer.server_ticket.as_str()),
        ("owner_agent", offer.owner_agent.as_str()),
        ("profile", offer.profile.as_str()),
        ("schema_fingerprint", offer.schema_fingerprint.as_str()),
        ("issued_at", offer.issued_at.as_str()),
        ("expires_at", offer.expires_at.as_str()),
    ] {
        validate_exact_field(name, value)?;
    }
    anyhow::ensure!(offer.profile == "client", "unsupported enrollment profile");
    anyhow::ensure!(
        offer.admin_sig.len() == 64,
        "invalid enrollment offer signature length"
    );
    Ok(offer)
}

fn validate_exact_field(name: &str, value: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "enrollment offer {name} is empty");
    anyhow::ensure!(
        value == value.trim(),
        "enrollment offer {name} has whitespace"
    );
    anyhow::ensure!(
        value.len() <= MAX_ENROLLMENT_FIELD_BYTES,
        "enrollment offer {name} exceeds the size limit"
    );
    Ok(())
}

fn parse_canonical_timestamp(name: &str, value: &str) -> Result<DateTime<FixedOffset>> {
    anyhow::ensure!(value.ends_with('Z'), "{name} must use canonical UTC form");
    let parsed = DateTime::parse_from_rfc3339(value).with_context(|| format!("parsing {name}"))?;
    anyhow::ensure!(
        parsed.to_rfc3339_opts(SecondsFormat::Secs, true) == value,
        "{name} must use canonical UTC seconds form"
    );
    Ok(parsed)
}

fn encode_wire_length(length: usize) -> Vec<u8> {
    let mut encoded = vec![0; length];
    encoded.push(0xff);
    encoded
}

fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut rendered = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        rendered.push(DIGITS[(byte >> 4) as usize] as char);
        rendered.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    rendered
}
