use super::{selection::load_hydration_selection_in_txn, *};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::required_rows;
use crate::tool_call_lifecycle::ToolCallLifecycle;
use gents_protocol::enrollment::canonical_domain_payload;
use gents_protocol::file_image_hydration::FileImageScope;
use gents_protocol::file_inventory::{
    coherent_inventory_entries, FileChangeMetadata, FileInventoryAdmission, FileInventoryBinding,
    FileInventoryCoverage, FileInventoryCursor, FileInventoryEntry, FileInventoryPage,
    FILE_INVENTORY_PAGE_LIMIT,
};
use gents_protocol::session_hydration::SessionHydrationCollection;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct InventoryRejection(&'static str);

pub(super) fn refusal_reason(
    error: &anyhow::Error,
) -> gents_protocol::file_inventory::FileInventoryRefusalReason {
    use gents_protocol::file_inventory::FileInventoryRefusalReason;
    if error.is::<InventoryRejection>() {
        FileInventoryRefusalReason::Rejected
    } else {
        FileInventoryRefusalReason::Unavailable
    }
}

/// Source-local facts, not transfer authorization. The delivery owner must
/// still validate current enrollment and pairing before signing or transferring
/// metadata. Construction reads membership, accepted tools, projections and
/// authenticated owners in one transaction and never reads retained image bytes.
#[derive(Debug, Clone)]
pub struct FileInventorySnapshot {
    binding: FileInventoryBinding,
    entries: Vec<FileInventoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileInventoryPageError {
    #[error("file inventory snapshot changed; restart enumeration")]
    StaleSnapshot,
    #[error("file inventory cursor is not a continuation boundary")]
    InvalidCursor,
}

#[derive(Deserialize, Serialize)]
struct Projection {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_doc_id: String,
    observation_doc_id: String,
    request_doc_id: String,
    node_did: String,
    session_id: String,
    requester_did: String,
    changes: Vec<FileChangeMetadata>,
}

#[derive(Serialize)]
#[serde(tag = "capture")]
enum Capture {
    Pending,
    Unknown,
    Observed {
        projection: Projection,
        owner: String,
    },
}

#[derive(Serialize)]
struct ToolFact {
    tool_call_doc_id: String,
    request_doc_id: String,
    scope: FileImageScope,
    state: String,
    accepted_header_doc_id: String,
    execution_generation: String,
    capture: Capture,
}

impl FileInventorySnapshot {
    pub fn binding(&self) -> &FileInventoryBinding {
        &self.binding
    }

    pub fn entries(&self) -> &[FileInventoryEntry] {
        &self.entries
    }

    /// These pages are unsigned. Enrollment and pairing authorization remain
    /// with the delivery owner; a page never grants retained-image access.
    pub fn page(
        &self,
        cursor: Option<&FileInventoryCursor>,
    ) -> std::result::Result<FileInventoryPage, FileInventoryPageError> {
        let offset = match cursor {
            None => 0,
            Some(cursor) => {
                if cursor.binding != self.binding {
                    return Err(FileInventoryPageError::StaleSnapshot);
                }
                if cursor.offset == 0
                    || cursor.offset % FILE_INVENTORY_PAGE_LIMIT as u64 != 0
                    || cursor.offset >= self.binding.coverage.entries
                {
                    return Err(FileInventoryPageError::InvalidCursor);
                }
                usize::try_from(cursor.offset).map_err(|_| FileInventoryPageError::InvalidCursor)?
            }
        };
        let following = offset
            .saturating_add(FILE_INVENTORY_PAGE_LIMIT)
            .min(self.entries.len());
        Ok(FileInventoryPage {
            binding: self.binding.clone(),
            offset: offset as u64,
            entries: self.entries[offset..following].to_vec(),
            next: (following < self.entries.len()).then(|| FileInventoryCursor {
                binding: self.binding.clone(),
                offset: following as u64,
            }),
        })
    }

    pub async fn load(
        node: &Arc<EmbeddedNode>,
        admission: &FileInventoryAdmission,
    ) -> Result<Self> {
        anyhow::ensure!(
            node.node_identity_did() == Some(admission.request.node_did.as_str())
                && [
                    admission.session_doc_id.as_str(),
                    &admission.request.request_key,
                    &admission.request.peer_id,
                    &admission.request.requester_did,
                    &admission.request.session_id
                ]
                .iter()
                .all(|value| !value.is_empty()),
            InventoryRejection("file inventory requires an exact source-local session reference")
        );
        ConfigAccess::transact_local_readonly(node, None, "file_inventory.snapshot", |txn| {
            Box::pin(Self::load_in_txn(txn, node, admission))
        })
        .await
    }

    async fn load_in_txn(
        txn: &ConfigApplyTxn<'_>,
        node: &Arc<EmbeddedNode>,
        admission: &FileInventoryAdmission,
    ) -> Result<Self> {
        let request = &admission.request;
        let session = crate::session::load_agent_session_row_in_txn(
            txn,
            &request.node_did,
            &request.session_id,
            Some(&request.requester_did),
        )
        .await?
        .context("file inventory session is unavailable")?;
        anyhow::ensure!(
            session.doc_id == admission.session_doc_id,
            InventoryRejection("file inventory session identity changed")
        );
        let selected = load_hydration_selection_in_txn(txn, request).await?;
        let mut facts = Vec::new();
        let mut entries = Vec::new();
        let mut pending = 0;
        let mut unknown = 0;
        for document in &selected.documents {
            if document.collection != SessionHydrationCollection::AgentToolCall {
                continue;
            }
            let tool = ToolCallLifecycle::load_by_doc_id_in_txn(
                txn,
                node.clone(),
                &document.doc_id,
                &document.node_did,
                &document.session_id,
                Some(&document.requester_did),
            )
            .await?
            .context("file inventory accepted tool is unavailable")?;
            let request_doc_id =
                tool.request_doc_id()
                    .map(str::to_owned)
                    .ok_or(InventoryRejection(
                        "file inventory tool omitted its physical request",
                    ))?;
            let scope = FileImageScope {
                node_did: tool.node_did().to_owned(),
                session_id: tool.session_id().to_owned(),
                requester_did: tool
                    .requester_did()
                    .map(str::to_owned)
                    .filter(|value| !value.is_empty())
                    .ok_or(InventoryRejection(
                        "file inventory tool omitted its requester",
                    ))?,
            };
            let query = format!(
                r#"{{ ToolFileInventory(filter: {{ tool_call_doc_id: {{ _eq: "{}" }} }}, limit: 2) {{
                    _docID tool_call_doc_id observation_doc_id request_doc_id node_did session_id requester_did changes
                }} }}"#,
                escape_graphql_string(&document.doc_id),
            );
            let response = txn.execute_local_response(&query).await?;
            let mut projections = required_rows::<Projection>(&response, "ToolFileInventory")?;
            anyhow::ensure!(
                projections.len() <= 1,
                InventoryRejection("file inventory has ambiguous projections")
            );
            let capture = if !tool.state().is_terminal() {
                anyhow::ensure!(
                    projections.is_empty(),
                    InventoryRejection("active tool has an uncommitted file inventory")
                );
                pending += 1;
                Capture::Pending
            } else if let Some(projection) = projections.pop() {
                anyhow::ensure!(
                    !projection.doc_id.is_empty()
                        && !projection.observation_doc_id.is_empty()
                        && projection.tool_call_doc_id == document.doc_id
                        && projection.request_doc_id == request_doc_id
                        && projection.node_did == scope.node_did
                        && projection.session_id == scope.session_id
                        && projection.requester_did == scope.requester_did
                        && !projection.changes.is_empty(),
                    InventoryRejection(
                        "file inventory projection conflicts with its accepted physical tool"
                    )
                );
                let owners = txn
                    .document_owners(
                        "ToolFileInventory",
                        std::slice::from_ref(&projection.doc_id),
                    )
                    .await?;
                anyhow::ensure!(
                    owners.len() == 1
                        && owners[0].doc_id == projection.doc_id
                        && owners[0]
                            .owner
                            .as_ref()
                            .is_some_and(|owner| owner.as_str() == scope.node_did),
                    InventoryRejection("file inventory projection is not owned by its source node")
                );
                entries.extend(projection.changes.iter().map(|change| FileInventoryEntry {
                    tool_call_doc_id: document.doc_id.clone(),
                    request_doc_id: request_doc_id.clone(),
                    scope: scope.clone(),
                    projection_doc_id: projection.doc_id.clone(),
                    observation_doc_id: projection.observation_doc_id.clone(),
                    change: change.clone(),
                }));
                Capture::Observed {
                    projection,
                    owner: scope.node_did.clone(),
                }
            } else {
                unknown += 1;
                Capture::Unknown
            };
            facts.push(ToolFact {
                tool_call_doc_id: document.doc_id.clone(),
                request_doc_id,
                scope,
                state: tool.state().as_str().to_owned(),
                accepted_header_doc_id: tool.accepted_header_doc_id().map(str::to_owned).ok_or(
                    InventoryRejection("file inventory tool omitted its accepted header"),
                )?,
                execution_generation: tool.execution_generation().map(str::to_owned).ok_or(
                    InventoryRejection("file inventory tool omitted its execution generation"),
                )?,
                capture,
            });
        }
        anyhow::ensure!(
            coherent_inventory_entries(&entries),
            InventoryRejection("file inventory entries are incoherent")
        );
        let documents = selected
            .documents
            .iter()
            .map(|document| {
                (
                    document.collection,
                    &document.doc_id,
                    &document.requester_did,
                    &document.node_did,
                    &document.session_id,
                )
            })
            .collect::<Vec<_>>();
        let preimage = serde_json::to_string(&(admission, documents, &facts))?;
        let digest = bs58::encode(Sha256::digest(canonical_domain_payload(
            "gents-file-inventory-snapshot-v1",
            [preimage.as_str()],
        )))
        .into_string();
        Ok(Self {
            binding: FileInventoryBinding {
                admission: admission.clone(),
                digest,
                coverage: FileInventoryCoverage {
                    entries: entries.len() as u64,
                    pending,
                    unknown,
                },
            },
            entries,
        })
    }
}

#[cfg(test)]
mod tests;
