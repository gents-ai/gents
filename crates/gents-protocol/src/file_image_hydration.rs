use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::enrollment::canonical_domain_payload;
use crate::session_hydration::HydrationRequest;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageScope {
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
}

impl FileImageScope {
    pub fn matches(&self, request: &HydrationRequest) -> bool {
        self.requester_did == request.requester_did
            && self.node_did == request.node_did
            && self.session_id == request.session_id
    }
}

pub const FILE_IMAGE_RECEIPT_VERSION: u8 = 2;
const KEY_DOMAIN: &str = "gents-file-image-demand-v1";
const RETRY_KEY_DOMAIN: &str = "gents-file-image-retry-v1";
const RECEIPT_DOMAIN: &str = "gents-file-image-receipt-v2";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageDemand {
    pub admission: HydrationRequest,
    pub request_doc_id: String,
    pub tool_call_doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum FileImageSelection {
    Rejected,
    Missing,
    Selected { document: String },
}

impl FileImageDemand {
    pub fn reference_fields(&self) -> [(&'static str, &str); 7] {
        let scope = &self.admission;
        [
            ("request_key", &scope.request_key),
            ("peer_id", &scope.peer_id),
            ("requester_did", &scope.requester_did),
            ("node_did", &scope.node_did),
            ("session_id", &scope.session_id),
            ("request_doc_id", &self.request_doc_id),
            ("tool_call_doc_id", &self.tool_call_doc_id),
        ]
    }

    pub fn derive_request_key(&self) -> String {
        let scope = &self.admission;
        let payload = canonical_domain_payload(
            KEY_DOMAIN,
            [
                scope.peer_id.as_str(),
                scope.requester_did.as_str(),
                scope.node_did.as_str(),
                scope.session_id.as_str(),
                self.request_doc_id.as_str(),
                self.tool_call_doc_id.as_str(),
            ],
        );
        bs58::encode(Sha256::digest(payload)).into_string()
    }

    /// One explicit retry keeps its identifier across transport cancellation or
    /// replay. Its fresh demand preserves earlier terminal receipts unchanged.
    pub fn derive_retry_request_key(&self, retry_id: &str) -> String {
        let original = self.derive_request_key();
        let payload = canonical_domain_payload(RETRY_KEY_DOMAIN, [original.as_str(), retry_id]);
        bs58::encode(Sha256::digest(payload)).into_string()
    }

    pub fn validate_reference(&self) -> Result<()> {
        anyhow::ensure!(
            self.reference_fields()
                .iter()
                .all(|(_, value)| !value.is_empty()),
            "file image demand has an empty reference"
        );
        Ok(())
    }
}

/// The node signs the complete demand and exact observation selection. An
/// acknowledged absence is distinct from an unverified or undelivered image.
/// Version 2 authorizes the named requester to install a local reader relation
/// on the exact selected observation, under the signer's existing ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageHydrationReceipt {
    pub version: u8,
    pub demand: FileImageDemand,
    pub selection: FileImageSelection,
    pub detail: String,
    pub processed_at: String,
    pub signer_did: String,
    pub signature: Vec<u8>,
}

impl FileImageHydrationReceipt {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let version = self.version.to_string();
        let scope = &self.demand.admission;
        let selection = serde_json::to_string(&self.selection)?;
        Ok(canonical_domain_payload(
            RECEIPT_DOMAIN,
            [
                version.as_str(),
                scope.request_key.as_str(),
                scope.peer_id.as_str(),
                scope.requester_did.as_str(),
                scope.node_did.as_str(),
                scope.session_id.as_str(),
                self.demand.request_doc_id.as_str(),
                self.demand.tool_call_doc_id.as_str(),
                selection.as_str(),
                self.detail.as_str(),
                self.processed_at.as_str(),
                self.signer_did.as_str(),
            ],
        ))
    }

    pub fn validate_shape(&self) -> Result<()> {
        if !matches!(self.selection, FileImageSelection::Rejected) {
            self.demand.validate_reference()?;
        }
        anyhow::ensure!(
            self.version == FILE_IMAGE_RECEIPT_VERSION,
            "unsupported file image receipt version"
        );
        anyhow::ensure!(
            self.signer_did == self.demand.admission.node_did,
            "file image receipt signer does not own the target node"
        );
        anyhow::ensure!(
            self.signature.len() == 64,
            "invalid file image receipt signature length"
        );
        chrono::DateTime::parse_from_rfc3339(&self.processed_at)?;
        match &self.selection {
            FileImageSelection::Selected { document } => anyhow::ensure!(
                !document.is_empty() && self.detail.is_empty(),
                "invalid selected file image receipt"
            ),
            FileImageSelection::Missing => anyhow::ensure!(
                self.detail.is_empty(),
                "missing file images cannot carry a rejection"
            ),
            FileImageSelection::Rejected => anyhow::ensure!(
                !self.detail.is_empty(),
                "file image rejection requires a reason"
            ),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
