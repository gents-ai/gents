use std::path::Path;

use anyhow::{anyhow, bail, Result};

use super::super::action_plan::{ActionPlan, CreateWorkspaceAction, HostAction};
use super::super::adapter::{
    artifacts_complete, capture_instruction_manifest, clone_artifacts, observe_dirty_base,
    observe_effect, observed_tree_hash, provision, resolve_base_sha, write_identity,
    ObservedEffect,
};
use super::super::documents::{
    new_isolated_workspace, ProvisioningObservation, WorkspaceDocuments, WorkspacePlacementDoc,
    ADAPTER_VERSION, LIFECYCLE_CLEANED, LIFECYCLE_CLEANING, LIFECYCLE_PROVISION_FAILED,
    LIFECYCLE_READY, LIFECYCLE_SEALED,
};
use super::super::instructions::is_empty_manifest;
use super::super::journal::{self, ActionJournalEntry, ActionJournalState};
use super::{
    workspace_host_path, CreateWorkspaceOutcome, HostExecuteError, HostExecutorContext,
    LogicalWorkspaceIdentity,
};
use crate::toolset::normalize_workspace_lifecycle_state;

pub fn execute_create_workspace_plan(
    plan: &ActionPlan,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<CreateWorkspaceOutcome, HostExecuteError> {
    if !journal::action_journal_prefix_legal(journal) {
        return Err(HostExecuteError::denied("illegal action journal prefix"));
    }
    plan.validate_against(&ctx.capabilities)
        .map_err(|err| HostExecuteError::denied(err.to_string()))?;
    if plan.actions.len() != 1 {
        return Err(HostExecuteError::denied(
            "create_workspace executor only runs a single create_workspace action",
        ));
    }
    let action = match &plan.actions[0] {
        HostAction::CreateWorkspace(action) => action,
        other => {
            return Err(HostExecuteError::denied(format!(
                "create_workspace executor cannot run {}",
                other.type_name()
            )));
        }
    };
    if ctx.repository.repository_id != action.repository_id {
        return Err(HostExecuteError::denied(format!(
            "RepositoryPlacement {} does not match plan repository_id {}",
            ctx.repository.repository_id, action.repository_id
        )));
    }
    if !ctx.repository.enabled {
        return Err(HostExecuteError::denied(format!(
            "RepositoryPlacement {} is disabled",
            ctx.repository.repository_id
        )));
    }
    if ctx.repository.owner_node_did != ctx.owner_node_did {
        return Err(HostExecuteError::denied(
            "RepositoryPlacement.owner_node_did does not match the executing node DID",
        ));
    }

    if journal::current_state(journal, 0).is_none() {
        journal::advance(journal, 0, ActionJournalState::Validated);
    }

    create_workspace_action(action, journal, ctx)
}

fn create_workspace_action(
    action: &CreateWorkspaceAction,
    journal: &mut Vec<ActionJournalEntry>,
    ctx: &mut HostExecutorContext<'_>,
) -> Result<CreateWorkspaceOutcome, HostExecuteError> {
    let source = ctx.repository.host_path.clone();
    let dest = workspace_host_path(&source, &action.workspace_id, &action.branch, ctx.ceiling)
        .map_err(|err| HostExecuteError::denied(err.to_string()))?;

    let resolved_base = resolve_base_sha(&source, &action.base_sha).map_err(|err| {
        HostExecuteError::denied(format!(
            "base_sha {} is not a commit: {err}",
            action.base_sha
        ))
    })?;
    let mut identity = action.identity();
    identity.base_sha = resolved_base.clone();

    let artifacts = action.effective_clone_artifacts();
    let state = journal::current_state(journal, 0).unwrap_or(ActionJournalState::Validated);
    let existing = ctx
        .documents
        .load_isolated_workspace(&identity.workspace_id)
        .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
    if let Some(existing) = &existing {
        if existing.identity() != identity || existing.owner_node_did != ctx.owner_node_did {
            return Err(HostExecuteError::failed(
                "workspace creation identity/capability does not match the admitted workspace",
                true,
                None,
            ));
        }
    }
    let legacy = existing
        .as_ref()
        .is_some_and(|row| !row.path_capability.is_exact());
    if !identity.path_capability.is_exact() && !legacy {
        return Err(HostExecuteError::denied(
            "fresh workspace creation requires an exact path capability",
        ));
    }

    if matches!(state, ActionJournalState::ResultDocsWritten) {
        return load_written_docs(&identity.workspace_id, ctx.documents)
            .map_err(|err| HostExecuteError::failed(err.to_string(), false, None));
    }

    if matches!(state, ActionJournalState::Validated) {
        journal::advance(journal, 0, ActionJournalState::Executing);
    }

    let observed = observe_effect(
        &source,
        &dest,
        &identity,
        &resolved_base,
        &artifacts,
        legacy,
        legacy || state == ActionJournalState::Executing,
    )
    .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;

    let (observation, tree_hash, dirty_base) = match observed {
        ObservedEffect::Absent
            if matches!(
                journal::current_state(journal, 0),
                Some(ActionJournalState::Executing)
            ) =>
        {
            match provision(&source, &dest, action, &resolved_base) {
                Ok(observation) => {
                    let tree_hash = observed_tree_hash(&dest).unwrap_or_default();
                    let dirty_base =
                        observe_dirty_base(&source).unwrap_or(super::super::adapter::DirtyBase {
                            dirty: false,
                            summary: String::new(),
                        });
                    (observation, tree_hash, dirty_base)
                }
                Err(err) => {
                    let dirty_base =
                        observe_dirty_base(&source).unwrap_or(super::super::adapter::DirtyBase {
                            dirty: false,
                            summary: String::new(),
                        });
                    journal::advance(journal, 0, ActionJournalState::EffectObserved);
                    let outcome = persist_docs(
                        action,
                        &identity,
                        ctx,
                        &dest,
                        ProvisioningObservation {
                            path_exists: dest.exists(),
                            ..ProvisioningObservation::default()
                        },
                        String::new(),
                        &dirty_base,
                        LIFECYCLE_PROVISION_FAILED,
                    )
                    .map_err(|persist_err| {
                        HostExecuteError::failed(
                            format!("{err}; also failed writing result docs: {persist_err}"),
                            false,
                            None,
                        )
                    })?;
                    journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
                    return Err(HostExecuteError::failed(
                        err.to_string(),
                        false,
                        Some(outcome),
                    ));
                }
            }
        }
        ObservedEffect::Absent => {
            return Err(HostExecuteError::failed(
                "journal is past Executing but destination is absent",
                false,
                None,
            ));
        }
        ObservedEffect::Match {
            mut observation,
            tree_hash,
            dirty_base,
        } => {
            write_identity(&dest, &identity)
                .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
            if action.adapter.clones_artifacts() {
                if let Err(err) = clone_artifacts(&source, &dest, &artifacts) {
                    journal::advance(journal, 0, ActionJournalState::EffectObserved);
                    let outcome = persist_docs(
                        action,
                        &identity,
                        ctx,
                        &dest,
                        observation,
                        tree_hash,
                        &dirty_base,
                        LIFECYCLE_PROVISION_FAILED,
                    )
                    .map_err(|persist_err| {
                        HostExecuteError::failed(
                            format!("{err}; also failed writing result docs: {persist_err}"),
                            false,
                            None,
                        )
                    })?;
                    journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
                    return Err(HostExecuteError::failed(
                        err.to_string(),
                        false,
                        Some(outcome),
                    ));
                }
                observation.artifacts_cloned = artifacts_complete(&source, &dest, &artifacts);
            }
            (observation, tree_hash, dirty_base)
        }
        ObservedEffect::Mismatch {
            reason,
            observation,
            dirty_base,
        } => {
            journal::advance(journal, 0, ActionJournalState::EffectObserved);
            let outcome = persist_docs(
                action,
                &identity,
                ctx,
                &dest,
                observation,
                String::new(),
                &dirty_base,
                LIFECYCLE_PROVISION_FAILED,
            )
            .map_err(|err| HostExecuteError::failed(err.to_string(), true, None))?;
            journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
            return Err(HostExecuteError::failed(reason, true, Some(outcome)));
        }
    };

    journal::advance(journal, 0, ActionJournalState::EffectObserved);
    let outcome = persist_docs(
        action,
        &identity,
        ctx,
        &dest,
        observation,
        tree_hash,
        &dirty_base,
        LIFECYCLE_READY,
    )
    .map_err(|err| HostExecuteError::failed(err.to_string(), false, None))?;
    journal::advance(journal, 0, ActionJournalState::ResultDocsWritten);
    Ok(outcome)
}

#[expect(
    clippy::too_many_arguments,
    reason = "provisioning results recorded into one document set"
)]
fn persist_docs(
    action: &CreateWorkspaceAction,
    identity: &LogicalWorkspaceIdentity,
    ctx: &mut HostExecutorContext<'_>,
    dest: &Path,
    observation: ProvisioningObservation,
    tree_hash: String,
    dirty_base: &super::super::adapter::DirtyBase,
    lifecycle_state: &str,
) -> Result<CreateWorkspaceOutcome> {
    if let Some(existing) = ctx
        .documents
        .load_isolated_workspace(&identity.workspace_id)?
    {
        if existing.identity() != *identity {
            bail!(
                "IsolatedWorkspace {} already exists with a different identity; leaving it in place",
                identity.workspace_id
            );
        }
        let existing_state = normalize_workspace_lifecycle_state(&existing.lifecycle_state);
        if existing_state == Some(LIFECYCLE_READY) && lifecycle_state != LIFECYCLE_READY {
            bail!(
                "refusing to overwrite Ready IsolatedWorkspace {}",
                identity.workspace_id
            );
        }
        if existing_state == Some(LIFECYCLE_PROVISION_FAILED)
            && lifecycle_state != LIFECYCLE_PROVISION_FAILED
        {
            bail!(
                "IsolatedWorkspace {} is provisionFailed; refusing {lifecycle_state} without cleanup",
                identity.workspace_id
            );
        }
        if matches!(
            existing_state,
            Some(LIFECYCLE_SEALED) | Some(LIFECYCLE_CLEANING) | Some(LIFECYCLE_CLEANED)
        ) {
            bail!(
                "IsolatedWorkspace {} is {}; refusing {lifecycle_state} without cleanup",
                identity.workspace_id,
                existing.lifecycle_state
            );
        }
    }

    let instruction_manifest = instruction_manifest_for_persist(
        ctx.documents,
        &identity.workspace_id,
        &ctx.repository.host_path,
        &identity.base_sha,
    )?;
    let workspace = new_isolated_workspace(
        identity,
        action.creation_policy,
        action.adapter,
        &ctx.owner_node_did,
        &ctx.writer_principal,
        &ctx.integrator_principal,
        &ctx.caused_by_invocation_id,
        &ctx.caused_by_correlation,
        lifecycle_state,
        instruction_manifest,
        None,
    );
    let host_path = dest
        .to_str()
        .ok_or_else(|| anyhow!("workspace host_path is not valid UTF-8"))?
        .to_string();
    let placement = WorkspacePlacementDoc {
        workspace_id: identity.workspace_id.clone(),
        owner_node_did: ctx.owner_node_did.clone(),
        host_path,
        repository_placement_id: ctx.repository.repository_id.clone(),
        adapter: action.adapter.as_str().to_string(),
        adapter_version: ADAPTER_VERSION.to_string(),
        dirty_base: dirty_base.dirty,
        dirty_base_summary: dirty_base.summary.clone(),
        provisioning_state: observation.to_json_string(),
        observed_tree_hash: tree_hash,
    };
    ctx.documents.write_isolated_workspace(workspace.clone())?;
    ctx.documents.write_placement(placement.clone())?;
    Ok(CreateWorkspaceOutcome {
        workspace,
        placement,
    })
}

fn instruction_manifest_for_persist(
    documents: &dyn WorkspaceDocuments,
    workspace_id: &str,
    source: &Path,
    base_sha: &str,
) -> Result<String> {
    if let Some(existing) = documents.load_isolated_workspace(workspace_id)? {
        if !is_empty_manifest(&existing.instruction_manifest) {
            return Ok(existing.instruction_manifest);
        }
    }
    capture_instruction_manifest(source, base_sha)
}

fn load_written_docs(
    workspace_id: &str,
    documents: &dyn WorkspaceDocuments,
) -> Result<CreateWorkspaceOutcome> {
    let workspace = documents
        .load_isolated_workspace(workspace_id)?
        .ok_or_else(|| {
            anyhow!("journal ResultDocsWritten but IsolatedWorkspace {workspace_id} missing")
        })?;
    let placement = documents.load_placement(workspace_id)?.ok_or_else(|| {
        anyhow!("journal ResultDocsWritten but WorkspacePlacement {workspace_id} missing")
    })?;
    Ok(CreateWorkspaceOutcome {
        workspace,
        placement,
    })
}
