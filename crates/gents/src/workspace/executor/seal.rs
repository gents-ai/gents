use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

use super::super::action_plan::{ActionPlan, HostAction, SealWorkspaceAction};
use super::super::adapter::{capture_seal_snapshot, write_seal_marker};
use super::super::binding::release_binding;
use super::super::documents::{
    writer_receipt_id, IsolatedWorkspaceDoc, WorkspaceDocuments, WorkspacePlacementDoc,
    WorkspaceReceiptDoc, LIFECYCLE_READY, LIFECYCLE_SEALED, RECEIPT_KIND_WRITER,
};
use super::super::journal::{self, ActionJournalEntry, ActionJournalState};
use super::{HostExecuteError, HostExecutorContext};
use crate::toolset::normalize_workspace_lifecycle_state;

const DIFF_ARTIFACT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealWorkspaceOutcome {
    pub workspace: IsolatedWorkspaceDoc,
    pub placement: WorkspacePlacementDoc,
    pub receipt: WorkspaceReceiptDoc,
}

pub(super) fn apply_seal_snapshot(
    workspace: &mut IsolatedWorkspaceDoc,
    placement: &mut WorkspacePlacementDoc,
    dest: &Path,
    seal_hash: &str,
) -> Result<()> {
    write_seal_marker(dest, seal_hash, &workspace.base_sha)?;
    workspace.seal_hash = Some(seal_hash.to_owned());
    workspace.lifecycle_state = LIFECYCLE_SEALED.to_owned();
    placement.observed_tree_hash = seal_hash.to_owned();
    Ok(())
}

pub(super) fn persist_sealed_workspace_documents(
    workspace: &IsolatedWorkspaceDoc,
    placement: &WorkspacePlacementDoc,
    documents: &mut dyn WorkspaceDocuments,
) -> Result<()> {
    documents.write_isolated_workspace(workspace.clone())?;
    documents.write_placement(placement.clone())?;
    Ok(())
}

