use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::enrollment::canonical_domain_payload;

/// The runtime route that decides a client's pairing reader change
/// (Lean `Identity.OwnerStaged.pairingOverHttp`).
pub const PAIRING_READER_PATH: &str = "/p2p/pairings/reader";
pub const PAIRING_READER_COMMAND_VERSION: u8 = 1;
const PAIRING_READER_SIGNATURE_DOMAIN: &str = "gents-pairing-reader-command-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingObservation {
    Desired,
    Applied,
}

impl PairingObservation {
    pub fn collection(self) -> &'static str {
        match self {
            Self::Desired => "PeerPairingDesired",
            Self::Applied => "PeerPairingApplied",
        }
    }
}

/// An explicit native grant for one physical pairing observation. Enabling a
/// tool or choosing an Agent does not itself publish or revoke this grant.
/// Either reason grants native read access to the complete document; the
/// relation records independent revocation reasons, not field-level access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PairingReadReason {
    RouteAdmission,
    ToolView,
}

impl PairingReadReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RouteAdmission => "route-admission",
            Self::ToolView => "tool-view",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PairingReaderCommandError {
    #[error("unsupported pairing reader command version")]
    UnsupportedVersion,
    #[error("pairing reader command field {name} is empty, padded or has control characters")]
    InvalidField { name: &'static str },
    #[error("invalid pairing reader command signature length")]
    SignatureLength,
    #[error(transparent)]
    ParseTimestamp(#[from] chrono::ParseError),
    #[error("pairing reader command is issued in the future")]
    FutureTimestamp,
    #[error("pairing reader command expired")]
    ExpiredTimestamp,
}

/// A client's signed request that the runtime grant or revoke one pairing
/// reader. The runtime acts on it only for the signer it authenticated, and
/// decides it exactly as that signer's embedded request would be decided.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingReaderCommand {
    pub version: u8,
    pub node_did: String,
    pub observation: PairingObservation,
    pub doc_id: String,
    pub reason: PairingReadReason,
    pub reader_did: String,
    pub grant: bool,
    pub signer_did: String,
    pub issued_at: String,
    pub nonce: String,
    pub sig: Vec<u8>,
}

impl PairingReaderCommand {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.version.to_string();
        canonical_domain_payload(
            PAIRING_READER_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.node_did,
                self.observation.collection(),
                &self.doc_id,
                self.reason.as_str(),
                &self.reader_did,
                if self.grant { "grant" } else { "revoke" },
                &self.signer_did,
                &self.issued_at,
                &self.nonce,
            ],
        )
    }

    pub fn validate_at(&self, now: DateTime<Utc>) -> Result<(), PairingReaderCommandError> {
        if self.version != PAIRING_READER_COMMAND_VERSION {
            return Err(PairingReaderCommandError::UnsupportedVersion);
        }
        for (name, value) in [
            ("node_did", self.node_did.as_str()),
            ("doc_id", self.doc_id.as_str()),
            ("reader_did", self.reader_did.as_str()),
            ("signer_did", self.signer_did.as_str()),
            ("issued_at", self.issued_at.as_str()),
            ("nonce", self.nonce.as_str()),
        ] {
            if value.is_empty() || value != value.trim() || value.chars().any(char::is_control) {
                return Err(PairingReaderCommandError::InvalidField { name });
            }
        }
        if self.sig.len() != 64 {
            return Err(PairingReaderCommandError::SignatureLength);
        }
        let issued = DateTime::parse_from_rfc3339(&self.issued_at)?.with_timezone(&Utc);
        if issued > now + Duration::seconds(5) {
            return Err(PairingReaderCommandError::FutureTimestamp);
        }
        if now - issued > Duration::seconds(60) {
            return Err(PairingReaderCommandError::ExpiredTimestamp);
        }
        Ok(())
    }
}

/// Why the runtime refused an authenticated command
/// (Lean `Identity.OwnerStaged.Outcome`). A denied response is the same for
/// every observation, so it reveals neither existence nor owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PairingReaderRefusal {
    Denied,
    Unmanaged,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_reader_signatures_bind_every_decided_field() {
        let now = Utc::now();
        let command = PairingReaderCommand {
            version: PAIRING_READER_COMMAND_VERSION,
            node_did: "did:key:node".into(),
            observation: PairingObservation::Desired,
            doc_id: "bae-doc".into(),
            reason: PairingReadReason::RouteAdmission,
            reader_did: "did:key:reader".into(),
            grant: true,
            signer_did: "did:key:signer".into(),
            issued_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            nonce: "nonce".into(),
            sig: vec![0; 64],
        };
        command.validate_at(now).unwrap();
        let payload = command.signing_payload();
        let changes: [fn(&mut PairingReaderCommand); 7] = [
            |c| c.node_did.push('x'),
            |c| c.observation = PairingObservation::Applied,
            |c| c.doc_id.push('x'),
            |c| c.reason = PairingReadReason::ToolView,
            |c| c.reader_did.push('x'),
            |c| c.grant = false,
            |c| c.signer_did.push('x'),
        ];
        for change in changes {
            let mut changed = command.clone();
            change(&mut changed);
            assert_ne!(changed.signing_payload(), payload);
        }
        assert!(command.validate_at(now + Duration::seconds(61)).is_err());
        assert!(command.validate_at(now - Duration::seconds(6)).is_err());
    }
}
