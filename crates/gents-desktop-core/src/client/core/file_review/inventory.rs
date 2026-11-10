use anyhow::{ensure, Context, Result};
use gents::store::config_client::ConfigAccess;
use gents::tools::call::lifecycle::{load_tool_file_change_observation, ToolFileChangeRead};
use gents_protocol::file_changes::{ToolFileChanges, FILE_CHANGES_VERSION};
use gents_protocol::file_inventory::{
    FileChangeMetadata, FileInventoryEntry, FileInventoryImage, FileInventoryImageReference,
    FileInventoryObservation, FileInventoryReadAction, FileInventorySnapshotView,
};

use super::ClientCore;

impl ClientCore {
    /// The verified inventory selects a physical reference, never read authority.
    /// Independent readers bind their own admission to the received source page.
    /// The selected capture retains its canonical author and accepted-tool checks.
    pub async fn inventory_file_change(
        &self,
        node_did: &str,
        session_id: &str,
        reference: &FileInventoryImageReference,
        retry_id: Option<&str>,
    ) -> Result<Option<FileInventoryImage>> {
        retry_id.map(uuid::Uuid::parse_str).transpose()?;
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
        let access = ConfigAccess::Graphql(
            self.operator_observation_graphql(&entry.scope.node_did)
                .context("Connect the runtime that owns these recorded file versions.")?,
        );
        let retained = match load_tool_file_change_observation(
            &access,
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
                ..
            } => Some((document_id, changes)),
            ToolFileChangeRead::Missing
            | ToolFileChangeRead::Pending
            | ToolFileChangeRead::Unsupported => None,
        };
        retained
            .map(|(document, changes)| match_observation(reference, entry, &document, changes))
            .transpose()
    }
}

fn selected_view<'a>(
    observation: &'a FileInventoryObservation,
    reference: &FileInventoryImageReference,
) -> Result<&'a FileInventorySnapshotView> {
    let cursor = &reference.cursor;
    observation
        .current
        .iter()
        .chain(observation.previous.iter())
        .find(|view| {
            view.attempt_id == cursor.attempt_id
                && view.binding.digest == cursor.snapshot_digest
                && view.offset == cursor.offset
        })
        .context("The recorded file selection changed. Open the changes again.")
}

fn selected_entry<'a>(
    observation: &'a FileInventoryObservation,
    reference: &FileInventoryImageReference,
) -> Result<&'a FileInventoryEntry> {
    selected_view(observation, reference)?
        .entries
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
