//! Signed terminal receipts for exact session hydration.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::enrollment::canonical_domain_payload;

pub const SESSION_HYDRATION_RECEIPT_VERSION: u8 = 1;
const RECEIPT_SIGNATURE_DOMAIN: &str = "gents-session-hydration-receipt-v1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHydrationDocumentKey {
    pub collection: String,
    pub doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHydrationReceipt {
    pub version: u8,
    pub request_key: String,
    pub requester_did: String,
    pub agent_did: String,
    pub session_id: String,
    pub status: String,
    pub status_detail: String,
    pub served_manifest: Vec<SessionHydrationDocumentKey>,
    pub processed_at: String,
    pub signer_did: String,
    pub signature: Vec<u8>,
}

impl SessionHydrationReceipt {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let version = self.version.to_string();
        let manifest = canonical_manifest_json(&self.served_manifest)?;
        Ok(canonical_domain_payload(
            RECEIPT_SIGNATURE_DOMAIN,
            [
                version.as_str(),
                &self.request_key,
                &self.requester_did,
                &self.agent_did,
                &self.session_id,
                &self.status,
                &self.status_detail,
                &manifest,
                &self.processed_at,
                &self.signer_did,
            ],
        ))
    }

    pub fn validate_shape(&self) -> Result<()> {
        anyhow::ensure!(
            self.version == SESSION_HYDRATION_RECEIPT_VERSION,
            "unsupported session hydration receipt version"
        );
        anyhow::ensure!(
            matches!(self.status.as_str(), "served" | "rejected"),
            "session hydration receipt status is not terminal"
        );
        anyhow::ensure!(
            self.signer_did == self.agent_did,
            "session hydration receipt signer does not own the target agent"
        );
        anyhow::ensure!(
            self.signature.len() == 64,
            "invalid session hydration receipt signature length"
        );
        anyhow::ensure!(
            self.served_manifest
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "session hydration manifest must be sorted and unique"
        );
        for entry in &self.served_manifest {
            anyhow::ensure!(
                !entry.collection.is_empty() && !entry.doc_id.is_empty(),
                "session hydration manifest contains an empty document identity"
            );
        }
        Ok(())
    }
}

pub fn canonical_manifest_json(entries: &[SessionHydrationDocumentKey]) -> Result<String> {
    serde_json::to_string(entries).context("serialize session hydration manifest")
}

pub fn decode_manifest_json(raw: &str) -> Result<Vec<SessionHydrationDocumentKey>> {
    let entries: Vec<SessionHydrationDocumentKey> =
        serde_json::from_str(raw).context("decode session hydration manifest")?;
    anyhow::ensure!(
        entries.windows(2).all(|pair| pair[0] < pair[1]),
        "session hydration manifest must be sorted and unique"
    );
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_or_reordered_manifest_is_rejected() {
        let entry = SessionHydrationDocumentKey {
            collection: "AgentMessage".into(),
            doc_id: "doc-1".into(),
        };
        let raw = serde_json::to_string(&vec![entry.clone(), entry]).unwrap();
        assert!(decode_manifest_json(&raw).is_err());
    }
}
