use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::enrollment::canonical_domain_payload;
use crate::pairing_reader::{PairingObservation, PairingReadReason};

/// The runtime route that decides a client's owner-staged relationship change
/// (Lean `Identity.OwnerStaged.overHttp`).
pub const OWNER_STAGED_PATH: &str = "/config/owner-staged";
pub const OWNER_STAGED_COMMAND_VERSION: u8 = 1;
const OWNER_STAGED_SIGNATURE_DOMAIN: &str = "gents-owner-staged-command-v1";

/// The owner-managed relations a client may ask the runtime to stage
/// (Lean `Identity.OwnerStaged`, one per modeled path). Each names a fixed
/// collection and relation, so the route cannot be asked to change any other
/// relation of a runtime-owned document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnerStagedChange {
    /// `writer` of an EventSourceCursor rebound to a selected Agent.
    EventCursorWriter {
        cursor_doc_id: String,
        agent_did: String,
    },
    /// `reader` of a RepositoryPlacement for a declared reader.
    PlacementReader {
        placement_doc_id: String,
        reader_did: String,
    },
    /// `workspace_selected_by` of an Agent, held by a Callback's readers.
    WorkspaceSelection {
        agent_doc_id: String,
        callback_doc_id: String,
    },
    /// A pairing observation's reader for one reason.
    PairingReader {
        observation: PairingObservation,
        doc_id: String,
        reason: PairingReadReason,
        reader_did: String,
    },
}

