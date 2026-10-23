use anyhow::Result;
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde::{Deserialize, Serialize};

use crate::enrollment::canonical_domain_payload;
use crate::file_changes::{FileChangeImages, FileImage, ImageFailure};
use crate::file_image_hydration::FileImageScope;
use crate::session_hydration::HydrationRequest;

pub const FILE_INVENTORY_PAGE_LIMIT: usize = 64;
pub const FILE_INVENTORY_RECEIPT_VERSION: u8 = 2;
const RECEIPT_DOMAIN: &str = "gents-file-inventory-page-v2";

/// Metadata is a separate projection: observing it never transfers image bytes
/// or grants a retained-image reader relation. Capture validation stays with
/// the native file-image owner before projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum FileImageMetadata {
    Missing,
    File { content_hash: String },
    Unavailable { reason: ImageFailure },
}

impl From<&FileImage> for FileImageMetadata {
    fn from(image: &FileImage) -> Self {
        match image {
            FileImage::Missing => Self::Missing,
            FileImage::File { content_hash, .. } => Self::File {
                content_hash: content_hash.clone(),
            },
            FileImage::Unavailable { reason } => Self::Unavailable {
                reason: reason.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileChangeMetadata {
    pub root: String,
    pub path: String,
    pub before: FileImageMetadata,
    pub after: FileImageMetadata,
}

impl From<&FileChangeImages> for FileChangeMetadata {
    fn from(change: &FileChangeImages) -> Self {
        Self {
            root: change.root.clone(),
            path: change.path.clone(),
            before: (&change.before).into(),
            after: (&change.after).into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryAdmission {
    pub request: HydrationRequest,
    pub session_doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryCoverage {
    pub entries: u64,
    pub pending: u64,
    pub unknown: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryBinding {
    pub admission: FileInventoryAdmission,
    pub digest: String,
    pub coverage: FileInventoryCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryEntry {
    pub tool_call_doc_id: String,
    pub request_doc_id: String,
    pub scope: FileImageScope,
    pub projection_doc_id: String,
    pub observation_doc_id: String,
    pub change: FileChangeMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryCursor {
    pub binding: FileInventoryBinding,
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryPage {
    pub binding: FileInventoryBinding,
    pub offset: u64,
    pub entries: Vec<FileInventoryEntry>,
    pub next: Option<FileInventoryCursor>,
}

impl FileInventoryPage {
    pub fn well_formed(&self) -> bool {
        let count = self.entries.len() as u64;
        let Some(remaining) = self.binding.coverage.entries.checked_sub(self.offset) else {
            return false;
        };
        let Some(following) = self.offset.checked_add(count) else {
            return false;
        };
        let next = (following < self.binding.coverage.entries).then(|| FileInventoryCursor {
            binding: self.binding.clone(),
            offset: following,
        });
        !self.binding.digest.is_empty()
            && self.entries.len() <= FILE_INVENTORY_PAGE_LIMIT
            && count == remaining.min(FILE_INVENTORY_PAGE_LIMIT as u64)
            && self.next == next
            && coherent_inventory_entries(&self.entries)
    }
}

/// Origin scopes are retained across forks. Coherence checks representation,
/// not whether an entry belongs to the receiver's authorized session closure.
/// That selection remains with the source's native snapshot owner.
pub fn coherent_inventory_entries(entries: &[FileInventoryEntry]) -> bool {
    let mut keys = RapidHashSet::new();
    let mut tools = RapidHashMap::new();
    let mut projections = RapidHashMap::new();
    let mut observations = RapidHashMap::new();
    let mut previous: Option<&str> = None;
    for entry in entries {
        if [
            entry.tool_call_doc_id.as_str(),
            &entry.request_doc_id,
            &entry.projection_doc_id,
            &entry.observation_doc_id,
            &entry.scope.requester_did,
            &entry.scope.node_did,
            &entry.scope.session_id,
            &entry.change.root,
            &entry.change.path,
        ]
        .iter()
        .any(|value| value.is_empty())
            || previous.is_some_and(|tool| tool > entry.tool_call_doc_id.as_str())
            || !keys.insert((
                &entry.projection_doc_id,
                &entry.change.root,
                &entry.change.path,
            ))
        {
            return false;
        }
        previous = Some(&entry.tool_call_doc_id);
        let tool = (
            &entry.request_doc_id,
            &entry.scope,
            &entry.projection_doc_id,
            &entry.observation_doc_id,
        );
        let projection = (
            &entry.tool_call_doc_id,
            &entry.request_doc_id,
            &entry.scope,
            &entry.observation_doc_id,
        );
        if tools
            .insert(&entry.tool_call_doc_id, tool)
            .is_some_and(|prior| prior != tool)
            || projections
                .insert(&entry.projection_doc_id, projection)
                .is_some_and(|prior| prior != projection)
            || observations
                .insert(&entry.observation_doc_id, &entry.projection_doc_id)
                .is_some_and(|prior| prior != &entry.projection_doc_id)
        {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryPageReceipt {
    pub version: u8,
    pub request_key: String,
    pub signer_did: String,
    pub page: FileInventoryPage,
    pub signature: Vec<u8>,
}

impl FileInventoryPageReceipt {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let version = self.version.to_string();
        let page = serde_json::to_string(&self.page)?;
        Ok(canonical_domain_payload(
            RECEIPT_DOMAIN,
            [version.as_str(), &self.request_key, &self.signer_did, &page],
        ))
    }
}
