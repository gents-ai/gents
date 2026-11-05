use super::FileInventorySnapshot;
use crate::store::config_client::ConfigAccess;
use anyhow::{ensure, Context, Result};
use gents_protocol::file_inventory::{
    DirectFileInventoryRequest, FileInventoryAdmission, FileInventoryCursor,
    FileInventoryObservation, FileInventoryReadAction, FileInventorySnapshotView,
    FileInventoryViewCursor,
};
use gents_protocol::session_hydration::HydrationRequest;

/// Read one native snapshot using the authenticated client's actor. Owner
/// observations use the same embedded transaction as the selected documents.
pub async fn read_direct_inventory(
    access: &ConfigAccess,
    request: &DirectFileInventoryRequest,
) -> Result<Option<FileInventoryObservation>> {
    let node_did = request.node_did.trim();
    let session_id = request.session_id.trim();
    ensure!(
        !node_did.is_empty() && !session_id.is_empty() && !request.peer_id.trim().is_empty(),
        "file inventory requires exact node, session, and reader route"
    );
    let session = access
        .transact_readonly("file_inventory.client_scope", |txn| {
            Box::pin(crate::session::load_agent_session_owner_in_txn(
                txn, node_did, session_id,
            ))
        })
        .await?;
    let Some(session) = session else {
        return Ok(None);
    };
    let admission = FileInventoryAdmission {
        request: HydrationRequest {
            request_key: format!("direct:{session_id}"),
            peer_id: request.peer_id.clone(),
            requester_did: access
                .actor_did()
                .context("file inventory requires an authenticated reader")?,
            node_did: node_did.to_owned(),
            session_id: session_id.to_owned(),
        },
        session_doc_id: session.doc_id,
    };
    let snapshot = FileInventorySnapshot::load_on(access, &admission).await?;
    let cursor = match &request.action {
        FileInventoryReadAction::Read { cursor } => cursor.as_ref(),
        _ => None,
    };
    if let Some(cursor) = cursor {
        ensure!(
            cursor.snapshot_digest == snapshot.binding().digest,
            "The file inventory changed. Reload its changes."
        );
    }
    let attempt_id = match &request.action {
        FileInventoryReadAction::Refresh { attempt_id } => {
            uuid::Uuid::parse_str(attempt_id)?.to_string()
        }
        _ => cursor
            .map(|cursor| cursor.attempt_id.clone())
            .unwrap_or_else(|| snapshot.binding().digest.clone()),
    };
    let page_cursor =
        cursor
            .filter(|cursor| cursor.offset != 0)
            .map(|cursor| FileInventoryCursor {
                binding: snapshot.binding().clone(),
                offset: cursor.offset,
            });
    let page = snapshot.page(page_cursor.as_ref())?;
    let next = page.next.map(|cursor| FileInventoryViewCursor {
        attempt_id: attempt_id.clone(),
        snapshot_digest: page.binding.digest.clone(),
        offset: cursor.offset,
    });
    let known_inventory = page.binding.coverage.pending == 0 && page.binding.coverage.unknown == 0;
    let current = FileInventorySnapshotView {
        attempt_id: attempt_id.clone(),
        received_entries: snapshot.entries().len().try_into()?,
        complete: true,
        known_inventory,
        binding: page.binding,
        offset: page.offset,
        entries: page.entries,
        next,
    };
    Ok(Some(FileInventoryObservation {
        admission,
        attempt_id,
        current: Some(current),
        previous: None,
        refusal: None,
    }))
}