impl OwnerStagedChange {
    fn kind(&self) -> &'static str {
        match self {
            Self::EventCursorWriter { .. } => "event_cursor_writer",
            Self::PlacementReader { .. } => "placement_reader",
            Self::WorkspaceSelection { .. } => "workspace_selection",
            Self::PairingReader { .. } => "pairing_reader",
        }
    }

    /// Every field the runtime decides on, in signing order.
    fn fields(&self) -> Vec<(&'static str, &str)> {
        match self {
            Self::EventCursorWriter {
                cursor_doc_id,
                agent_did,
            } => vec![("cursor_doc_id", cursor_doc_id), ("agent_did", agent_did)],
            Self::PlacementReader {
                placement_doc_id,
                reader_did,
            } => vec![
                ("placement_doc_id", placement_doc_id),
                ("reader_did", reader_did),
            ],
            Self::WorkspaceSelection {
                agent_doc_id,
                callback_doc_id,
            } => vec![
                ("agent_doc_id", agent_doc_id),
                ("callback_doc_id", callback_doc_id),
            ],
            Self::PairingReader {
                observation,
                doc_id,
                reason,
                reader_did,
            } => vec![
                ("observation", observation.collection()),
                ("doc_id", doc_id),
                ("reason", reason.as_str()),
                ("reader_did", reader_did),
            ],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OwnerStagedCommandError {
    #[error("unsupported owner-staged command version")]
    UnsupportedVersion,
    #[error("owner-staged command field {name} is empty, padded or has control characters")]
    InvalidField { name: &'static str },
    #[error("invalid owner-staged command signature length")]
    SignatureLength,
    #[error(transparent)]
    ParseTimestamp(#[from] chrono::ParseError),
    #[error("owner-staged command is issued in the future")]
    FutureTimestamp,
    #[error("owner-staged command expired")]
    ExpiredTimestamp,
}

/// A client's signed request that the runtime grant or revoke one
/// owner-staged relation. The runtime acts on it only for the signer it
/// authenticated and decides it exactly as that signer's embedded request
/// would be decided. `audience` is the endpoint authority the client
/// addressed (`host[:port]`, as its bearer's audience), so a command is
/// refused by any other endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerStagedCommand {
    pub version: u8,
    pub audience: String,
    pub change: OwnerStagedChange,
    pub grant: bool,
    pub signer_did: String,
    pub issued_at: String,
    pub nonce: String,
    pub sig: Vec<u8>,
}

impl OwnerStagedCommand {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.version.to_string();
        let mut fields = vec![version.as_str(), &self.audience, self.change.kind()];
        fields.extend(self.change.fields().into_iter().map(|(_, value)| value));
        fields.extend([
            if self.grant { "grant" } else { "revoke" },
            &self.signer_did,
            &self.issued_at,
            &self.nonce,
        ]);
        canonical_domain_payload(OWNER_STAGED_SIGNATURE_DOMAIN, fields)
    }

    pub fn validate_at(&self, now: DateTime<Utc>) -> Result<(), OwnerStagedCommandError> {
        if self.version != OWNER_STAGED_COMMAND_VERSION {
            return Err(OwnerStagedCommandError::UnsupportedVersion);
        }
        let mut fields = vec![
            ("audience", self.audience.as_str()),
            ("signer_did", self.signer_did.as_str()),
            ("issued_at", self.issued_at.as_str()),
            ("nonce", self.nonce.as_str()),
        ];
        fields.extend(self.change.fields());
        for (name, value) in fields {
            if value.is_empty() || value != value.trim() || value.chars().any(char::is_control) {
                return Err(OwnerStagedCommandError::InvalidField { name });
            }
        }
        if self.sig.len() != 64 {
            return Err(OwnerStagedCommandError::SignatureLength);
        }
        let issued = DateTime::parse_from_rfc3339(&self.issued_at)?.with_timezone(&Utc);
        if issued > now + Duration::seconds(5) {
            return Err(OwnerStagedCommandError::FutureTimestamp);
        }
        if now - issued > Duration::seconds(60) {
            return Err(OwnerStagedCommandError::ExpiredTimestamp);
        }
        Ok(())
    }
}

/// Why the runtime refused an authenticated command
/// (Lean `Identity.OwnerStaged.Outcome`). A denied response is the same for
/// every document, so it reveals neither existence nor owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OwnerStagedRefusal {
    Denied,
    Unmanaged,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_staged_signatures_bind_every_decided_field() {
        let now = Utc::now();
        let command = |change| OwnerStagedCommand {
            version: OWNER_STAGED_COMMAND_VERSION,
            audience: "127.0.0.1:9181".into(),
            change,
            grant: true,
            signer_did: "did:key:signer".into(),
            issued_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            nonce: "nonce".into(),
            sig: vec![0; 64],
        };
        let pairing = command(OwnerStagedChange::PairingReader {
            observation: PairingObservation::Desired,
            doc_id: "bae-doc".into(),
            reason: PairingReadReason::RouteAdmission,
            reader_did: "did:key:reader".into(),
        });
        pairing.validate_at(now).unwrap();
        let payload = pairing.signing_payload();
        let changes: [fn(&mut OwnerStagedCommand); 8] = [
            |c| c.audience.push('x'),
            |c| {
                if let OwnerStagedChange::PairingReader { observation, .. } = &mut c.change {
                    *observation = PairingObservation::Applied;
                }
            },
            |c| {
                if let OwnerStagedChange::PairingReader { doc_id, .. } = &mut c.change {
                    doc_id.push('x');
                }
            },
            |c| {
                if let OwnerStagedChange::PairingReader { reason, .. } = &mut c.change {
                    *reason = PairingReadReason::ToolView;
                }
            },
            |c| {
                if let OwnerStagedChange::PairingReader { reader_did, .. } = &mut c.change {
                    reader_did.push('x');
                }
            },
            |c| c.grant = false,
            |c| c.signer_did.push('x'),
            |c| c.nonce.push('x'),
        ];
        for change in changes {
            let mut changed = pairing.clone();
            change(&mut changed);
            assert_ne!(changed.signing_payload(), payload);
        }
        // The same identifiers under another path sign differently.
        let placement = command(OwnerStagedChange::PlacementReader {
            placement_doc_id: "bae-doc".into(),
            reader_did: "did:key:reader".into(),
        });
        let cursor = command(OwnerStagedChange::EventCursorWriter {
            cursor_doc_id: "bae-doc".into(),
            agent_did: "did:key:reader".into(),
        });
        assert_ne!(placement.signing_payload(), cursor.signing_payload());
        let empty = command(OwnerStagedChange::WorkspaceSelection {
            agent_doc_id: "bae-agent".into(),
            callback_doc_id: String::new(),
        });
        assert!(matches!(
            empty.validate_at(now),
            Err(OwnerStagedCommandError::InvalidField {
                name: "callback_doc_id"
            })
        ));
        assert!(pairing.validate_at(now + Duration::seconds(61)).is_err());
        assert!(pairing.validate_at(now - Duration::seconds(6)).is_err());
    }
}
