//! Immutable signed pending-input commands and runtime-owned terminal receipts.
use crate::{enrollment::canonical_domain_payload, request_admission::AgentRequestCreate};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SESSION_INPUT_EDIT_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingMessageEdit {
    pub request_doc_id: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingQueueEdit {
    pub expected_request_doc_ids: Vec<String>,
    pub selected_request_doc_ids: Vec<String>,
    pub messages: Vec<PendingMessageEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingQueueReceipt {
    pub request_doc_ids: Vec<String>,
    pub request_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInputEdit {
    pub version: u8,
    pub command_id: String,
    pub requester_did: String,
    pub requester_peer_id: Option<String>,
    pub node_did: String,
    pub session_id: String,
    pub issued_at: String,
    pub expires_at: String,
    pub edit: PendingQueueEdit,
    pub replacements: Vec<AgentRequestCreate>,
    pub signature: Vec<u8>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl SessionInputEdit {
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut fields = vec![
            self.version.to_string(),
            self.command_id.clone(),
            self.requester_did.clone(),
            self.node_did.clone(),
            self.session_id.clone(),
            self.issued_at.clone(),
            self.expires_at.clone(),
        ];
        match &self.requester_peer_id {
            Some(peer) => {
                fields.push("some".into());
                fields.push(peer.clone());
            }
            None => fields.push("none".into()),
        }
        for ids in [
            &self.edit.expected_request_doc_ids,
            &self.edit.selected_request_doc_ids,
        ] {
            fields.push(ids.len().to_string());
            fields.extend(ids.iter().cloned());
        }
        fields.push(self.edit.messages.len().to_string());
        for message in &self.edit.messages {
            fields.push(message.request_doc_id.clone());
            fields.push(message.content.clone());
        }
        fields.push(self.replacements.len().to_string());
        for replacement in &self.replacements {
            fields.push(hex(&replacement.signing_payload()));
            fields.push(hex(&replacement.admission.signature));
        }
        canonical_domain_payload(
            "gents-session-input-edit-v1",
            fields.iter().map(String::as_str),
        )
    }

    /// Includes the author's signature so a receipt identifies one exact signed intent.
    pub fn computed_digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(canonical_domain_payload(
            "gents-session-input-edit-digest-v1",
            [hex(&self.signing_payload()), hex(&self.signature)]
                .iter()
                .map(String::as_str),
        ));
        format!("sha256:{}", hex(&hash.finalize()))
    }

    pub fn validate_shape(&self) -> Result<()> {
        ensure!(
            self.version == SESSION_INPUT_EDIT_VERSION,
            "unsupported session input edit version"
        );
        for value in [
            &self.command_id,
            &self.requester_did,
            &self.node_did,
            &self.session_id,
        ] {
            ensure!(
                !value.trim().is_empty(),
                "empty session input edit identity"
            );
        }
        ensure!(
            self.requester_peer_id
                .as_ref()
                .is_none_or(|peer| !peer.trim().is_empty()),
            "empty requester peer identity"
        );
        let issued = chrono::DateTime::parse_from_rfc3339(&self.issued_at)?;
        let expires = chrono::DateTime::parse_from_rfc3339(&self.expires_at)?;
        ensure!(
            expires > issued,
            "session input edit expiry must follow issuance"
        );
        ensure!(
            self.signature.len() == 64,
            "invalid session input edit signature length"
        );
        ensure!(
            !self.edit.selected_request_doc_ids.is_empty(),
            "session input edit selects no requests"
        );
        ensure!(
            self.replacements.is_empty()
                || self.replacements.len() == self.edit.selected_request_doc_ids.len(),
            "replacement count differs from selected group"
        );
        ensure!(
            self.edit.messages.len() == self.replacements.len(),
            "message and presigned replacement counts differ"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionInputEditOutcome {
    Applied,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInputEditReceipt {
    pub version: u8,
    pub command_id: String,
    pub command_digest: String,
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
    pub outcome: SessionInputEditOutcome,
    pub reason: String,
    pub request_doc_ids: Vec<String>,
    pub request_ids: Vec<String>,
    pub processed_at: String,
    pub signer_did: String,
    pub signature: Vec<u8>,
}

impl SessionInputEditReceipt {
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut fields = vec![
            self.version.to_string(),
            self.command_id.clone(),
            self.command_digest.clone(),
            self.requester_did.clone(),
            self.node_did.clone(),
            self.session_id.clone(),
            match self.outcome {
                SessionInputEditOutcome::Applied => "applied",
                SessionInputEditOutcome::Rejected => "rejected",
            }
            .into(),
            self.reason.clone(),
            self.processed_at.clone(),
            self.signer_did.clone(),
        ];
        for ids in [&self.request_doc_ids, &self.request_ids] {
            fields.push(ids.len().to_string());
            fields.extend(ids.iter().cloned());
        }
        canonical_domain_payload(
            "gents-session-input-edit-receipt-v1",
            fields.iter().map(String::as_str),
        )
    }
    pub fn validate_shape(&self) -> Result<()> {
        ensure!(
            self.version == SESSION_INPUT_EDIT_VERSION,
            "unsupported session input edit receipt version"
        );
        ensure!(
            self.signer_did == self.node_did,
            "session input edit receipt signer does not own target node"
        );
        ensure!(
            self.signature.len() == 64,
            "invalid session input edit receipt signature length"
        );
        ensure!(
            self.request_doc_ids.len() == self.request_ids.len(),
            "replacement receipt identity counts differ"
        );
        ensure!(
            self.outcome != SessionInputEditOutcome::Rejected || self.request_ids.is_empty(),
            "rejected edit cannot contain replacements"
        );
        chrono::DateTime::parse_from_rfc3339(&self.processed_at)?;
        Ok(())
    }

    /// Verification still checks the target node's signature using its existing DID owner.
    pub fn validate_for(&self, command: &SessionInputEdit) -> Result<()> {
        self.validate_shape()?;
        ensure!(
            self.command_id == command.command_id
                && self.command_digest == command.computed_digest()
                && self.requester_did == command.requester_did
                && self.node_did == command.node_did
                && self.session_id == command.session_id,
            "session input edit receipt scope differs from intent"
        );
        if self.outcome == SessionInputEditOutcome::Applied {
            ensure!(
                self.request_ids
                    == command
                        .replacements
                        .iter()
                        .map(|r| r.request_id.clone())
                        .collect::<Vec<_>>(),
                "session input edit receipt replacements differ from intent"
            );
        }
        Ok(())
    }
}
