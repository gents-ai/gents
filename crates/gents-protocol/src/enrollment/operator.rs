use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::canonical::{canonical_domain_payload, parse_canonical_timestamp, validate_exact_field};
use super::error::{EnrollmentError, Result};
use super::{
    ENROLLMENT_PROTOCOL_VERSION, MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
    OPERATOR_DECISION_COMMAND_SIGNATURE_DOMAIN, OPERATOR_QUERY_COMMAND_SIGNATURE_DOMAIN,
};

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
