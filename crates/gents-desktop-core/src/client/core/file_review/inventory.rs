use anyhow::{ensure, Context, Result};
use gents::config_client::ConfigAccess;
use gents::tool_call_lifecycle::{load_tool_file_change_observation, ToolFileChangeRead};
use gents_protocol::file_changes::{ToolFileChanges, FILE_CHANGES_VERSION};
use gents_protocol::file_image_hydration::FileImageDemand;
use gents_protocol::file_inventory::{
    FileChangeMetadata, FileInventoryEntry, FileInventoryImage, FileInventoryImageReference,
    FileInventoryObservation, FileInventoryReadAction,
};
use gents_protocol::session_hydration::HydrationRequest;

use super::{resumable_image_demand, ClientCore};

impl ClientCore {
    /// The verified inventory selects a physical reference, never read authority.
    /// Image reads retain the origin's own admission, accepted-tool and byte
    /// verification owners; foreign requesters are not relabeled to this client.
    pub async fn inventory_file_change(
        &self,
        node_did: &str,
        session_id: &str,
        reference: &FileInventoryImageReference,
        retry_id: Option<&str>,
    ) -> Result<Option<FileInventoryImage>> {
        let retry = retry_id.map(uuid::Uuid::parse_str).transpose()?;
        let observation = self
            .session_file_inventory(
                node_did,
                session_id,
                FileInventoryReadAction::Read {
                    cursor: Some(reference.cursor.clone()),
                },
            )
            .await?
            .context("The recorded file inventory is unavailable. Reload its changes.")?;
        let entry = selected_entry(&observation, reference)?.clone();
        let records = self.sync_state.records();
        let mut routes = records
            .iter()
            .filter(|record| record.node_did == entry.scope.node_did);
        let route = routes
            .next()
            .context("Connect the runtime that owns these recorded file versions.")?;
        ensure!(
            routes.next().is_none(),
            "multiple runtime routes own this file version"
        );
        let retained = if route.is_enrollment() {
            ensure!(
                entry.scope.requester_did == self.node_identity.did(),
                "These recorded file versions require their original conversation's authorization."
            );
            let mut demand = FileImageDemand {
                admission: HydrationRequest {
                    request_key: String::new(),
                    peer_id: self.local_peer_id().into(),
                    requester_did: entry.scope.requester_did.clone(),
                    node_did: entry.scope.node_did.clone(),
                    session_id: entry.scope.session_id.clone(),
                },
                request_doc_id: entry.request_doc_id.clone(),
                tool_call_doc_id: entry.tool_call_doc_id.clone(),
            };
            demand.admission.request_key = match retry {
                Some(id) => demand.derive_retry_request_key(&id.to_string()),
                None => demand.derive_request_key(),
            };
            demand.validate_reference()?;
            if retry.is_none() {
                let access = self
                    .store_access
                    .lock()
                    .await
                    .as_ref()
                    .context("local store access is unavailable")?
                    .clone();
                demand = resumable_image_demand(&access, &demand, &self.node_identity).await?;
            }
            self.remote_file_observation(demand).await?
        } else {
            let route = route.clone();
            let endpoint = tokio::task::spawn_blocking(move || {
                crate::local_runtime::cached_operator_endpoint(&route)
                    .filter(|endpoint| !endpoint.is_anonymous())
            })
            .await
            .context("reading cached file review authority")?
            .context("Runtime authorization is unavailable. Reconnect to review file versions.")?;
            match load_tool_file_change_observation(
                &ConfigAccess::Graphql(endpoint),
                &entry.tool_call_doc_id,
                &entry.request_doc_id,
                &entry.scope.node_did,
                &entry.scope.session_id,
                Some(&entry.scope.requester_did),
            )
            .await?
            {
                ToolFileChangeRead::Retained {
                    document_id,
                    changes,
                } => Some((document_id, changes)),
                ToolFileChangeRead::Missing
                | ToolFileChangeRead::Pending
                | ToolFileChangeRead::Unsupported => None,
            }
        };
        retained
            .map(|(document, changes)| match_observation(reference, entry, &document, changes))
            .transpose()
    }
}

fn selected_entry<'a>(
    observation: &'a FileInventoryObservation,
    reference: &FileInventoryImageReference,
) -> Result<&'a FileInventoryEntry> {
    let cursor = &reference.cursor;
    let view = observation
        .current
        .iter()
        .chain(observation.previous.iter())
        .find(|view| {
            view.attempt_id == cursor.attempt_id
                && view.binding.digest == cursor.snapshot_digest
                && view.offset == cursor.offset
        })
        .context("The recorded file selection changed. Open the changes again.")?;
    view.entries
        .get(usize::from(reference.entry_index))
        .context("The recorded file selection is outside its received page.")
}

fn match_observation(
    reference: &FileInventoryImageReference,
    entry: FileInventoryEntry,
    document: &str,
    changes: ToolFileChanges,
) -> Result<FileInventoryImage> {
    ensure!(
        document == entry.observation_doc_id
            && changes.version == FILE_CHANGES_VERSION
            && changes.tool_call_doc_id == entry.tool_call_doc_id,
        "The recorded file versions do not match this inventory observation."
    );
    let mut matching = changes
        .changes
        .into_iter()
        .filter(|change| change.root == entry.change.root && change.path == entry.change.path);
    let change = matching
        .next()
        .context("The recorded file version is unavailable.")?;
    ensure!(
        matching.next().is_none() && FileChangeMetadata::from(&change) == entry.change,
        "The recorded file versions differ from the signed inventory."
    );
    Ok(FileInventoryImage {
        reference: reference.clone(),
        entry,
        change,
    })
}

#[cfg(test)]
mod tests;
