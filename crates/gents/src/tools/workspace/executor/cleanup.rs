use std::path::PathBuf;

use anyhow::{anyhow, Result};

use super::super::action_plan::{ActionPlan, CleanupWorkspaceAction, HostAction};
use super::super::adapter::cleanup_workspace_tree;
use super::super::binding::release_binding;
use super::super::documents::{
    IsolatedWorkspaceDoc, WorkspaceDocuments, WorkspacePlacementDoc, LIFECYCLE_CLEANED,
    LIFECYCLE_CLEANING, LIFECYCLE_PROVISION_FAILED, LIFECYCLE_READY, LIFECYCLE_SEALED,
};
use super::super::journal::{self, ActionJournalEntry, ActionJournalState};
use super::{workspace_host_path, HostExecuteError, HostExecutorContext};
use crate::tools::native::normalize_workspace_lifecycle_state;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupWorkspaceOutcome {
    pub workspace: IsolatedWorkspaceDoc,
    pub placement: WorkspacePlacementDoc,
}

pub fn execute_cleanup_workspace_plan(
    plan: &ActionPlan,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<CleanupWorkspaceOutcome, HostExecuteError> {
    if !journal::action_journal_prefix_legal(journal) {
        return Err(HostExecuteError::denied("illegal action journal prefix"));
    }
    plan.validate_against(&ctx.capabilities)
        .map_err(|err| HostExecuteError::denied(err.to_string()))?;
    if plan.actions.len() != 1 {
        return Err(HostExecuteError::denied(
            "cleanup_workspace executor only runs a single cleanup_workspace action",
        ));
    }
    let action = match &plan.actions[0] {
        HostAction::CleanupWorkspace(action) => action,
        other => {
            return Err(HostExecuteError::denied(format!(
                "cleanup_workspace executor cannot run {}",
                other.type_name()
            )));
        }
    };
    if journal::current_state(journal, 0).is_none() {
        journal::advance(journal, 0, ActionJournalState::Validated);
    }
    cleanup_workspace_action(action, journal, ctx)
}

fn cleanup_workspace_action(
    action: &CleanupWorkspaceAction,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<CleanupWorkspaceOutcome, HostExecuteError> {
    if matches!(
        journal::current_state(journal, 0),
        Some(ActionJournalState::ResultDocsWritten)
    ) {
        return load_written_cleanup(&action.workspace_id, ctx.documents)
            .map_err(|err| HostExecuteError::failed(err.to_string(), false, None));
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
    if lifecycle == Some(LIFECYCLE_CLEANED) {
        journal::advance(journal, 0, ActionJournalState::EffectObserved);
        let outcome = persist_cleanup_docs(workspace, placement, ctx.documents)
            .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
        journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
        return Ok(outcome);
    }

    let dest = PathBuf::from(&placement.host_path);
    let source = ctx.repository.host_path.clone();
    match lifecycle {
        Some(LIFECYCLE_SEALED) | Some(LIFECYCLE_CLEANING) | Some(LIFECYCLE_PROVISION_FAILED) => {}
        Some(LIFECYCLE_READY) => {
            return Err(HostExecuteError::denied(format!(
                "workspace {} is ready; cleanup would leave a bindable Ready workspace without a placement",
                action.workspace_id
            )));
        }
        other => {
            return Err(HostExecuteError::denied(format!(
                "workspace {} in state {} cannot be cleaned",
                action.workspace_id,
                other.unwrap_or(workspace.lifecycle_state.as_str())
            )));
        }
    }

    let active = ctx
        .documents
        .load_bindings(&action.workspace_id)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?
        .into_iter()
        .filter(|binding| binding.is_active())
        .collect::<Vec<_>>();
    if !active.is_empty() {
        return Err(HostExecuteError::denied(format!(
            "workspace {} has {} Active binding(s); cleanup requires them Released first",
            action.workspace_id,
            active.len()
        )));
    }

    let expected = workspace_host_path(
        &source,
        &workspace.workspace_id,
        &workspace.branch,
        ctx.ceiling,
    )
    .map_err(|err| HostExecuteError::denied(err.to_string()))?;

    if matches!(lifecycle, Some(LIFECYCLE_SEALED) | Some(LIFECYCLE_CLEANING)) {
        workspace.lifecycle_state = LIFECYCLE_CLEANING.to_string();
        ctx.documents
            .write_isolated_workspace(workspace.clone())
            .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
    }

    cleanup_workspace_tree(&source, &dest, &expected, ctx.ceiling)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;

    journal::advance(journal, 0, ActionJournalState::EffectObserved);
    if matches!(lifecycle, Some(LIFECYCLE_SEALED) | Some(LIFECYCLE_CLEANING)) {
        workspace.lifecycle_state = LIFECYCLE_CLEANED.to_string();
    }
    let outcome = persist_cleanup_docs(workspace, placement, ctx.documents)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
    journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
    Ok(outcome)
}

fn persist_cleanup_docs(
    workspace: IsolatedWorkspaceDoc,
    placement: WorkspacePlacementDoc,
    documents: &mut dyn WorkspaceDocuments,
) -> Result<CleanupWorkspaceOutcome> {
    let bindings = documents.load_bindings(&workspace.workspace_id)?;
    for binding in bindings {
        if binding.is_active() {
            documents.write_binding(release_binding(binding))?;
        }
    }
    documents.write_isolated_workspace(workspace.clone())?;
    Ok(CleanupWorkspaceOutcome {
        workspace,
        placement,
    })
}

fn load_written_cleanup(
    workspace_id: &str,
    documents: &dyn WorkspaceDocuments,
) -> Result<CleanupWorkspaceOutcome> {
    let workspace = documents
        .load_isolated_workspace(workspace_id)?
        .ok_or_else(|| {
            anyhow!("journal ResultDocsWritten but IsolatedWorkspace {workspace_id} missing")
        })?;
    let placement = documents.load_placement(workspace_id)?.ok_or_else(|| {
        anyhow!("journal ResultDocsWritten but WorkspacePlacement {workspace_id} missing")
    })?;
    Ok(CleanupWorkspaceOutcome {
        workspace,
        placement,
    })
}
