use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::super::action_plan::{ActionPlan, HostAction, IntegrateWorkspaceAction};
use super::super::adapter::{
    absolute_git_dir, advance_trunk_to_integrate_commit, capture_seal_snapshot, commit_exists,
    prepare_integrate_commit,
};
use super::super::documents::{
    integrator_receipt_id, writer_receipt_id, IsolatedWorkspaceDoc, WorkspaceDocuments,
    WorkspacePlacementDoc, WorkspaceReceiptDoc, LIFECYCLE_SEALED, RECEIPT_KIND_INTEGRATOR,
    RECEIPT_KIND_WRITER,
};
use super::super::journal::{self, ActionJournalEntry, ActionJournalState};
use super::seal::{bound_diff_artifact, changed_files_json, validate_receipt_binding};
use super::{sanitize_fs, workspace_host_path, HostExecuteError, HostExecutorContext};
use crate::tools::native::normalize_workspace_lifecycle_state;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrateWorkspaceOutcome {
    pub workspace: IsolatedWorkspaceDoc,
    pub placement: WorkspacePlacementDoc,
    pub receipt: WorkspaceReceiptDoc,
    pub pending_head_sha: Option<String>,
}

pub fn execute_integrate_workspace_plan(
    plan: &ActionPlan,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<IntegrateWorkspaceOutcome, HostExecuteError> {
    if !journal::action_journal_prefix_legal(journal) {
        return Err(HostExecuteError::denied("illegal action journal prefix"));
    }
    plan.validate_against(&ctx.capabilities)
        .map_err(|err| HostExecuteError::denied(err.to_string()))?;
    if plan.actions.len() != 1 {
        return Err(HostExecuteError::denied(
            "integrate_workspace executor only runs a single integrate_workspace action",
        ));
    }
    let action = match &plan.actions[0] {
        HostAction::IntegrateWorkspace(action) => action,
        other => {
            return Err(HostExecuteError::denied(format!(
                "integrate_workspace executor cannot run {}",
                other.type_name()
            )));
        }
    };
    restore_integrate_journal(journal, &ctx.repository.host_path, &action.workspace_id);
    if !journal::action_journal_prefix_legal(journal) {
        return Err(HostExecuteError::denied(
            "persisted integrate journal has an illegal prefix",
        ));
    }
    if journal::current_state(journal, 0).is_none() {
        journal::advance(journal, 0, ActionJournalState::Validated);
    }
    integrate_workspace_action(action, journal, ctx)
}

fn integrate_workspace_action(
    action: &IntegrateWorkspaceAction,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<IntegrateWorkspaceOutcome, HostExecuteError> {
    let trunk = ctx.repository.host_path.clone();

    if journal::current_state(journal, 0).is_none()
        || matches!(
            journal::current_state(journal, 0),
            Some(ActionJournalState::Validated)
        )
    {
        journal::advance(journal, 0, ActionJournalState::Executing);
    }

    let workspace = ctx
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
    let placement = ctx
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
    let lifecycle = normalize_workspace_lifecycle_state(&workspace.lifecycle_state);
    if lifecycle != Some(LIFECYCLE_SEALED) {
        return Err(HostExecuteError::denied(format!(
            "workspace {} in state {} cannot be integrated; Integrate bindings require Sealed",
            action.workspace_id, workspace.lifecycle_state
        )));
    }
    let seal_hash = workspace
        .seal_hash
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            HostExecuteError::denied(format!(
                "sealed workspace {} is missing seal_hash",
                action.workspace_id
            ))
        })?
        .to_string();
    if placement.observed_tree_hash != seal_hash {
        return Err(HostExecuteError::failed(
            format!(
                "placement observed_tree_hash {} does not match workspace seal_hash {seal_hash}",
                placement.observed_tree_hash
            ),
            false,
            None,
        ));
    }

    let expected = workspace_host_path(
        &trunk,
        &workspace.workspace_id,
        &workspace.branch,
        ctx.ceiling,
    )
    .map_err(|err| HostExecuteError::denied(err.to_string()))?;
    let dest = PathBuf::from(&placement.host_path);
    let dest_canon = dest.canonicalize().unwrap_or_else(|_| dest.clone());
    let expected_canon = expected.canonicalize().unwrap_or_else(|_| expected.clone());
    if dest_canon != expected_canon {
        return Err(HostExecuteError::denied(format!(
            "placement host_path {} does not match host-chosen dest {}",
            dest.display(),
            expected.display()
        )));
    }
    if dest_canon == trunk {
        return Err(HostExecuteError::denied(
            "integrate_workspace refuses to mutate the worker root; trunk is the source checkout",
        ));
    }

    let receipts = ctx
        .documents
        .load_receipts(&workspace.workspace_id)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
    let receipt_id = integrator_receipt_id(&workspace.workspace_id, &action.produced_by_request_id);
    if let Some(existing) = receipts
        .iter()
        .find(|receipt| receipt.receipt_id == receipt_id)
        .cloned()
    {
        validate_receipt_binding(
            &workspace,
            &existing,
            RECEIPT_KIND_INTEGRATOR,
            &action.produced_by_request_id,
            &action.produced_by_request_doc_id,
        )
        .map_err(|err| HostExecuteError::denied(err.to_string()))?;
        if existing.seal_hash != seal_hash {
            return Err(HostExecuteError::failed(
                format!(
                    "existing integrator receipt {} has seal_hash {}, not {seal_hash}",
                    existing.receipt_id, existing.seal_hash
                ),
                false,
                None,
            ));
        }
        let head = existing
            .head_sha
            .as_deref()
            .ok_or_else(|| HostExecuteError::denied("integrator receipt is missing head_sha"))?;
        let has_effect = super::super::adapter::verify_integrate_commit(
            &trunk,
            head,
            &workspace.identity(),
            &seal_hash,
        )
        .map_err(|err| HostExecuteError::denied(err.to_string()))?;
        let marker = load_integrate_marker(&trunk, &action.workspace_id);
        let pending = if !has_effect
            || (marker.is_none() && super::super::adapter::commit_is_in_trunk(&trunk, head))
        {
            None
        } else {
            if let Some(marker) = &marker {
                if marker.seal_hash != seal_hash
                    || marker.request_id != action.produced_by_request_id
                {
                    return Err(HostExecuteError::denied(
                        "integration journal belongs to a different seal/request",
                    ));
                }
            }
            workspace
                .path_capability
                .validate_paths_at(&trunk)
                .map_err(|err| HostExecuteError::denied(err.to_string()))?;
            Some(head.to_owned())
        };
        journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
        if pending.is_some() {
            persist_integrate_journal(
                journal,
                &trunk,
                &action.workspace_id,
                pending.as_deref(),
                &seal_hash,
                &action.produced_by_request_id,
            );
        }
        return Ok(IntegrateWorkspaceOutcome {
            workspace,
            placement,
            receipt: existing,
            pending_head_sha: pending,
        });
    }

    let writer_matches = receipts.iter().any(|receipt| {
        !receipt.produced_by_request_id.trim().is_empty()
            && !receipt.produced_by_request_doc_id.trim().is_empty()
            && receipt.receipt_id
                == writer_receipt_id(&workspace.workspace_id, &receipt.produced_by_request_id)
            && validate_receipt_binding(
                &workspace,
                receipt,
                RECEIPT_KIND_WRITER,
                &receipt.produced_by_request_id,
                &receipt.produced_by_request_doc_id,
            )
            .is_ok()
    });
    if !writer_matches {
        return Err(HostExecuteError::denied(
            "integration requires a matching writer receipt for the sealed workspace/base/capability/tree",
        ));
    }

    let marker = load_integrate_marker(&trunk, &action.workspace_id);
    if let Some(marker) = &marker {
        if marker.pending_head_sha.is_some()
            && (marker.seal_hash != seal_hash || marker.request_id != action.produced_by_request_id)
        {
            return Err(HostExecuteError::denied(
                "integration journal belongs to a different seal/request",
            ));
        }
    }
    let completing = marker
        .and_then(|marker| marker.pending_head_sha)
        .filter(|sha| commit_exists(&trunk, sha));
    if completing.is_none() {
        require_active_integrate_binding(action, ctx.documents)
            .map_err(HostExecuteError::denied)?;
    }

    super::super::adapter::verify_workspace_identity(&trunk, &dest_canon, &workspace.identity())
        .map_err(|err| HostExecuteError::failed(err.to_string(), true, None))?;
    let snapshot = capture_seal_snapshot(&dest_canon, &workspace.identity())
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
    if snapshot.tree_hash != seal_hash {
        return Err(HostExecuteError::denied(
            "live snapshot does not match the sealed tree",
        ));
    }
    let effect = if let Some(sha) = completing {
        super::super::adapter::IntegrateEffect {
            head_sha: sha,
            changed_files: snapshot.changed_files,
            diff: snapshot.diff,
            pending_head: true,
        }
    } else {
        prepare_integrate_commit(
            &trunk,
            &dest_canon,
            &seal_hash,
            &workspace.base_sha,
            snapshot,
        )
        .map_err(|err| {
            HostExecuteError::failed(
                format!("typed integrate_workspace failed: {err}"),
                false,
                None,
            )
        })?
    };

    let has_effect = super::super::adapter::verify_integrate_commit(
        &trunk,
        &effect.head_sha,
        &workspace.identity(),
        &seal_hash,
    )
    .map_err(|err| HostExecuteError::denied(err.to_string()))?;
    workspace
        .path_capability
        .validate_paths_at(&trunk)
        .map_err(|err| HostExecuteError::denied(err.to_string()))?;
    journal::advance(journal, 0, ActionJournalState::EffectObserved);
    let pending = (has_effect && effect.pending_head).then(|| effect.head_sha.clone());
    persist_integrate_journal(
        journal,
        &trunk,
        &action.workspace_id,
        pending.as_deref(),
        &seal_hash,
        &action.produced_by_request_id,
    );
    let receipt = WorkspaceReceiptDoc {
        path_capability_digest: workspace.path_capability.digest(),
        receipt_id,
        workspace_id: workspace.workspace_id.clone(),
        produced_by_request_id: action.produced_by_request_id.clone(),
        produced_by_request_doc_id: action.produced_by_request_doc_id.clone(),
        kind: RECEIPT_KIND_INTEGRATOR.to_string(),
        base_sha: workspace.base_sha.clone(),
        seal_hash: seal_hash.clone(),
        work_unit_id: Some(workspace.work_unit_id.clone()),
        caused_by_correlation: Some(workspace.caused_by_correlation.clone()),
        head_sha: Some(effect.head_sha.clone()),
        changed_files: changed_files_json(&effect.changed_files),
        diff_artifact: bound_diff_artifact(&effect.diff),
        unresolved_conflicts: None,
        integration_instructions: None,
    };
    let mut outcome = persist_integrate_docs(workspace, placement, receipt, ctx.documents)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
    outcome.pending_head_sha = pending;
    journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
    persist_integrate_journal(
        journal,
        &trunk,
        &action.workspace_id,
        outcome.pending_head_sha.as_deref(),
        &seal_hash,
        &action.produced_by_request_id,
    );
    Ok(outcome)
}

