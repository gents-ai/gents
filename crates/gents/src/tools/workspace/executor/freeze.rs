use std::path::PathBuf;

use super::super::action_plan::{ActionPlan, FreezeWorkspaceBaseAction, HostAction};
use super::super::adapter::{capture_seal_snapshot, observe_dirty_base, resolve_base_sha};
use super::super::documents::{LIFECYCLE_READY, LIFECYCLE_SEALED};
use super::super::journal::{self, ActionJournalEntry, ActionJournalState};
use super::seal::{apply_seal_snapshot, persist_sealed_workspace_documents};
use super::{workspace_host_path, CreateWorkspaceOutcome, HostExecuteError, HostExecutorContext};
use crate::tools::native::normalize_workspace_lifecycle_state;

pub fn execute_freeze_workspace_base_plan(
    plan: &ActionPlan,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<CreateWorkspaceOutcome, HostExecuteError> {
    if !journal::action_journal_prefix_legal(journal) {
        return Err(HostExecuteError::denied("illegal action journal prefix"));
    }
    plan.validate_against(&ctx.capabilities)
        .map_err(|error| HostExecuteError::denied(error.to_string()))?;
    let [HostAction::FreezeWorkspaceBase(action)] = plan.actions.as_slice() else {
        return Err(HostExecuteError::denied(
            "base freeze requires one freeze_workspace_base action",
        ));
    };
    if journal::current_state(journal, 0).is_none() {
        journal::advance(journal, 0, ActionJournalState::Validated);
    }
    freeze_workspace_base_action(action, journal, ctx)
}

fn freeze_workspace_base_action(
    action: &FreezeWorkspaceBaseAction,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<CreateWorkspaceOutcome, HostExecuteError> {
    let failed = |error: anyhow::Error| HostExecuteError::failed(error.to_string(), false, None);
    let denied = |error: anyhow::Error| HostExecuteError::denied(error.to_string());
    let mut workspace = ctx
        .documents
        .load_isolated_workspace(&action.workspace_id)
        .map_err(failed)?
        .ok_or_else(|| HostExecuteError::denied("base freeze workspace is missing"))?;
    let mut placement = ctx
        .documents
        .load_placement(&action.workspace_id)
        .map_err(failed)?
        .ok_or_else(|| HostExecuteError::denied("base freeze placement is missing"))?;
    if !(workspace.owner_node_did == ctx.owner_node_did
        && ctx.repository.enabled
        && ctx.repository.owner_node_did == ctx.owner_node_did
        && workspace.repository_id == ctx.repository.repository_id
        && placement.workspace_id == workspace.workspace_id
        && placement.owner_node_did == ctx.owner_node_did
        && placement.repository_placement_id == ctx.repository.repository_id
        && placement.adapter == workspace.adapter)
    {
        return Err(HostExecuteError::denied(
            "base freeze owner/repository/placement mismatch",
        ));
    }
    if !matches!(&workspace.path_capability, super::super::WorkspacePathCapability::ExactPaths { paths } if paths.is_empty())
    {
        return Err(HostExecuteError::denied(
            "base freeze requires an empty exact-path capability",
        ));
    }
    if workspace.base_sha != action.base_sha {
        return Err(HostExecuteError::denied(
            "base freeze expected commit differs from workspace identity",
        ));
    }
    let source = &ctx.repository.host_path;
    let canonical_base = resolve_base_sha(source, &action.base_sha).map_err(denied)?;
    if canonical_base != action.base_sha {
        return Err(HostExecuteError::denied(
            "base freeze requires the immutable resolved base commit",
        ));
    }
    let base_tree =
        super::super::adapter::base_tree_hash(source, &canonical_base).map_err(failed)?;
    let expected = workspace_host_path(
        source,
        &workspace.workspace_id,
        &workspace.branch,
        ctx.ceiling,
    )
    .map_err(denied)?;
    let dest = PathBuf::from(&placement.host_path);
    let expected_canon = expected.canonicalize().unwrap_or_else(|_| expected.clone());
    let dest_canon = dest.canonicalize().unwrap_or_else(|_| dest.clone());
    if dest_canon != expected_canon {
        return Err(HostExecuteError::denied(
            "base freeze placement differs from host-chosen workspace path",
        ));
    }
    let state = normalize_workspace_lifecycle_state(&workspace.lifecycle_state);
    if state == Some(LIFECYCLE_SEALED) {
        if workspace.seal_hash.as_deref() != Some(base_tree.as_str()) {
            return Err(HostExecuteError::denied(
                "base freeze replay differs from persisted seal",
            ));
        }
        if placement.observed_tree_hash != base_tree {
            placement.observed_tree_hash = base_tree.clone();
            journal::advance(journal, 0, ActionJournalState::EffectObserved);
            persist_sealed_workspace_documents(&workspace, &placement, ctx.documents)
                .map_err(failed)?;
        }
        journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
        return Ok(CreateWorkspaceOutcome {
            workspace,
            placement,
        });
    }
    if state != Some(LIFECYCLE_READY) {
        return Err(HostExecuteError::denied(
            "only a Ready base workspace can be frozen",
        ));
    }
    if journal::current_state(journal, 0) == Some(ActionJournalState::ResultDocsWritten) {
        return Err(HostExecuteError::denied(
            "base freeze journal result has no persisted seal",
        ));
    }
    if ctx
        .documents
        .load_bindings(&action.workspace_id)
        .map_err(failed)?
        .iter()
        .any(|binding| binding.is_active_read_write())
    {
        return Err(HostExecuteError::denied(
            "base freeze requires no active writer binding",
        ));
    }
    super::super::adapter::verify_workspace_identity(source, &dest, &workspace.identity())
        .map_err(denied)?;
    if !(resolve_base_sha(&dest, "HEAD").map_err(denied)? == canonical_base) {
        return Err(HostExecuteError::denied(
            "base freeze checkout HEAD differs from immutable base commit",
        ));
    }
    if observe_dirty_base(&dest).map_err(failed)?.dirty {
        return Err(HostExecuteError::denied(
            "base freeze requires a clean index and checkout",
        ));
    }
    let snapshot = capture_seal_snapshot(&dest, &workspace.identity()).map_err(denied)?;
    if !(snapshot.tree_hash == base_tree
        && snapshot.diff.is_empty()
        && snapshot.changed_files.is_empty())
    {
        return Err(HostExecuteError::denied(
            "base freeze requires an unchanged base tree",
        ));
    }
    journal::advance(journal, 0, ActionJournalState::Executing);
    apply_seal_snapshot(&mut workspace, &mut placement, &dest, &base_tree).map_err(failed)?;
    journal::advance(journal, 0, ActionJournalState::EffectObserved);
    persist_sealed_workspace_documents(&workspace, &placement, ctx.documents).map_err(failed)?;
    journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
    Ok(CreateWorkspaceOutcome {
        workspace,
        placement,
    })
}
