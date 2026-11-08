use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::{
    emit_create_workspace_plan, emit_freeze_workspace_base_plan, execute_create_workspace_plan,
    execute_freeze_workspace_base_plan, flush_workspace_docs, ActionJournalEntry,
    CreateWorkspaceAction, CreateWorkspaceOutcome, CreationPolicy, FreezeWorkspaceBaseAction,
    HostExecutorContext, RepositoryPlacementRef, WorkspaceAdapterKind, WorkspaceDocuments,
    CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE, CAP_SEAL_WORKSPACE,
};
use crate::documents::apply_desired_state_plan;
use crate::store::config_client::{
    read_desired_state_record_in_txn, ConfigAccess, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use crate::Collection;

/// The user configures each Agent explicitly; physical Agent documents are
/// resolved under that user's native access before any host effect.
#[derive(Debug, Clone)]
pub struct WorkspaceProvisioningSelection {
    pub workspace_id: String,
    pub node_did: String,
    pub creator_agent_did: String,
    pub writer_agent_did: String,
    pub integrator_agent_did: String,
    pub reader_agent_dids: Vec<String>,
}

fn repository_id(path: &Path, node_did: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(node_did.as_bytes());
    hasher.update([0]);
    hasher.update(path.as_os_str().as_encoded_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("local-repository-{}", &digest[..24])
}

async fn publish_repository_and_resolve_agents(
    operator: &ConfigAccess,
    repository: &RepositoryPlacementRef,
    selected: &WorkspaceProvisioningSelection,
) -> Result<(String, String, Vec<super::WorkspaceReaderBinding>)> {
    let host_path = repository
        .host_path
        .to_str()
        .context("repository path is not UTF-8")?;
    let document = serde_json::json!({"repository_id":repository.repository_id,
        "node_did":repository.owner_node_did, "host_path":host_path, "enabled":true});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::RepositoryPlacement,
        add: document.clone(),
        update: document,
    }])?;
    operator
        .transact_write("workspace.quickstart.repository", |txn| {
            let plan = &plan;
            Box::pin(async move {
                anyhow::ensure!(
                    txn.actor_did().is_some(),
                    "repository publication requires the authenticated user"
                );
                let mut targets = Vec::new();
                let dids = [
                    &selected.creator_agent_did,
                    &selected.writer_agent_did,
                    &selected.integrator_agent_did,
                ]
                .into_iter()
                .chain(selected.reader_agent_dids.iter())
                .collect::<Vec<_>>();
                for &did in &dids {
                    anyhow::ensure!(
                        !did.is_empty() && did != &selected.node_did,
                        "workspace role must select an Agent DID"
                    );
                    identity::Did::new(did)?;
                    let (doc, row) = read_desired_state_record_in_txn(
                        txn,
                        Collection::Agent,
                        &selected.node_did,
                        did,
                    )
                    .await?
                    .context("selected workspace Agent unavailable")?;
                    anyhow::ensure!(
                        row["agent_did"] == *did
                            && row["node_did"] == selected.node_did
                            && row["enabled"] != false,
                        "selected workspace Agent changed or disabled"
                    );
                    targets.push(doc);
                }
                let previous = read_desired_state_record_in_txn(
                    txn,
                    Collection::RepositoryPlacement,
                    &selected.node_did,
                    &repository.repository_id,
                )
                .await?;
                apply_desired_state_plan(txn, plan).await?;
                let (doc, _) = read_desired_state_record_in_txn(
                    txn,
                    Collection::RepositoryPlacement,
                    &selected.node_did,
                    &repository.repository_id,
                )
                .await?
                .context("published repository unavailable")?;
                if previous.is_none() {
                    for target in targets.iter().skip(1).collect::<BTreeSet<_>>() {
                        if target != &targets[0] {
                            txn.add_entity_set_relationship(
                                "Agent",
                                target,
                                "workspace_selected_by",
                                "agents",
                                &targets[0],
                                "executor",
                            )
                            .await?;
                        }
                    }
                    for did in dids.iter().copied().collect::<BTreeSet<_>>() {
                        txn.add_actor_relationship(
                            "RepositoryPlacement",
                            &doc,
                            "reader",
                            &identity::Did::new(did)?,
                        )
                        .await?;
                    }
                } else {
                    anyhow::ensure!(
                        previous
                            .as_ref()
                            .is_some_and(|(previous, _)| previous == &doc),
                        "repository physical identity changed"
                    );
                }
                let readers = selected
                    .reader_agent_dids
                    .iter()
                    .zip(targets.iter().skip(3))
                    .map(|(did, doc)| super::WorkspaceReaderBinding {
                        agent_did: did.clone(),
                        agent_doc_id: Some(doc.clone()),
                    })
                    .collect();
                Ok::<_, anyhow::Error>((targets[1].clone(), targets[2].clone(), readers))
            })
        })
        .await
}

