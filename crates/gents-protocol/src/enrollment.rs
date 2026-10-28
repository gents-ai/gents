use std::io::Cursor;

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

#[derive(Debug, thiserror::Error)]
pub enum EnrollmentError {
    #[error("unsupported enrollment request version {version}")]
    UnsupportedRequestVersion { version: u8 },
    #[error("request offer_id mismatch")]
    RequestOfferIdMismatch,
    #[error("request challenge mismatch")]
    RequestChallengeMismatch,
    #[error("request network_id mismatch")]
    RequestNetworkIdMismatch,
    #[error("request admin_did mismatch")]
    RequestAdminDidMismatch,
    #[error("request server_peer mismatch")]
    RequestServerPeerMismatch,
    #[error("request owner_agent mismatch")]
    RequestOwnerAgentMismatch,
    #[error("request profile mismatch")]
    RequestProfileMismatch,
    #[error("request cannot extend the signed offer lifetime")]
    RequestExtendsOffer,
    #[error("request digest mismatch")]
    RequestDigestMismatch,
    #[error("request_id mismatch")]
    RequestIdMismatch,
    #[error("enrollment offer {name} is empty")]
    OfferFieldEmpty { name: &'static str },
    #[error("enrollment offer {name} has whitespace")]
    OfferFieldWhitespace { name: &'static str },
    #[error("enrollment offer {name} exceeds the size limit")]
    OfferFieldTooLarge { name: &'static str },
    #[error("invalid enrollment request signature length")]
    RequestSignatureLength,
    #[error("request issuance is outside the signed offer window")]
    RequestOutsideOfferWindow,
    #[error("unsupported operator decision command version {version}")]
    UnsupportedOperatorDecisionVersion { version: u8 },
    #[error("operator decision command {name} contains control characters")]
    OperatorDecisionControlCharacters { name: &'static str },
    #[error("invalid operator decision command signature length")]
    OperatorDecisionSignatureLength,
    #[error("approval lease_seconds is outside the supported range")]
    ApprovalLeaseOutOfRange,
    #[error("deny/revoke command cannot allocate a lease")]
    DenyOrRevokeWithLease,
    #[error("operator decision command is issued in the future")]
    OperatorDecisionFromFuture,
    #[error("operator decision command expired")]
    OperatorDecisionExpired,
    #[error("{name} must use canonical UTC form")]
    TimestampNotUtc { name: &'static str },
    #[error("{name} must use canonical UTC seconds form")]
    TimestampNotCanonical { name: &'static str },
    #[error("parsing {name}")]
    TimestampParse {
        name: &'static str,
        #[source]
        source: chrono::ParseError,
    },
    #[error("unsupported operator query command version {version}")]
    UnsupportedOperatorQueryVersion { version: u8 },
    #[error("operator query command {name} contains control characters")]
    OperatorQueryControlCharacters { name: &'static str },
    #[error("invalid operator query command signature length")]
    OperatorQuerySignatureLength,
    #[error("operator query command is issued in the future")]
    OperatorQueryFromFuture,
    #[error("operator query command expired")]
    OperatorQueryExpired,
    #[error("unsupported enrollment decision version {version}")]
    UnsupportedDecisionVersion { version: u8 },
    #[error("decision request_id mismatch")]
    DecisionRequestIdMismatch,
    #[error("decision request_digest mismatch")]
    DecisionRequestDigestMismatch,
    #[error("decision network_id mismatch")]
    DecisionNetworkIdMismatch,
    #[error("decision admin_did mismatch")]
    DecisionAdminDidMismatch,
    #[error("decision candidate_did mismatch")]
    DecisionCandidateDidMismatch,
    #[error("decision candidate_peer mismatch")]
    DecisionCandidatePeerMismatch,
    #[error("decision owner_agent mismatch")]
    DecisionOwnerAgentMismatch,
    #[error("decision signer mismatch")]
    DecisionSignerMismatch,
    #[error("decision_id mismatch")]
    DecisionIdMismatch,
    #[error("approved decision requires a positive authorization sequence")]
    ApprovedWithoutSequence,
    #[error("approval lease must expire after its decision")]
    ApprovalLeaseNotAfterDecision,
    #[error("approval lease exceeds the supported maximum")]
    ApprovalLeaseTooLong,
    #[error("denied decision cannot allocate an authorization sequence")]
    DeniedWithSequence,
    #[error("denied decision must use its decision time as its non-leased boundary")]
    DeniedLeaseNotDecision,
    #[error("decision authorization sequence exceeds the DefraDB Int range")]
    DecisionSequenceOverflow,
    #[error("invalid decision signature length")]
    DecisionSignatureLength,
    #[error("decision is outside the signed request window")]
    DecisionOutsideRequestWindow,
    #[error("unsupported authorization revision version {version}")]
    UnsupportedRevisionVersion { version: u8 },
    #[error("authorization revision requires an approved decision")]
    RevisionNotApproved,
    #[error("revision request_id mismatch")]
    RevisionRequestIdMismatch,
    #[error("revision request_digest mismatch")]
    RevisionRequestDigestMismatch,
    #[error("revision network_id mismatch")]
    RevisionNetworkIdMismatch,
    #[error("revision admin_did mismatch")]
    RevisionAdminDidMismatch,
    #[error("revision member_did mismatch")]
    RevisionMemberDidMismatch,
    #[error("revision member_peer mismatch")]
    RevisionMemberPeerMismatch,
    #[error("revision owner_agent mismatch")]
    RevisionOwnerAgentMismatch,
    #[error("revision signer mismatch")]
    RevisionSignerMismatch,
    #[error("authorization revision sequence must be positive")]
    RevisionSequenceNotPositive,
    #[error("authorization revision sequence exceeds the DefraDB Int range")]
    RevisionSequenceOverflow,
    #[error("revision authorization lease mismatch")]
    RevisionLeaseMismatch,
    #[error("revision_id mismatch")]
    RevisionIdMismatch,
    #[error("active revision sequence must equal its approval")]
    ActiveSequenceMismatch,
    #[error("active revision timestamp must equal its approval")]
    ActiveTimestampMismatch,
    #[error("revocation must dominate its approval")]
    RevocationNotDominant,
    #[error("revocation cannot predate its approval")]
    RevocationBeforeApproval,
    #[error("invalid revision signature length")]
    RevisionSignatureLength,
    #[error("unsupported enrollment route receipt version {version}")]
    UnsupportedRouteReceiptVersion { version: u8 },
    #[error("route receipt requires an approved decision")]
    RouteReceiptNotApproved,
    #[error("receipt request_id mismatch")]
    ReceiptRequestIdMismatch,
    #[error("receipt request_digest mismatch")]
    ReceiptRequestDigestMismatch,
    #[error("receipt network_id mismatch")]
    ReceiptNetworkIdMismatch,
    #[error("receipt admin_did mismatch")]
    ReceiptAdminDidMismatch,
    #[error("receipt member_did mismatch")]
    ReceiptMemberDidMismatch,
    #[error("receipt member_peer mismatch")]
    ReceiptMemberPeerMismatch,
    #[error("receipt server_peer mismatch")]
    ReceiptServerPeerMismatch,
    #[error("receipt owner_agent mismatch")]
    ReceiptOwnerAgentMismatch,
    #[error("receipt authorization sequence mismatch")]
    ReceiptSequenceMismatch,
    #[error("receipt authorization lease mismatch")]
    ReceiptLeaseMismatch,
    #[error("receipt signer mismatch")]
    ReceiptSignerMismatch,
    #[error("receipt_id mismatch")]
    ReceiptIdMismatch,
    #[error("route receipt cannot predate approval")]
    RouteReceiptBeforeApproval,
    #[error("route receipt cannot be issued after lease expiry")]
    RouteReceiptAfterLease,
    #[error("invalid route receipt signature length")]
    RouteReceiptSignatureLength,
    #[error("encoding enrollment offer")]
    EncodeOffer(#[source] ciborium::ser::Error<std::io::Error>),
    #[error("decoding enrollment offer")]
    DecodeOfferBase58(#[source] bs58::decode::Error),
    #[error("parsing enrollment offer")]
    ParseOffer(#[source] ciborium::de::Error<std::io::Error>),
    #[error("enrollment offer is empty")]
    OfferEmpty,
    #[error("enrollment offer has whitespace")]
    OfferWhitespace,
    #[error("enrollment offer exceeds the size limit")]
    OfferTooLarge,
    #[error("decoded enrollment offer exceeds the size limit")]
    DecodedOfferTooLarge,
    #[error("enrollment offer contains trailing data")]
    OfferTrailingData,
    #[error("unsupported enrollment offer version {version}")]
    UnsupportedOfferVersion { version: u8 },
    #[error("unsupported enrollment profile")]
    UnsupportedOfferProfile,
    #[error("invalid enrollment offer signature length")]
    OfferSignatureLength,
}

type Result<T, E = EnrollmentError> = std::result::Result<T, E>;

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
        if self.protocol_version != ENROLLMENT_PROTOCOL_VERSION {
            return Err(EnrollmentError::UnsupportedRequestVersion {
                version: self.protocol_version,
            });
        }
        if self.offer_id != offer.offer_id {
            return Err(EnrollmentError::RequestOfferIdMismatch);
        }
        if self.challenge != offer.challenge {
            return Err(EnrollmentError::RequestChallengeMismatch);
        }
        if self.network_id != offer.network_id {
            return Err(EnrollmentError::RequestNetworkIdMismatch);
        }
        if self.admin_did != offer.admin_did {
            return Err(EnrollmentError::RequestAdminDidMismatch);
        }
        if self.server_peer != offer.server_peer {
            return Err(EnrollmentError::RequestServerPeerMismatch);
        }
        if self.owner_agent != offer.owner_agent {
            return Err(EnrollmentError::RequestOwnerAgentMismatch);
        }
        if self.profile != offer.profile {
            return Err(EnrollmentError::RequestProfileMismatch);
        }
        if self.expires_at != offer.expires_at {
            return Err(EnrollmentError::RequestExtendsOffer);
        }
        if self.request_digest != self.computed_digest() {
            return Err(EnrollmentError::RequestDigestMismatch);
        }
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
        if self.request_id != expected_id {
            return Err(EnrollmentError::RequestIdMismatch);
        }
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
        if self.candidate_sig.len() != 64 {
            return Err(EnrollmentError::RequestSignatureLength);
        }
        let offer_issued = parse_canonical_timestamp("offer issued_at", &offer.issued_at)?;
        let request_issued = parse_canonical_timestamp("request issued_at", &self.issued_at)?;
        let offer_expires = parse_canonical_timestamp("offer expires_at", &offer.expires_at)?;
        if !(offer_issued <= request_issued && request_issued <= offer_expires) {
            return Err(EnrollmentError::RequestOutsideOfferWindow);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentDecisionKind {
    Approved,
    Denied,
}

/// Why the runtime refused an operator's enrollment decision, as the
/// decision endpoint reports it beside the error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentDecisionRefusalCode {
    /// The request is not (yet) a fresh pending request in this projection.
    NotPending,
    /// Two requests used one offer's challenge; both are refused.
    ChallengeReused,
    /// The request conflicts with other enrollment rows.
    Conflicted,
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
        if self.protocol_version != ENROLLMENT_PROTOCOL_VERSION {
            return Err(EnrollmentError::UnsupportedOperatorDecisionVersion {
                version: self.protocol_version,
            });
        }
        for (name, value) in [
            ("request_id", self.request_id.as_str()),
            ("admin_did", self.admin_did.as_str()),
            ("issued_at", self.issued_at.as_str()),
            ("nonce", self.nonce.as_str()),
        ] {
            validate_exact_field(name, value)?;
            if value.chars().any(char::is_control) {
                return Err(EnrollmentError::OperatorDecisionControlCharacters { name });
            }
        }
        if self.admin_sig.len() != 64 {
            return Err(EnrollmentError::OperatorDecisionSignatureLength);
        }
        match self.action {
            EnrollmentOperatorAction::Approve => {
                if !(1..=MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS).contains(&self.lease_seconds) {
                    return Err(EnrollmentError::ApprovalLeaseOutOfRange);
                }
            }
            EnrollmentOperatorAction::Deny | EnrollmentOperatorAction::Revoke => {
                if self.lease_seconds != 0 {
                    return Err(EnrollmentError::DenyOrRevokeWithLease);
                }
            }
        }
        let issued = parse_canonical_timestamp("operator command issued_at", &self.issued_at)?
            .with_timezone(&Utc);
        if issued > now + chrono::Duration::seconds(5) {
            return Err(EnrollmentError::OperatorDecisionFromFuture);
        }
        if now - issued > chrono::Duration::seconds(60) {
            return Err(EnrollmentError::OperatorDecisionExpired);
        }
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
        if self.protocol_version != ENROLLMENT_PROTOCOL_VERSION {
            return Err(EnrollmentError::UnsupportedOperatorQueryVersion {
                version: self.protocol_version,
            });
        }
        for (name, value) in [
            ("admin_did", self.admin_did.as_str()),
            ("issued_at", self.issued_at.as_str()),
            ("nonce", self.nonce.as_str()),
        ] {
            validate_exact_field(name, value)?;
            if value.chars().any(char::is_control) {
                return Err(EnrollmentError::OperatorQueryControlCharacters { name });
            }
        }
        if self.admin_sig.len() != 64 {
            return Err(EnrollmentError::OperatorQuerySignatureLength);
        }
        let issued = parse_canonical_timestamp("operator query issued_at", &self.issued_at)?
            .with_timezone(&Utc);
        if issued > now + chrono::Duration::seconds(5) {
            return Err(EnrollmentError::OperatorQueryFromFuture);
        }
        if now - issued > chrono::Duration::seconds(60) {
            return Err(EnrollmentError::OperatorQueryExpired);
        }
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
        if self.protocol_version != ENROLLMENT_PROTOCOL_VERSION {
            return Err(EnrollmentError::UnsupportedDecisionVersion {
                version: self.protocol_version,
            });
        }
        if self.request_id != request.request_id {
            return Err(EnrollmentError::DecisionRequestIdMismatch);
        }
        if self.request_digest != request.request_digest {
            return Err(EnrollmentError::DecisionRequestDigestMismatch);
        }
        if self.network_id != request.network_id {
            return Err(EnrollmentError::DecisionNetworkIdMismatch);
        }
        if self.admin_did != request.admin_did {
            return Err(EnrollmentError::DecisionAdminDidMismatch);
        }
        if self.candidate_did != request.candidate_did {
            return Err(EnrollmentError::DecisionCandidateDidMismatch);
        }
        if self.candidate_peer != request.candidate_peer {
            return Err(EnrollmentError::DecisionCandidatePeerMismatch);
        }
        if self.owner_agent != request.owner_agent {
            return Err(EnrollmentError::DecisionOwnerAgentMismatch);
        }
        if self.signer_did != request.admin_did {
            return Err(EnrollmentError::DecisionSignerMismatch);
        }
        if self.decision_id != derive_decision_id(&request.request_id, &request.request_digest) {
            return Err(EnrollmentError::DecisionIdMismatch);
        }
        match self.decision {
            EnrollmentDecisionKind::Approved => {
                if self.authorization_sequence == 0 {
                    return Err(EnrollmentError::ApprovedWithoutSequence);
                }
                let expires = parse_canonical_timestamp(
                    "decision authorization_expires_at",
                    &self.authorization_expires_at,
                )?;
                let decided = parse_canonical_timestamp("decision decided_at", &self.decided_at)?;
                if expires <= decided {
                    return Err(EnrollmentError::ApprovalLeaseNotAfterDecision);
                }
                if expires - decided
                    > chrono::Duration::seconds(MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS as i64)
                {
                    return Err(EnrollmentError::ApprovalLeaseTooLong);
                }
            }
            EnrollmentDecisionKind::Denied => {
                if self.authorization_sequence != 0 {
                    return Err(EnrollmentError::DeniedWithSequence);
                }
                if self.authorization_expires_at != self.decided_at {
                    return Err(EnrollmentError::DeniedLeaseNotDecision);
                }
                parse_canonical_timestamp(
                    "denied decision authorization_expires_at",
                    &self.authorization_expires_at,
                )?;
            }
        }
        if self.authorization_sequence > i64::MAX as u64 {
            return Err(EnrollmentError::DecisionSequenceOverflow);
        }
        if self.admin_sig.len() != 64 {
            return Err(EnrollmentError::DecisionSignatureLength);
        }
        let request_issued = parse_canonical_timestamp("request issued_at", &request.issued_at)?;
        let request_expires = parse_canonical_timestamp("request expires_at", &request.expires_at)?;
        let decided = parse_canonical_timestamp("decision decided_at", &self.decided_at)?;
        if !(request_issued <= decided && decided <= request_expires) {
            return Err(EnrollmentError::DecisionOutsideRequestWindow);
        }
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
        if self.protocol_version != ENROLLMENT_PROTOCOL_VERSION {
            return Err(EnrollmentError::UnsupportedRevisionVersion {
                version: self.protocol_version,
            });
        }
        if decision.decision != EnrollmentDecisionKind::Approved {
            return Err(EnrollmentError::RevisionNotApproved);
        }
        if self.request_id != request.request_id {
            return Err(EnrollmentError::RevisionRequestIdMismatch);
        }
        if self.request_digest != request.request_digest {
            return Err(EnrollmentError::RevisionRequestDigestMismatch);
        }
        if self.network_id != request.network_id {
            return Err(EnrollmentError::RevisionNetworkIdMismatch);
        }
        if self.admin_did != request.admin_did {
            return Err(EnrollmentError::RevisionAdminDidMismatch);
        }
        if self.member_did != request.candidate_did {
            return Err(EnrollmentError::RevisionMemberDidMismatch);
        }
        if self.member_peer != request.candidate_peer {
            return Err(EnrollmentError::RevisionMemberPeerMismatch);
        }
        if self.owner_agent != request.owner_agent {
            return Err(EnrollmentError::RevisionOwnerAgentMismatch);
        }
        if self.signer_did != request.admin_did {
            return Err(EnrollmentError::RevisionSignerMismatch);
        }
        if self.sequence == 0 {
            return Err(EnrollmentError::RevisionSequenceNotPositive);
        }
        if self.sequence > i64::MAX as u64 {
            return Err(EnrollmentError::RevisionSequenceOverflow);
        }
        if self.authorization_expires_at != decision.authorization_expires_at {
            return Err(EnrollmentError::RevisionLeaseMismatch);
        }
        if self.revision_id
            != derive_revision_id(
                &self.network_id,
                &self.member_did,
                self.sequence,
                &self.kind,
                &self.request_digest,
            )
        {
            return Err(EnrollmentError::RevisionIdMismatch);
        }
        match self.kind {
            AuthorizationRevisionKind::Active => {
                if self.sequence != decision.authorization_sequence {
                    return Err(EnrollmentError::ActiveSequenceMismatch);
                }
                if self.issued_at != decision.decided_at {
                    return Err(EnrollmentError::ActiveTimestampMismatch);
                }
            }
            AuthorizationRevisionKind::Revoked => {
                if self.sequence <= decision.authorization_sequence {
                    return Err(EnrollmentError::RevocationNotDominant);
                }
                let decision_time =
                    parse_canonical_timestamp("decision decided_at", &decision.decided_at)?;
                let revision_time =
                    parse_canonical_timestamp("revision issued_at", &self.issued_at)?;
                if revision_time < decision_time {
                    return Err(EnrollmentError::RevocationBeforeApproval);
                }
            }
        }
        if self.admin_sig.len() != 64 {
            return Err(EnrollmentError::RevisionSignatureLength);
        }
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
        if self.protocol_version != ENROLLMENT_PROTOCOL_VERSION {
            return Err(EnrollmentError::UnsupportedRouteReceiptVersion {
                version: self.protocol_version,
            });
        }
        decision.validate_against_request(request)?;
        if decision.decision != EnrollmentDecisionKind::Approved {
            return Err(EnrollmentError::RouteReceiptNotApproved);
        }
        if self.request_id != request.request_id {
            return Err(EnrollmentError::ReceiptRequestIdMismatch);
        }
        if self.request_digest != request.request_digest {
            return Err(EnrollmentError::ReceiptRequestDigestMismatch);
        }
        if self.network_id != request.network_id {
            return Err(EnrollmentError::ReceiptNetworkIdMismatch);
        }
        if self.admin_did != request.admin_did {
            return Err(EnrollmentError::ReceiptAdminDidMismatch);
        }
        if self.member_did != request.candidate_did {
            return Err(EnrollmentError::ReceiptMemberDidMismatch);
        }
        if self.member_peer != request.candidate_peer {
            return Err(EnrollmentError::ReceiptMemberPeerMismatch);
        }
        if self.server_peer != request.server_peer {
            return Err(EnrollmentError::ReceiptServerPeerMismatch);
        }
        if self.owner_agent != request.owner_agent {
            return Err(EnrollmentError::ReceiptOwnerAgentMismatch);
        }
        if self.authorization_sequence != decision.authorization_sequence {
            return Err(EnrollmentError::ReceiptSequenceMismatch);
        }
        if self.authorization_expires_at != decision.authorization_expires_at {
            return Err(EnrollmentError::ReceiptLeaseMismatch);
        }
        if self.signer_did != request.admin_did {
            return Err(EnrollmentError::ReceiptSignerMismatch);
        }
        if self.receipt_id
            != derive_route_receipt_id(
                &self.request_id,
                &self.request_digest,
                self.authorization_sequence,
                &self.direction,
            )
        {
            return Err(EnrollmentError::ReceiptIdMismatch);
        }
        let decided = parse_canonical_timestamp("decision decided_at", &decision.decided_at)?;
        let applied = parse_canonical_timestamp("receipt applied_at", &self.applied_at)?;
        if applied < decided {
            return Err(EnrollmentError::RouteReceiptBeforeApproval);
        }
        let expires = parse_canonical_timestamp(
            "receipt authorization_expires_at",
            &self.authorization_expires_at,
        )?;
        if applied >= expires {
            return Err(EnrollmentError::RouteReceiptAfterLease);
        }
        if self.admin_sig.len() != 64 {
            return Err(EnrollmentError::RouteReceiptSignatureLength);
        }
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
    ciborium::ser::into_writer(offer, &mut bytes).map_err(EnrollmentError::EncodeOffer)?;
    Ok(bs58::encode(bytes).into_string())
}

pub fn decode_offer(encoded: &str) -> Result<EnrollmentOfferRecord> {
    if encoded.is_empty() {
        return Err(EnrollmentError::OfferEmpty);
    }
    if encoded != encoded.trim() {
        return Err(EnrollmentError::OfferWhitespace);
    }
    if encoded.len() > MAX_OFFER_TOKEN_BYTES {
        return Err(EnrollmentError::OfferTooLarge);
    }
    let bytes = bs58::decode(encoded)
        .into_vec()
        .map_err(EnrollmentError::DecodeOfferBase58)?;
    if bytes.len() > MAX_OFFER_TOKEN_BYTES {
        return Err(EnrollmentError::DecodedOfferTooLarge);
    }
    let mut cursor = Cursor::new(bytes.as_slice());
    let offer: EnrollmentOfferRecord =
        ciborium::de::from_reader(&mut cursor).map_err(EnrollmentError::ParseOffer)?;
    if cursor.position() != bytes.len() as u64 {
        return Err(EnrollmentError::OfferTrailingData);
    }
    if offer.version != ENROLLMENT_PROTOCOL_VERSION {
        return Err(EnrollmentError::UnsupportedOfferVersion {
            version: offer.version,
        });
    }
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
    if offer.profile != "client" {
        return Err(EnrollmentError::UnsupportedOfferProfile);
    }
    if offer.admin_sig.len() != 64 {
        return Err(EnrollmentError::OfferSignatureLength);
    }
    Ok(offer)
}

fn validate_exact_field(name: &'static str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(EnrollmentError::OfferFieldEmpty { name });
    }
    if value != value.trim() {
        return Err(EnrollmentError::OfferFieldWhitespace { name });
    }
    if value.len() > MAX_ENROLLMENT_FIELD_BYTES {
        return Err(EnrollmentError::OfferFieldTooLarge { name });
    }
    Ok(())
}

fn parse_canonical_timestamp(name: &'static str, value: &str) -> Result<DateTime<FixedOffset>> {
    if !value.ends_with('Z') {
        return Err(EnrollmentError::TimestampNotUtc { name });
    }
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|source| EnrollmentError::TimestampParse { name, source })?;
    if parsed.to_rfc3339_opts(SecondsFormat::Secs, true) != value {
        return Err(EnrollmentError::TimestampNotCanonical { name });
    }
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
