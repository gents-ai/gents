//! Signed terminal receipts for exact session hydration.

use serde::{Deserialize, Serialize};

use crate::enrollment::canonical_domain_payload;

#[derive(Debug, thiserror::Error)]
pub enum SessionHydrationError {
    #[error("serialize session hydration manifest")]
    SerializeManifest(#[source] serde_json::Error),
    #[error("decode session hydration manifest")]
    DecodeManifest(#[source] serde_json::Error),
    #[error("unsupported session hydration receipt version")]
    UnsupportedVersion,
    #[error("session hydration receipt status is not terminal")]
    NotTerminal,
    #[error("session hydration receipt signer does not own the target agent")]
    SignerNotNodeOwner,
    #[error("invalid session hydration receipt signature length")]
    InvalidSignatureLength,
    #[error("session hydration manifest must be sorted and unique")]
    ManifestNotSorted,
    #[error("session hydration manifest contains an empty document identity")]
    EmptyDocumentIdentity,
}

type Result<T, E = SessionHydrationError> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct HydrationRequest {
    pub request_key: String,
    pub peer_id: String,
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
}

impl HydrationRequest {
    pub fn from_row(
        request_key: String,
        requester_did: String,
        node_did: String,
        session_id: String,
    ) -> Result<Self, &'static str> {
        let Some((peer_id, key_session_id)) = request_key.split_once(':') else {
            return Err("request_key must be {peer_id}:{session_id}");
        };
        if peer_id.is_empty() || key_session_id != session_id {
            return Err("request_key does not match peer/session columns");
        }
        let peer_id = peer_id.to_string();
        Ok(Self {
            request_key,
            peer_id,
            requester_did,
            node_did,
            session_id,
        })
    }
}

pub const SESSION_HYDRATION_RECEIPT_VERSION: u8 = 1;
const RECEIPT_SIGNATURE_DOMAIN: &str = "gents-session-hydration-receipt-v1";

/// Closed collection vocabulary: retired response/spill documents cannot be
/// claimed as transcript hydration. Origin dependencies retain their ACP;
/// following a reference never grants access to the whole origin session.
///
/// The manifest is the authorized reference closure of the served headers.
/// Terminal selections must resolve their exact headers; no
/// latest-visible-message fallback. Mutable request/tool lifecycle facts
/// still require their existing owner checks; the manifest does not bind
/// those documents' revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SessionHydrationCollection {
    AgentRequest,
    AgentMessage,
    AgentToolCall,
    AgentOutputSegment,
    CompactionEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHydrationDocumentKey {
    pub collection: SessionHydrationCollection,
    pub doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHydrationReceipt {
    pub version: u8,
    pub request_key: String,
    pub requester_did: String,
    pub node_did: String,
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
                &self.node_did,
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
        if self.version != SESSION_HYDRATION_RECEIPT_VERSION {
            return Err(SessionHydrationError::UnsupportedVersion);
        }
        if !matches!(self.status.as_str(), "served" | "rejected") {
            return Err(SessionHydrationError::NotTerminal);
        }
        if self.signer_did != self.node_did {
            return Err(SessionHydrationError::SignerNotNodeOwner);
        }
        if self.signature.len() != 64 {
            return Err(SessionHydrationError::InvalidSignatureLength);
        }
        if !self
            .served_manifest
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        {
            return Err(SessionHydrationError::ManifestNotSorted);
        }
        if self
            .served_manifest
            .iter()
            .any(|entry| entry.doc_id.is_empty())
        {
            return Err(SessionHydrationError::EmptyDocumentIdentity);
        }
        Ok(())
    }
}

pub fn canonical_manifest_json(entries: &[SessionHydrationDocumentKey]) -> Result<String> {
    serde_json::to_string(entries).map_err(SessionHydrationError::SerializeManifest)
}

pub fn decode_manifest_json(raw: &str) -> Result<Vec<SessionHydrationDocumentKey>> {
    let entries: Vec<SessionHydrationDocumentKey> =
        serde_json::from_str(raw).map_err(SessionHydrationError::DecodeManifest)?;
    if !entries.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(SessionHydrationError::ManifestNotSorted);
    }
    Ok(entries)
}