fn require_active_integrate_binding(
    action: &IntegrateWorkspaceAction,
    documents: &dyn WorkspaceDocuments,
) -> Result<(), String> {
    let bindings = documents
        .load_bindings(&action.workspace_id)
        .map_err(|err| err.to_string())?;
    let matched = bindings.iter().find(|binding| {
        binding.is_active_integrate() && binding.request_id == action.produced_by_request_id
    });
    match matched {
        Some(binding) => {
            let workspace = documents
                .load_isolated_workspace(&action.workspace_id)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| format!("IsolatedWorkspace {} not found", action.workspace_id))?;
            let workspace_hash = workspace.seal_hash.as_deref().unwrap_or_default();
            match binding
                .seal_hash
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(hash) if hash == workspace_hash => Ok(()),
                Some(hash) => Err(format!(
                    "Integrate binding seal_hash {hash} does not match workspace seal_hash {workspace_hash}"
                )),
                None => Err(format!(
                    "Integrate binding {} is missing seal_hash",
                    binding.binding_id
                )),
            }
        }
        None => Err(format!(
            "integrate_workspace requires an Active Integrate binding for request {}",
            action.produced_by_request_id
        )),
    }
}

fn persist_integrate_docs(
    workspace: IsolatedWorkspaceDoc,
    placement: WorkspacePlacementDoc,
    receipt: WorkspaceReceiptDoc,
    documents: &mut dyn WorkspaceDocuments,
) -> Result<IntegrateWorkspaceOutcome> {
    documents.write_receipt(receipt.clone())?;
    Ok(IntegrateWorkspaceOutcome {
        workspace,
        placement,
        receipt,
        pending_head_sha: None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IntegrateJournalMarker {
    journal: Vec<ActionJournalEntry>,
    pending_head_sha: Option<String>,
    seal_hash: String,
    request_id: String,
}

fn integrate_marker_path(trunk: &Path, workspace_id: &str) -> Option<PathBuf> {
    let git_dir = absolute_git_dir(trunk).ok()?;
    Some(git_dir.join(format!(
        "gents-integrate-{}.json",
        sanitize_fs(workspace_id)
    )))
}

fn load_integrate_marker(trunk: &Path, workspace_id: &str) -> Option<IntegrateJournalMarker> {
    let path = integrate_marker_path(trunk, workspace_id)?;
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn restore_integrate_journal(
    journal: &mut Vec<ActionJournalEntry>,
    trunk: &Path,
    workspace_id: &str,
) {
    if !journal.is_empty() {
        return;
    }
    if let Some(marker) = load_integrate_marker(trunk, workspace_id) {
        *journal = marker.journal;
    }
}

fn persist_integrate_journal(
    journal: &[ActionJournalEntry],
    trunk: &Path,
    workspace_id: &str,
    pending_head_sha: Option<&str>,
    seal_hash: &str,
    request_id: &str,
) {
    let Some(path) = integrate_marker_path(trunk, workspace_id) else {
        return;
    };
    let marker = IntegrateJournalMarker {
        journal: journal.to_vec(),
        pending_head_sha: pending_head_sha.map(str::to_string),
        seal_hash: seal_hash.to_string(),
        request_id: request_id.to_string(),
    };
    if let Ok(json) = serde_json::to_vec_pretty(&marker) {
        let _ = std::fs::write(path, json);
    }
}

pub(crate) fn clear_integrate_journal(trunk: &Path, workspace_id: &str) {
    if let Some(path) = integrate_marker_path(trunk, workspace_id) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn finalize_integrate_trunk(trunk: &Path, pending_head_sha: Option<&str>) -> Result<()> {
    let Some(sha) = pending_head_sha
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    advance_trunk_to_integrate_commit(trunk, sha)
}