pub fn execute_seal_workspace_plan(
    plan: &ActionPlan,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<SealWorkspaceOutcome, HostExecuteError> {
    if !journal::action_journal_prefix_legal(journal) {
        return Err(HostExecuteError::denied("illegal action journal prefix"));
    }
    plan.validate_against(&ctx.capabilities)
        .map_err(|err| HostExecuteError::denied(err.to_string()))?;
    if plan.actions.len() != 1 {
        return Err(HostExecuteError::denied(
            "seal_workspace executor only runs a single seal_workspace action",
        ));
    }
    let action = match &plan.actions[0] {
        HostAction::SealWorkspace(action) => action,
        other => {
            return Err(HostExecuteError::denied(format!(
                "seal_workspace executor cannot run {}",
                other.type_name()
            )));
        }
    };
    if journal::current_state(journal, 0).is_none() {
        journal::advance(journal, 0, ActionJournalState::Validated);
    }
    seal_workspace_action(action, journal, ctx)
}

fn seal_workspace_action(
    action: &SealWorkspaceAction,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<SealWorkspaceOutcome, HostExecuteError> {
    if matches!(
        journal::current_state(journal, 0),
        Some(ActionJournalState::ResultDocsWritten)
    ) {
        let outcome = load_written_seal(&action.workspace_id, action, ctx.documents)
            .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
        if outcome.workspace.owner_node_did != ctx.owner_node_did {
            return Err(HostExecuteError::denied(
                "receipt replay belongs to another agent",
            ));
        }
        return Ok(outcome);
    }

    if matches!(
        journal::current_state(journal, 0),
        Some(ActionJournalState::Validated)
    ) {
        journal::advance(journal, 0, ActionJournalState::Executing);
    }

    let mut workspace = ctx
        .documents
        .load_isolated_workspace(&action.workspace_id)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?
        .ok_or_else(|| {
            HostExecuteError::failed(
                format!("IsolatedWorkspace {} not found", action.workspace_id),
                false,
                None,
            )
        })?;
    let mut placement = ctx
        .documents
        .load_placement(&action.workspace_id)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?
        .ok_or_else(|| {
            HostExecuteError::failed(
                format!("WorkspacePlacement {} not found", action.workspace_id),
                false,
                None,
            )
        })?;
    if workspace.owner_node_did != ctx.owner_node_did {
        return Err(HostExecuteError::denied(format!(
            "workspace {} is owned by {}, not executing node {}",
            action.workspace_id, workspace.owner_node_did, ctx.owner_node_did
        )));
    }

    let receipt_id = writer_receipt_id(&workspace.workspace_id, &action.produced_by_request_id);
    if let Some(receipt) = ctx
        .documents
        .load_receipts(&workspace.workspace_id)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?
        .into_iter()
        .find(|receipt| receipt.receipt_id == receipt_id)
    {
        validate_receipt_binding(
            &workspace,
            &receipt,
            RECEIPT_KIND_WRITER,
            &action.produced_by_request_id,
            &action.produced_by_request_doc_id,
        )
        .map_err(|err| HostExecuteError::denied(err.to_string()))?;
        if !Path::new(&placement.host_path).exists() {
            journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
            return Ok(SealWorkspaceOutcome {
                workspace,
                placement,
                receipt,
            });
        }
    }

    let dest = PathBuf::from(&placement.host_path);
    super::super::adapter::verify_workspace_identity(
        &ctx.repository.host_path,
        &dest,
        &workspace.identity(),
    )
    .map_err(|err| HostExecuteError::failed(err.to_string(), true, None))?;
    let snapshot = capture_seal_snapshot(&dest, &workspace.identity())
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
    let seal_hash = snapshot.tree_hash.clone();

    let lifecycle = normalize_workspace_lifecycle_state(&workspace.lifecycle_state);
    if lifecycle == Some(LIFECYCLE_SEALED) {
        let existing = workspace.seal_hash.as_deref().unwrap_or_default();
        if existing != seal_hash {
            return Err(HostExecuteError::failed(
                format!(
                    "sealed workspace {} hash {existing} does not match live tree {seal_hash}",
                    action.workspace_id
                ),
                false,
                None,
            ));
        }
        journal::advance(journal, 0, ActionJournalState::EffectObserved);
        placement.observed_tree_hash = seal_hash;
        let outcome = persist_seal_docs(action, workspace, placement, &snapshot, ctx.documents)
            .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
        journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
        return Ok(outcome);
    }

    if lifecycle != Some(LIFECYCLE_READY) {
        return Err(HostExecuteError::denied(format!(
            "workspace {} in state {} cannot be sealed",
            action.workspace_id, workspace.lifecycle_state
        )));
    }

    apply_seal_snapshot(&mut workspace, &mut placement, &dest, &seal_hash)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;

    journal::advance(journal, 0, ActionJournalState::EffectObserved);
    let outcome = persist_seal_docs(action, workspace, placement, &snapshot, ctx.documents)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
    journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
    Ok(outcome)
}

fn persist_seal_docs(
    action: &SealWorkspaceAction,
    workspace: IsolatedWorkspaceDoc,
    placement: WorkspacePlacementDoc,
    snapshot: &super::super::adapter::SealSnapshot,
    documents: &mut dyn WorkspaceDocuments,
) -> Result<SealWorkspaceOutcome> {
    let seal_hash = workspace
        .seal_hash
        .clone()
        .ok_or_else(|| anyhow!("seal persist requires seal_hash"))?;
    let receipt_id = writer_receipt_id(&workspace.workspace_id, &action.produced_by_request_id);
    let receipt = if let Some(existing) = documents
        .load_receipts(&workspace.workspace_id)?
        .into_iter()
        .find(|receipt| receipt.receipt_id == receipt_id)
    {
        if existing.seal_hash != seal_hash {
            bail!(
                "existing writer receipt {} has seal_hash {}, not {seal_hash}",
                existing.receipt_id,
                existing.seal_hash
            );
        }
        validate_receipt_binding(
            &workspace,
            &existing,
            RECEIPT_KIND_WRITER,
            &action.produced_by_request_id,
            &action.produced_by_request_doc_id,
        )?;
        existing
    } else {
        WorkspaceReceiptDoc {
            path_capability_digest: workspace.path_capability.digest(),
            receipt_id,
            workspace_id: workspace.workspace_id.clone(),
            produced_by_request_id: action.produced_by_request_id.clone(),
            produced_by_request_doc_id: action.produced_by_request_doc_id.clone(),
            kind: RECEIPT_KIND_WRITER.to_string(),
            base_sha: workspace.base_sha.clone(),
            seal_hash: seal_hash.clone(),
            work_unit_id: Some(workspace.work_unit_id.clone()),
            caused_by_correlation: Some(workspace.caused_by_correlation.clone()),
            head_sha: None,
            changed_files: changed_files_json(&snapshot.changed_files),
            diff_artifact: bound_diff_artifact(&snapshot.diff),
            unresolved_conflicts: None,
            integration_instructions: None,
        }
    };

    let bindings = documents.load_bindings(&workspace.workspace_id)?;
    for binding in bindings {
        if binding.is_active_read_write() {
            documents.write_binding(release_binding(binding))?;
        }
    }
    persist_sealed_workspace_documents(&workspace, &placement, documents)?;
    documents.write_receipt(receipt.clone())?;
    Ok(SealWorkspaceOutcome {
        workspace,
        placement,
        receipt,
    })
}

pub(super) fn validate_receipt_binding(
    workspace: &IsolatedWorkspaceDoc,
    receipt: &WorkspaceReceiptDoc,
    kind: &str,
    request_id: &str,
    request_doc_id: &str,
) -> Result<()> {
    workspace.path_capability.validate()?;
    if receipt.workspace_id != workspace.workspace_id
        || receipt.base_sha != workspace.base_sha
        || Some(receipt.seal_hash.as_str()) != workspace.seal_hash.as_deref()
        || receipt.path_capability_digest != workspace.path_capability.digest()
        || receipt.kind != kind
        || receipt.produced_by_request_id != request_id
        || receipt.produced_by_request_doc_id != request_doc_id
    {
        bail!(
            "workspace receipt does not match immutable workspace/base/capability/seal/request binding"
        );
    }
    Ok(())
}

fn load_written_seal(
    workspace_id: &str,
    action: &SealWorkspaceAction,
    documents: &dyn WorkspaceDocuments,
) -> Result<SealWorkspaceOutcome> {
    let workspace = documents
        .load_isolated_workspace(workspace_id)?
        .ok_or_else(|| {
            anyhow!("journal ResultDocsWritten but IsolatedWorkspace {workspace_id} missing")
        })?;
    let placement = documents.load_placement(workspace_id)?.ok_or_else(|| {
        anyhow!("journal ResultDocsWritten but WorkspacePlacement {workspace_id} missing")
    })?;
    let receipt_id = writer_receipt_id(workspace_id, &action.produced_by_request_id);
    let receipt = documents
        .load_receipts(workspace_id)?
        .into_iter()
        .find(|receipt| receipt.receipt_id == receipt_id)
        .ok_or_else(|| {
            anyhow!("journal ResultDocsWritten but WorkspaceReceipt {receipt_id} missing")
        })?;
    validate_receipt_binding(
        &workspace,
        &receipt,
        RECEIPT_KIND_WRITER,
        &action.produced_by_request_id,
        &action.produced_by_request_doc_id,
    )?;
    Ok(SealWorkspaceOutcome {
        workspace,
        placement,
        receipt,
    })
}

pub(super) fn changed_files_json(files: &[String]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    serde_json::to_string(files).ok()
}

pub(super) fn bound_diff_artifact(diff: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(diff).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    if text.len() <= DIFF_ARTIFACT_LIMIT {
        return Some(text.to_string());
    }
    let mut end = DIFF_ARTIFACT_LIMIT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(text[..end].to_string())
}