/// Uses user-held configuration access and a separately held managed Agent.
/// Existing repository grants are never restored by a repeated quickstart.
pub async fn provision_read_only_workspace(
    operator: &ConfigAccess,
    agent: &ConfigAccess,
    repository_path: &Path,
    head_sha: &str,
    selected: &WorkspaceProvisioningSelection,
) -> Result<CreateWorkspaceOutcome> {
    anyhow::ensure!(
        operator.actor_did().is_some(),
        "quickstart requires an authenticated user"
    );
    anyhow::ensure!(
        agent.actor_did().as_deref() == Some(selected.creator_agent_did.as_str()),
        "quickstart requires the selected held creator Agent"
    );
    let workspace_id = selected.workspace_id.trim().to_owned();
    anyhow::ensure!(
        !workspace_id.is_empty(),
        "quickstart requires a stable workspace ID"
    );
    anyhow::ensure!(
        uuid::Uuid::parse_str(&workspace_id)?.to_string() == workspace_id,
        "quickstart workspace ID must be a canonical UUID"
    );
    let path = repository_path.to_owned();
    let repository_path = tokio::task::spawn_blocking(move || std::fs::canonicalize(&path))
        .await
        .context("joining repository path validation")??;
    let repository_id = repository_id(&repository_path, &selected.node_did);
    let repository = RepositoryPlacementRef {
        repository_id: repository_id.clone(),
        owner_node_did: selected.node_did.clone(),
        host_path: repository_path.clone(),
        enabled: true,
    };
    let (writer_doc, integrator_doc, readers) =
        publish_repository_and_resolve_agents(operator, &repository, selected).await?;
    let visible =
        super::runtime::load_repository_with_access(agent, &repository_id, &selected.node_did)
            .await?;
    anyhow::ensure!(
        visible == repository,
        "selected Agent cannot use the configured repository placement"
    );

    let action = CreateWorkspaceAction {
        writer_agent_did: Some(selected.writer_agent_did.clone()),
        writer_agent_doc_id: Some(writer_doc),
        integrator_agent_did: Some(selected.integrator_agent_did.clone()),
        integrator_agent_doc_id: Some(integrator_doc),
        readers,
        path_capability: super::WorkspacePathCapability::exact_paths(Vec::new())?,
        workspace_id: workspace_id.clone(),
        work_unit_id: workspace_id.clone(),
        repository_id,
        base_sha: head_sha.to_owned(),
        branch: format!("gents-review-{workspace_id}"),
        creation_policy: CreationPolicy::GitWorktreeDiff,
        adapter: WorkspaceAdapterKind::GitWorktree,
        clone_artifacts: None,
    };
    let invocation = format!("graph-workspace-{workspace_id}");
    let reservation = {
        let action = action.clone();
        let repository = repository.clone();
        let selection = selected.clone();
        let invocation = invocation.clone();
        let correlation = workspace_id.clone();
        tokio::task::spawn_blocking(move || {
            let mut reservation = super::prepare_workspace_reservation(
                &action,
                &repository,
                Some(&repository.host_path),
                &selection.node_did,
                &selection.writer_agent_did,
                &selection.integrator_agent_did,
                &invocation,
                &correlation,
            )?;
            reservation.seal_hash = Some(super::adapter::base_tree_hash(
                &repository.host_path,
                &reservation.base_sha,
            )?);
            Ok::<_, anyhow::Error>(reservation)
        })
        .await??
    };
    let mut documents =
        super::runtime::load_docs_with_access(agent, &workspace_id, &selected.node_did).await?;
    if let Some(stored) = documents.load_isolated_workspace(&workspace_id)? {
        let mut expected = reservation.clone();
        expected.lifecycle_state = stored.lifecycle_state.clone();
        anyhow::ensure!(
            stored == expected,
            "retained quickstart workspace differs from the selected inputs"
        );
        if stored.lifecycle_state == "sealed" {
            let placement = documents
                .load_placement(&workspace_id)?
                .context("sealed quickstart placement is unavailable")?;
            return Ok(CreateWorkspaceOutcome {
                workspace: stored,
                placement,
            });
        }
        anyhow::ensure!(
            stored.lifecycle_state == "provisioning",
            "quickstart workspace requires explicit recovery from state {}",
            stored.lifecycle_state
        );
    } else {
        documents.write_isolated_workspace(reservation)?;
        flush_workspace_docs(agent, &documents, &selected.creator_agent_did, &action).await?;
    }
    let capabilities = [
        CAP_CREATE_WORKSPACE,
        CAP_OBSERVE_DIRTY_BASE,
        CAP_SEAL_WORKSPACE,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    let (documents, created) = {
        let action = action.clone();
        let repository = repository.clone();
        let selection = selected.clone();
        let invocation = invocation.clone();
        let correlation = workspace_id.clone();
        let caps = capabilities.clone();
        tokio::task::spawn_blocking(move || {
            let mut documents = documents;
            let result = {
                let mut context = HostExecutorContext {
                    owner_node_did: selection.node_did,
                    ceiling: Some(&repository.host_path),
                    repository: repository.clone(),
                    capabilities: caps,
                    writer_principal: selection.writer_agent_did,
                    integrator_principal: selection.integrator_agent_did,
                    caused_by_invocation_id: invocation,
                    caused_by_correlation: correlation,
                    documents: &mut documents,
                };
                execute_create_workspace_plan(
                    &emit_create_workspace_plan(action),
                    &mut Vec::<ActionJournalEntry>::new(),
                    &mut context,
                )
            };
            (documents, result)
        })
        .await?
    };
    let created = match created {
        Ok(created) => created,
        Err(error) => {
            flush_workspace_docs(agent, &documents, &selected.creator_agent_did, &action).await?;
            return Err(anyhow::anyhow!(error.to_string())).context(format!(
                "workspace {workspace_id} provisioning is incomplete"
            ));
        }
    };
    let (documents, frozen) = {
        let repository = repository.clone();
        let selection = selected.clone();
        let invocation = invocation.clone();
        let correlation = workspace_id.clone();
        tokio::task::spawn_blocking(move || {
            let mut documents = documents;
            let result = {
                let mut context = HostExecutorContext {
                    owner_node_did: selection.node_did,
                    ceiling: Some(&repository.host_path),
                    repository: repository.clone(),
                    capabilities,
                    writer_principal: selection.writer_agent_did,
                    integrator_principal: selection.integrator_agent_did,
                    caused_by_invocation_id: invocation,
                    caused_by_correlation: correlation,
                    documents: &mut documents,
                };
                execute_freeze_workspace_base_plan(
                    &emit_freeze_workspace_base_plan(FreezeWorkspaceBaseAction {
                        workspace_id: created.workspace.workspace_id,
                        base_sha: created.workspace.base_sha,
                    }),
                    &mut Vec::new(),
                    &mut context,
                )
            };
            (documents, result)
        })
        .await?
    };
    let frozen = frozen.map_err(|error| anyhow::anyhow!(error.to_string()))?;
    super::publication::flush_frozen_workspace(
        agent,
        &documents,
        &frozen.workspace.workspace_id,
        &selected.node_did,
    )
    .await?;
    Ok(frozen)
}
