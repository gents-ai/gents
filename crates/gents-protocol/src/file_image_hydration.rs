use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::enrollment::canonical_domain_payload;
use crate::file_inventory::{FileInventoryPageDemand, FILE_INVENTORY_PAGE_LIMIT};
use crate::session_hydration::HydrationRequest;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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

#[derive(Debug, thiserror::Error)]
pub enum FileImageHydrationError {
    #[error("file image demand has an empty reference")]
    EmptyReference,
    #[error(transparent)]
    Inventory(#[from] crate::file_inventory::FileInventoryError),
    #[error("file image selection crossed its metadata reader, root or snapshot")]
    InvalidInventorySelection,
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
    #[error(transparent)]
    ProcessedAt(#[from] chrono::ParseError),
    #[error("unsupported file image receipt version")]
    UnsupportedVersion,
    #[error("file image receipt signer does not own the target node")]
    SignerNotNodeOwner,
    #[error("invalid file image receipt signature length")]
    InvalidSignatureLength,
    #[error("invalid selected file image receipt")]
    InvalidSelected,
    #[error("missing file images cannot carry a rejection")]
    MissingWithRejection,
    #[error("file image rejection requires a reason")]
    RejectionWithoutReason,
}

type Result<T, E = FileImageHydrationError> = std::result::Result<T, E>;

pub const FILE_IMAGE_RECEIPT_VERSION: u8 = 2;
pub const FILE_INVENTORY_IMAGE_RECEIPT_VERSION: u8 = 3;
const KEY_DOMAIN: &str = "gents-file-image-demand-v1";
const INVENTORY_KEY_DOMAIN: &str = "gents-inventory-image-demand-v1";
const RETRY_KEY_DOMAIN: &str = "gents-file-image-retry-v1";
const RECEIPT_DOMAIN: &str = "gents-file-image-receipt-v2";
const INVENTORY_RECEIPT_DOMAIN: &str = "gents-inventory-image-receipt-v1";

/// A source-verified metadata page selects one exact historical observation.
/// Its reader, physical root and snapshot do not authorize reading image bytes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageInventoryReference {
    pub page: FileInventoryPageDemand,
    pub snapshot_digest: String,
    pub entry_index: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageDemand {
    pub admission: HydrationRequest,
    pub request_doc_id: String,
    pub tool_call_doc_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_reference: Option<FileImageInventoryReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum FileImageSelection {
    Rejected,
    Missing,
    Selected { document: String },
}

impl FileImageDemand {
    /// Client-authored collection roots are native replication identities.
    /// Bound requests use a fresh schema while retained author receipts keep
    /// their original collection and signing domain.
    pub fn collection_name(&self) -> &'static str {
        if self.inventory_reference.is_some() {
            crate::schemas::FILE_INVENTORY_IMAGE_REQUEST_NAME
        } else {
            crate::schemas::FILE_IMAGE_HYDRATION_REQUEST_NAME
        }
    }

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
        let reference = self.inventory_reference.as_ref().map(|reference| {
            serde_json::to_string(reference)
                .expect("inventory image references contain only strings and unsigned integers")
        });
        let mut fields = vec![
            scope.peer_id.as_str(),
            scope.requester_did.as_str(),
            scope.node_did.as_str(),
            scope.session_id.as_str(),
            self.request_doc_id.as_str(),
            self.tool_call_doc_id.as_str(),
        ];
        if let Some(reference) = &reference {
            fields.push(reference);
        }
        let payload = canonical_domain_payload(
            if reference.is_some() {
                INVENTORY_KEY_DOMAIN
            } else {
                KEY_DOMAIN
            },
            fields,
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
        if self
            .reference_fields()
            .iter()
            .any(|(_, value)| value.is_empty())
        {
            return Err(FileImageHydrationError::EmptyReference);
        }
        if let Some(reference) = &self.inventory_reference {
            reference.page.validate_reference()?;
            let root = &reference.page.admission.request;
            if reference.snapshot_digest.is_empty()
                || usize::from(reference.entry_index) >= FILE_INVENTORY_PAGE_LIMIT
                || root.peer_id != self.admission.peer_id
                || root.requester_did != self.admission.requester_did
                || root.node_did != self.admission.node_did
                || root.session_id != self.admission.session_id
                || reference
                    .page
                    .cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor.binding.digest != reference.snapshot_digest)
            {
                return Err(FileImageHydrationError::InvalidInventorySelection);
            }
        }
        Ok(())
    }

    pub fn receipt_version(&self) -> u8 {
        if self.inventory_reference.is_some() {
            FILE_INVENTORY_IMAGE_RECEIPT_VERSION
        } else {
            FILE_IMAGE_RECEIPT_VERSION
        }
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
        let reference = self
            .demand
            .inventory_reference
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let mut fields = vec![
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
        ];
        if let Some(reference) = &reference {
            fields.push(reference);
        }
        Ok(canonical_domain_payload(
            if reference.is_some() {
                INVENTORY_RECEIPT_DOMAIN
            } else {
                RECEIPT_DOMAIN
            },
            fields,
        ))
    }

    pub fn validate_shape(&self) -> Result<()> {
        if !matches!(self.selection, FileImageSelection::Rejected) {
            self.demand.validate_reference()?;
        }
        if self.version != self.demand.receipt_version() {
            return Err(FileImageHydrationError::UnsupportedVersion);
        }
        if self.signer_did != self.demand.admission.node_did {
            return Err(FileImageHydrationError::SignerNotNodeOwner);
        }
        if self.signature.len() != 64 {
            return Err(FileImageHydrationError::InvalidSignatureLength);
        }
        chrono::DateTime::parse_from_rfc3339(&self.processed_at)?;
        match &self.selection {
            FileImageSelection::Selected { document } => {
                if document.is_empty() || !self.detail.is_empty() {
                    return Err(FileImageHydrationError::InvalidSelected);
                }
            }
            FileImageSelection::Missing => {
                if !self.detail.is_empty() {
                    return Err(FileImageHydrationError::MissingWithRejection);
                }
            }
            FileImageSelection::Rejected => {
                if self.detail.is_empty() {
                    return Err(FileImageHydrationError::RejectionWithoutReason);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
