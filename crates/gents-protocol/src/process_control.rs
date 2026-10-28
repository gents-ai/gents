use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::enrollment::canonical_domain_payload;

pub const PROCESS_STOP_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessStop {
    pub version: u8,
    pub command_id: String,
    pub requester_did: String,
    pub requester_peer_id: Option<String>,
    pub node_did: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_call_doc_id: String,
    pub issued_at: String,
    pub expires_at: String,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStopOutcome {
    /// Authorization was committed; the runtime must still attempt the stop.
    Requested,
    Stopped,
    AlreadyTerminal,
    Lost,
    Unverified,
    Rejected,
}

impl ProcessStopOutcome {
    pub fn needs_execution(self) -> bool {
        self == Self::Requested
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessStopReceipt {
    pub version: u8,
    pub command_id: String,
    pub command_digest: String,
    pub node_did: String,
    pub outcome: ProcessStopOutcome,
    pub reason: String,
    pub processed_at: String,
    pub signature: Vec<u8>,
}

impl ProcessStopReceipt {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.version.to_string();
        let outcome = match self.outcome {
            ProcessStopOutcome::Requested => "requested",
            ProcessStopOutcome::Stopped => "stopped",
            ProcessStopOutcome::AlreadyTerminal => "already_terminal",
            ProcessStopOutcome::Lost => "lost",
            ProcessStopOutcome::Unverified => "unverified",
            ProcessStopOutcome::Rejected => "rejected",
        };
        canonical_domain_payload(
            "gents-process-stop-receipt-v1",
            [
                version.as_str(),
                &self.command_id,
                &self.command_digest,
                &self.node_did,
                outcome,
                &self.reason,
                &self.processed_at,
            ],
        )
    }

    /// Callers must also verify the receipt signature against the target DID.
    pub fn validate_for(&self, command: &ProcessStop) -> Result<()> {
        ensure!(
            self.version == PROCESS_STOP_VERSION,
            "unsupported process stop receipt version"
        );
        ensure!(
            self.command_id == command.command_id
                && self.command_digest == command.digest()
                && self.node_did == command.node_did,
            "process stop receipt does not match the command"
        );
        ensure!(
            self.signature.len() == 64,
            "invalid process stop receipt signature length"
        );
        chrono::DateTime::parse_from_rfc3339(&self.processed_at)?;
        Ok(())
    }
}

impl ProcessStop {
    pub fn signing_payload(&self) -> Vec<u8> {
        let version = self.version.to_string();
        let (peer_kind, peer) = self
            .requester_peer_id
            .as_deref()
            .map_or(("none", ""), |value| ("some", value));
        canonical_domain_payload(
            "gents-process-stop-v1",
            [
                version.as_str(),
                &self.command_id,
                &self.requester_did,
                peer_kind,
                peer,
                &self.node_did,
                &self.session_id,
                &self.tool_call_id,
                &self.tool_call_doc_id,
                &self.issued_at,
                &self.expires_at,
            ],
        )
    }

    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.signing_payload());
        hash.update(&self.signature);
        format!("sha256:{:x}", hash.finalize())
    }

    pub fn validate_shape(&self) -> Result<()> {
        ensure!(
            self.version == PROCESS_STOP_VERSION,
            "unsupported process stop version"
        );
        for value in [
            &self.command_id,
            &self.requester_did,
            &self.node_did,
            &self.session_id,
            &self.tool_call_id,
            &self.tool_call_doc_id,
        ] {
            ensure!(!value.trim().is_empty(), "empty process stop identity");
        }
        ensure!(
            self.requester_peer_id
                .as_ref()
                .is_none_or(|peer| !peer.trim().is_empty()),
            "empty requester peer identity"
        );
        let issued = chrono::DateTime::parse_from_rfc3339(&self.issued_at)?;
        let expires = chrono::DateTime::parse_from_rfc3339(&self.expires_at)?;
        ensure!(expires > issued, "process stop expiry must follow issuance");
        ensure!(
            self.signature.len() == 64,
            "invalid process stop signature length"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_stop_binds_physical_identity_and_expiry() {
        let command = ProcessStop {
            version: PROCESS_STOP_VERSION,
            command_id: "command".into(),
            requester_did: "requester".into(),
            requester_peer_id: None,
            node_did: "node".into(),
            session_id: "session".into(),
            tool_call_id: "call".into(),
            tool_call_doc_id: "document".into(),
            issued_at: "2026-10-01T00:00:00Z".into(),
            expires_at: "2026-10-01T00:01:00Z".into(),
            signature: vec![1; 64],
        };
        command.validate_shape().unwrap();
        let mut changed = command.clone();
        changed.tool_call_doc_id = "another-document".into();
        assert_ne!(command.signing_payload(), changed.signing_payload());
        assert_ne!(command.digest(), changed.digest());
        let receipt = ProcessStopReceipt {
            version: PROCESS_STOP_VERSION,
            command_id: command.command_id.clone(),
            command_digest: command.digest(),
            node_did: command.node_did.clone(),
            outcome: ProcessStopOutcome::Requested,
            reason: String::new(),
            processed_at: "2026-10-01T00:00:30Z".into(),
            signature: vec![2; 64],
        };
        receipt.validate_for(&command).unwrap();
        assert!(receipt.outcome.needs_execution());
        assert!(!ProcessStopOutcome::Stopped.needs_execution());
        assert!(receipt.validate_for(&changed).is_err());
        changed.expires_at = changed.issued_at.clone();
        assert!(changed.validate_shape().is_err());
    }
}
