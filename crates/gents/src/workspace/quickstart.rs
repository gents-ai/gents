//! Operator-initiated read-only workspace placement for graph entrypoints.
//!
//! This composes the existing repository placement, ActionPlan executor, and
//! workspace document mutations. It does not create a second workspace model.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::{
    emit_create_workspace_plan, execute_create_workspace_plan, isolated_workspace_upsert_mutation,
    repository_placement_upsert_mutation, workspace_placement_upsert_mutation, ActionJournalEntry,
    CreateWorkspaceAction, CreateWorkspaceOutcome, CreationPolicy, HostExecutorContext,
    MemoryWorkspaceDocuments, RepositoryPlacementRef, WorkspaceAdapterKind, CAP_CREATE_WORKSPACE,
    CAP_OBSERVE_DIRTY_BASE,
};
use crate::config_client::ConfigAccess;

fn repository_id(path: &Path, deployment_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(deployment_id.as_bytes());
    hasher.update([0]);
    hasher.update(path.as_os_str().as_encoded_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("local-repository-{}", &digest[..24])
}

async fn require_local_deployment(access: &ConfigAccess, expected: &str) -> Result<()> {
    let response = access
        .execute("{ HostDeployment(limit: 2) { deployment_id } }")
        .await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("HostDeployment"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if rows.len() != 1 {
        anyhow::bail!(
            "local repository placement requires exactly one unambiguous HostDeployment on the connected server"
        );
    }
    let local = rows[0]
        .get("deployment_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .context("connected server HostDeployment is missing deployment_id")?;
    if local != expected {
        anyhow::bail!(
            "package deployment {expected:?} is not the connected server's local deployment {local:?}; refusing to persist a client-local repository path"
        );
    }
    Ok(())
}

async fn upsert_repository_placement(
    access: &ConfigAccess,
    repository_id: &str,
    deployment_id: &str,
    path: &Path,
) -> Result<()> {
    let placement = RepositoryPlacementRef {
        repository_id: repository_id.to_owned(),
        deployment_id: deployment_id.to_owned(),
        host_path: path.to_owned(),
        enabled: true,
    };
    let mutation =
        repository_placement_upsert_mutation(&placement, &chrono::Utc::now().to_rfc3339())?;
    access.execute_committed(&mutation).await?;
    Ok(())
}

async fn flush_workspace_documents(
    access: &ConfigAccess,
    documents: &MemoryWorkspaceDocuments,
) -> Result<()> {
    if documents.workspaces.is_empty() && documents.placements.is_empty() {
        return Ok(());
    }
    let txn = access.begin_apply_txn().await?;
    let result = async {
        for workspace in documents.workspaces.values() {
            txn.execute(&isolated_workspace_upsert_mutation(workspace))
                .await?;
        }
        let now = chrono::Utc::now().to_rfc3339();
        for placement in documents.placements.values() {
            txn.execute(&workspace_placement_upsert_mutation(placement, &now))
                .await?;
        }
        Result::<()>::Ok(())
    }
    .await;
    match result {
        Ok(()) => txn
            .commit()
            .await
            .context("commit graph workspace placement"),
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

/// Create a Git worktree at an immutable head commit and persist its ordinary
/// workspace records. The source checkout is only a repository placement; its
/// path never enters GraphRevision or GraphRun.
pub async fn provision_read_only_workspace(
    access: &ConfigAccess,
    repository_path: &Path,
    head_sha: &str,
    deployment_id: &str,
    principal_did: &str,
) -> Result<CreateWorkspaceOutcome> {
    require_local_deployment(access, deployment_id).await?;
    let repository_path = std::fs::canonicalize(repository_path).with_context(|| {
        format!(
            "canonicalizing repository placement {}",
            repository_path.display()
        )
    })?;
    let repository_id = repository_id(&repository_path, deployment_id);
    upsert_repository_placement(access, &repository_id, deployment_id, &repository_path).await?;

    let workspace_id = uuid::Uuid::new_v4().to_string();
    let branch = format!("gents-review-{}", &workspace_id[..12]);
    let plan = emit_create_workspace_plan(CreateWorkspaceAction {
        workspace_id: workspace_id.clone(),
        work_unit_id: workspace_id.clone(),
        repository_id: repository_id.clone(),
        base_sha: head_sha.to_owned(),
        branch,
        creation_policy: CreationPolicy::GitWorktreeDiff,
        adapter: WorkspaceAdapterKind::GitWorktree,
        clone_artifacts: None,
    });
    let mut documents = MemoryWorkspaceDocuments::default();
    let mut journal = Vec::<ActionJournalEntry>::new();
    let capabilities = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let outcome = {
        let mut context = HostExecutorContext {
            deployment_id: deployment_id.to_owned(),
            repository: RepositoryPlacementRef {
                repository_id,
                deployment_id: deployment_id.to_owned(),
                host_path: repository_path.clone(),
                enabled: true,
            },
            ceiling: Some(&repository_path),
            capabilities,
            writer_principal: principal_did.to_owned(),
            integrator_principal: principal_did.to_owned(),
            caused_by_invocation_id: format!("graph-workspace-{workspace_id}"),
            caused_by_correlation: workspace_id,
            documents: &mut documents,
        };
        execute_create_workspace_plan(&plan, &mut journal, &mut context)
    };
    flush_workspace_documents(access, &documents).await?;
    outcome.map_err(|error| anyhow::anyhow!(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use defra_node::EmbeddedNode;

    use super::*;

    #[tokio::test]
    async fn local_repository_paths_require_the_connected_deployment() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let access = ConfigAccess::Local(node.clone());
        let response = node
            .execute(
                r#"mutation { create_HostDeployment(input: {
                    deployment_id: "local", display_name: "Local"
                }) { _docID } }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        require_local_deployment(&access, "local").await.unwrap();
        let mismatch = require_local_deployment(&access, "remote")
            .await
            .unwrap_err();
        assert!(mismatch.to_string().contains("refusing to persist"));

        let response = node
            .execute(
                r#"mutation { create_HostDeployment(input: {
                    deployment_id: "replica", display_name: "Replica"
                }) { _docID } }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let ambiguous = require_local_deployment(&access, "local")
            .await
            .unwrap_err();
        assert!(ambiguous.to_string().contains("exactly one unambiguous"));

        drop(access);
        let node = Arc::try_unwrap(node).unwrap_or_else(|_| panic!("test retained node clone"));
        node.shutdown().await;
    }
}
