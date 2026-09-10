//! Resolve `spawn_subagent` workspace inherit / bind-id / provision.

use std::collections::BTreeSet;
use std::path::Path;

use defra_node::EmbeddedNode;

use crate::background_tools::SpawnWorkspaceArg;
use crate::callback::{
    flush_workspace_docs, load_isolated_workspace, load_repository_placement,
    load_workspace_placement,
};
use crate::lifecycle::WorkspaceLineage;
use crate::tool_call_lifecycle::FailureClass;
use crate::toolset::{normalize_workspace_lifecycle_state, WorkspaceAuthority};
use crate::workspace::{
    emit_create_workspace_plan, execute_create_workspace_plan, require_under_ceiling,
    workspace_host_path, ActionJournalEntry, CreateWorkspaceAction, CreateWorkspaceOutcome,
    CreationPolicy, HostExecuteError, HostExecutorContext, IsolatedWorkspaceDoc,
    MemoryWorkspaceDocuments, WorkspaceAdapterKind, WorkspaceDocuments, WorkspacePlacementDoc,
    CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct ParentWorkspaceStamp {
    pub workspace_id: Option<String>,
    pub workspace_owner_agent_did: Option<String>,
    pub workspace_authority: Option<String>,
    /// Principal from the verified physical parent request, not caller input.
    pub agent_did: String,
    pub workspace_seal_hash: Option<String>,
}

impl ParentWorkspaceStamp {
    pub(crate) fn from_fields(
        agent_did: &str,
        workspace_id: Option<&str>,
        workspace_owner_agent_did: Option<&str>,
        workspace_authority: Option<&str>,
        workspace_seal_hash: Option<&str>,
    ) -> Self {
        Self {
            workspace_id: nonempty(workspace_id).map(str::to_string),
            workspace_owner_agent_did: workspace_owner_agent_did.map(str::to_owned),
            workspace_authority: nonempty(workspace_authority).map(str::to_string),
            agent_did: agent_did.to_owned(),
            workspace_seal_hash: nonempty(workspace_seal_hash).map(str::to_string),
        }
    }

    pub(crate) fn has_workspace_id(&self) -> bool {
        nonempty(self.workspace_id.as_deref()).is_some()
    }

    pub(crate) fn spawn_is_workspace_bound(&self, arg: Option<&SpawnWorkspaceArg>) -> bool {
        self.has_workspace_id() || arg.is_some()
    }

    fn workspace_owner(&self) -> Result<&str, SpawnWorkspaceError> {
        self.workspace_owner_agent_did
            .as_deref()
            .filter(|owner| !owner.trim().is_empty())
            .ok_or_else(|| {
                SpawnWorkspaceError::invalid("parent workspace lacks signed owner scope")
            })
    }

    fn authority(&self) -> Result<Option<WorkspaceAuthority>, SpawnWorkspaceError> {
        match self.workspace_authority.as_deref().map(str::trim) {
            Some(value) if !value.is_empty() => WorkspaceAuthority::parse(value)
                .map(Some)
                .map_err(|error| SpawnWorkspaceError::invalid(error.to_string())),
            _ => Ok(None),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SpawnWorkspaceError {
    pub class: FailureClass,
    pub message: String,
}

impl SpawnWorkspaceError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::ArgumentInvalid,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            class: FailureClass::ServiceUnavailable,
            message: message.into(),
        }
    }

    pub(crate) fn payload(&self) -> String {
        let failure_class = match self.class {
            FailureClass::ArgumentInvalid => "invalid_tool_arguments",
            _ => "service_unavailable",
        };
        serde_json::json!({
            "ok": false,
            "failure_class": failure_class,
            "path": "/workspace",
            "message": self.message,
            "retryable": false,
            "service_id": "subagent",
            "tool_name": "spawn_subagent"
        })
        .to_string()
    }
}

impl std::fmt::Display for SpawnWorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SpawnWorkspaceError {}

/// Skip re-resolve only when the bridge already carries a complete stamp.
pub(crate) fn complete_lineage_from_bridge(
    workspace_id: Option<&str>,
    workspace_owner_agent_did: Option<&str>,
    workspace_authority: Option<&str>,
    workspace_seal_hash: Option<&str>,
) -> Option<WorkspaceLineage> {
    let workspace_id = nonempty(workspace_id)?;
    let workspace_authority = nonempty(workspace_authority)?;
    let workspace_owner_agent_did = nonempty(workspace_owner_agent_did)?;
    Some(WorkspaceLineage {
        workspace_id: Some(workspace_id.to_string()),
        workspace_owner_agent_did: Some(workspace_owner_agent_did.to_string()),
        workspace_authority: Some(workspace_authority.to_string()),
        workspace_seal_hash: nonempty(workspace_seal_hash).map(str::to_string),
    })
}

pub(crate) fn merge_workspace_lineage(bridge: &mut serde_json::Value, lineage: &WorkspaceLineage) {
    let Some(object) = bridge.as_object_mut() else {
        return;
    };
    if let Some(value) = nonempty(lineage.workspace_id.as_deref()) {
        object.insert("workspace_id".to_string(), serde_json::json!(value));
    }
    if let Some(value) = lineage.workspace_owner_agent_did.as_deref() {
        object.insert(
            "workspace_owner_agent_did".to_string(),
            serde_json::json!(value),
        );
    }
    if let Some(value) = nonempty(lineage.workspace_authority.as_deref()) {
        object.insert("workspace_authority".to_string(), serde_json::json!(value));
    }
    if let Some(value) = nonempty(lineage.workspace_seal_hash.as_deref()) {
        object.insert("workspace_seal_hash".to_string(), serde_json::json!(value));
    }
}

/// Re-validate a complete bridge stamp, or resolve inherit/bind/provision.
pub(crate) async fn resolve_child_workspace(
    node: &EmbeddedNode,
    parent: &ParentWorkspaceStamp,
    arg: Option<&SpawnWorkspaceArg>,
    stamped: Option<WorkspaceLineage>,
    writer_principal: &str,
    caused_by_invocation_id: &str,
    caused_by_correlation: &str,
    operator_tool_root: Option<&Path>,
) -> Result<Option<WorkspaceLineage>, SpawnWorkspaceError> {
    if let Some(lineage) = stamped {
        return revalidate_stamped_lineage(node, parent, arg, lineage, writer_principal)
            .await
            .map(Some);
    }
    resolve_spawn_workspace(
        node,
        parent,
        arg,
        writer_principal,
        caused_by_invocation_id,
        caused_by_correlation,
        operator_tool_root,
    )
    .await
}

/// Default inherit when the parent already has `workspace_id`.
pub(crate) async fn resolve_spawn_workspace(
    node: &EmbeddedNode,
    parent: &ParentWorkspaceStamp,
    arg: Option<&SpawnWorkspaceArg>,
    writer_principal: &str,
    caused_by_invocation_id: &str,
    caused_by_correlation: &str,
    operator_tool_root: Option<&Path>,
) -> Result<Option<WorkspaceLineage>, SpawnWorkspaceError> {
    match arg {
        None => {
            if parent.has_workspace_id() {
                inherit_workspace(node, parent, writer_principal)
                    .await
                    .map(Some)
            } else {
                Ok(None)
            }
        }
        Some(SpawnWorkspaceArg::Inherit) => inherit_workspace(node, parent, writer_principal)
            .await
            .map(Some),
        Some(SpawnWorkspaceArg::Bind { id, authority }) => {
            bind_workspace(node, parent, id, authority.as_deref(), writer_principal)
                .await
                .map(Some)
        }
        Some(SpawnWorkspaceArg::Provision { policy }) => provision_workspace(
            node,
            parent,
            policy.as_deref(),
            writer_principal,
            caused_by_invocation_id,
            caused_by_correlation,
            operator_tool_root,
        )
        .await
        .map(Some),
    }
}

async fn inherit_workspace(
    node: &EmbeddedNode,
    parent: &ParentWorkspaceStamp,
    principal_did: &str,
) -> Result<WorkspaceLineage, SpawnWorkspaceError> {
    let parent_id = nonempty(parent.workspace_id.as_deref()).ok_or_else(|| {
        SpawnWorkspaceError::invalid("workspace inherit requires the parent to have workspace_id")
    })?;
    let parent_authority = parent.authority()?.ok_or_else(|| {
        SpawnWorkspaceError::invalid(
            "workspace inherit requires the parent to have workspace_authority",
        )
    })?;
    let workspace = load_workspace(node, parent_id, parent.workspace_owner()?).await?;
    require_parent_stamp_agrees(parent, &workspace)?;
    require_workspace_placement(node, &workspace).await?;
    let default_authority = default_authority_for_state(&workspace.lifecycle_state)?;
    let authority = parent_authority.infimum(default_authority);
    require_principal(&workspace, principal_did, authority)?;
    stamp_from_workspace(&workspace, authority)
}

async fn bind_workspace(
    node: &EmbeddedNode,
    parent: &ParentWorkspaceStamp,
    workspace_id: &str,
    requested_authority: Option<&str>,
    principal_did: &str,
) -> Result<WorkspaceLineage, SpawnWorkspaceError> {
    let workspace_id = nonempty(Some(workspace_id)).ok_or_else(|| {
        SpawnWorkspaceError::invalid("workspace bind requires a non-empty IsolatedWorkspace id")
    })?;
    let workspace = load_workspace(
        node,
        workspace_id,
        if parent.has_workspace_id() {
            parent.workspace_owner()?
        } else {
            &parent.agent_did
        },
    )
    .await?;
    require_workspace_placement(node, &workspace).await?;
    let default_authority = default_authority_for_state(&workspace.lifecycle_state)?;
    let requested = match requested_authority.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => WorkspaceAuthority::parse(value)
            .map_err(|error| SpawnWorkspaceError::invalid(error.to_string()))?,
        None => default_authority,
    };
    let authority = match parent.authority()? {
        Some(parent_authority) => parent_authority.infimum(requested),
        None => requested,
    };
    require_principal(&workspace, principal_did, authority)?;
    stamp_from_workspace(&workspace, authority)
}

async fn provision_workspace(
    node: &EmbeddedNode,
    parent: &ParentWorkspaceStamp,
    policy: Option<&str>,
    writer_principal: &str,
    caused_by_invocation_id: &str,
    caused_by_correlation: &str,
    operator_tool_root: Option<&Path>,
) -> Result<WorkspaceLineage, SpawnWorkspaceError> {
    let creation_policy = parse_creation_policy(policy)?;
    let parent_id = nonempty(parent.workspace_id.as_deref()).ok_or_else(|| {
        SpawnWorkspaceError::invalid(
            "workspace provision requires the parent to be bound to an IsolatedWorkspace",
        )
    })?;
    let parent_workspace = load_workspace(node, parent_id, parent.workspace_owner()?).await?;
    if !parent_workspace.path_capability.is_exact() {
        return Err(SpawnWorkspaceError::invalid(
            "new child workspaces require an exact parent path capability",
        ));
    }
    require_parent_stamp_agrees(parent, &parent_workspace)?;
    require_workspace_placement(node, &parent_workspace).await?;
    let authority = provision_authority(parent)?;
    if !authority.bindable_lifecycle_state("ready") {
        return Err(SpawnWorkspaceError::invalid(format!(
            "isolated workspace provision is not bindable for authority {}",
            authority.as_str()
        )));
    }

    let repository =
        load_repository_placement(node, &parent_workspace.repository_id, writer_principal)
            .await
            .map_err(|error| SpawnWorkspaceError::unavailable(error.to_string()))?
            .ok_or_else(|| {
                SpawnWorkspaceError::unavailable(format!(
                    "RepositoryPlacement {} not found for child principal",
                    parent_workspace.repository_id
                ))
            })?;

    let workspace_id = spawn_provision_workspace_id(caused_by_invocation_id);
    let work_unit_id = spawn_provision_work_unit_id(caused_by_invocation_id);
    let branch = unique_child_branch(&parent_workspace.branch, &workspace_id);
    let operator = operator_tool_root
        .map(Path::to_path_buf)
        .or_else(crate::workspace::process_operator_tool_root);
    let dest = workspace_host_path(
        &repository.host_path,
        &workspace_id,
        &branch,
        operator.as_deref(),
    )
    .map_err(|error| SpawnWorkspaceError::invalid(error.to_string()))?;
    require_under_ceiling(&dest, operator.as_deref()).map_err(|error| {
        SpawnWorkspaceError::invalid(format!(
            "provisioned workspace placement would escape operator ceiling: {error}"
        ))
    })?;
    let executor_ceiling = operator.clone();

    let plan = emit_create_workspace_plan(CreateWorkspaceAction {
        path_capability: parent_workspace.path_capability.clone(),
        workspace_id: workspace_id.clone(),
        work_unit_id,
        repository_id: parent_workspace.repository_id.clone(),
        base_sha: parent_workspace.base_sha.clone(),
        branch,
        creation_policy,
        adapter: WorkspaceAdapterKind::GitWorktree,
        clone_artifacts: None,
    });
    let mut docs = MemoryWorkspaceDocuments::default();
    let mut journal = Vec::<ActionJournalEntry>::new();
    let capabilities: BTreeSet<String> = [CAP_CREATE_WORKSPACE, CAP_OBSERVE_DIRTY_BASE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let execute_result = {
        let mut ctx = HostExecutorContext {
            owner_agent_did: writer_principal.to_owned(),
            repository,
            ceiling: executor_ceiling.as_deref(),
            capabilities,
            writer_principal: writer_principal.to_string(),
            integrator_principal: writer_principal.to_string(),
            caused_by_invocation_id: caused_by_invocation_id.to_string(),
            caused_by_correlation: caused_by_correlation.to_string(),
            documents: &mut docs,
        };
        execute_create_workspace_plan(&plan, &mut journal, &mut ctx)
    };
    persist_provision_docs(node, &execute_result, &docs).await?;
    let outcome = execute_result.map_err(|error| match error {
        HostExecuteError::Denied { reason } => SpawnWorkspaceError::invalid(reason),
        HostExecuteError::Failed { reason, .. } => SpawnWorkspaceError::unavailable(reason),
    })?;
    stamp_from_workspace(&outcome.workspace, authority)
}

async fn persist_provision_docs(
    node: &EmbeddedNode,
    execute_result: &Result<CreateWorkspaceOutcome, HostExecuteError>,
    docs: &MemoryWorkspaceDocuments,
) -> Result<(), SpawnWorkspaceError> {
    match execute_result {
        Ok(outcome) => flush_outcome(node, outcome).await,
        Err(error) => {
            if let Some(outcome) = error.outcome() {
                flush_outcome(node, outcome).await
            } else {
                flush_workspace_docs(node, docs)
                    .await
                    .map_err(|error| SpawnWorkspaceError::unavailable(error.to_string()))
            }
        }
    }
}

async fn flush_outcome(
    node: &EmbeddedNode,
    outcome: &CreateWorkspaceOutcome,
) -> Result<(), SpawnWorkspaceError> {
    let mut written = MemoryWorkspaceDocuments::default();
    written
        .write_isolated_workspace(outcome.workspace.clone())
        .map_err(|error| SpawnWorkspaceError::unavailable(error.to_string()))?;
    written
        .write_placement(outcome.placement.clone())
        .map_err(|error| SpawnWorkspaceError::unavailable(error.to_string()))?;
    flush_workspace_docs(node, &written)
        .await
        .map_err(|error| SpawnWorkspaceError::unavailable(error.to_string()))
}

async fn revalidate_stamped_lineage(
    node: &EmbeddedNode,
    parent: &ParentWorkspaceStamp,
    arg: Option<&SpawnWorkspaceArg>,
    lineage: WorkspaceLineage,
    principal_did: &str,
) -> Result<WorkspaceLineage, SpawnWorkspaceError> {
    let workspace_id = nonempty(lineage.workspace_id.as_deref())
        .ok_or_else(|| SpawnWorkspaceError::invalid("workspace stamp is missing workspace_id"))?;
    lineage
        .require_authority_if_workspace_id()
        .map_err(|error| SpawnWorkspaceError::invalid(error.to_string()))?;
    let owner = lineage.workspace_owner_agent_did.as_deref().unwrap();
    if matches!(arg, Some(SpawnWorkspaceArg::Provision { .. })) && owner != principal_did {
        return Err(SpawnWorkspaceError::invalid(
            "provisioned workspace is not owned by selected child",
        ));
    }
    if parent.has_workspace_id() && parent.workspace_id == lineage.workspace_id {
        let source = WorkspaceLineage {
            workspace_id: parent.workspace_id.clone(),
            workspace_owner_agent_did: parent.workspace_owner_agent_did.clone(),
            workspace_authority: parent.workspace_authority.clone(),
            workspace_seal_hash: parent.workspace_seal_hash.clone(),
        };
        lineage
            .validate_source(&source, true)
            .map_err(|error| SpawnWorkspaceError::invalid(error.to_string()))?;
    } else if let Some(parent_authority) = parent.authority()? {
        let requested = WorkspaceAuthority::parse(lineage.workspace_authority.as_deref().unwrap())
            .map_err(|error| SpawnWorkspaceError::invalid(error.to_string()))?;
        if requested.infimum(parent_authority) != requested {
            return Err(SpawnWorkspaceError::invalid(
                "workspace authority exceeds parent authority",
            ));
        }
    }
    let workspace = load_workspace(node, workspace_id, owner).await?;
    require_parent_stamp_agrees(
        &ParentWorkspaceStamp::from_fields(
            owner,
            lineage.workspace_id.as_deref(),
            lineage.workspace_owner_agent_did.as_deref(),
            lineage.workspace_authority.as_deref(),
            lineage.workspace_seal_hash.as_deref(),
        ),
        &workspace,
    )?;
    require_workspace_placement(node, &workspace).await?;
    let authority = nonempty(lineage.workspace_authority.as_deref())
        .ok_or_else(|| {
            SpawnWorkspaceError::invalid("workspace stamp is missing workspace_authority")
        })
        .and_then(|value| {
            WorkspaceAuthority::parse(value)
                .map_err(|error| SpawnWorkspaceError::invalid(error.to_string()))
        })?;
    require_principal(&workspace, principal_did, authority)?;
    stamp_from_workspace(&workspace, authority)
}

fn require_principal(
    workspace: &IsolatedWorkspaceDoc,
    principal_did: &str,
    authority: WorkspaceAuthority,
) -> Result<(), SpawnWorkspaceError> {
    let required = match authority {
        WorkspaceAuthority::ReadOnly => return Ok(()),
        WorkspaceAuthority::ReadWrite => &workspace.writer_principal,
        WorkspaceAuthority::Integrate => &workspace.integrator_principal,
    };
    if principal_did.trim() != required.trim() {
        return Err(SpawnWorkspaceError::invalid(format!(
            "principal {principal_did} is not authorized for {} on workspace {}",
            authority.as_str(),
            workspace.workspace_id
        )));
    }
    Ok(())
}

fn provision_authority(
    parent: &ParentWorkspaceStamp,
) -> Result<WorkspaceAuthority, SpawnWorkspaceError> {
    Ok(match parent.authority()? {
        Some(parent_authority) => parent_authority.infimum(WorkspaceAuthority::ReadWrite),
        None => WorkspaceAuthority::ReadWrite,
    })
}

fn stamp_from_workspace(
    workspace: &IsolatedWorkspaceDoc,
    authority: WorkspaceAuthority,
) -> Result<WorkspaceLineage, SpawnWorkspaceError> {
    if !authority.bindable_lifecycle_state(&workspace.lifecycle_state) {
        return Err(SpawnWorkspaceError::invalid(format!(
            "isolated workspace {} in state {} is not bindable for authority {}",
            workspace.workspace_id,
            workspace.lifecycle_state,
            authority.as_str()
        )));
    }
    let seal_hash = nonempty(workspace.seal_hash.as_deref()).map(str::to_string);
    if matches!(
        normalize_workspace_lifecycle_state(&workspace.lifecycle_state),
        Some("sealed")
    ) && seal_hash.is_none()
    {
        return Err(SpawnWorkspaceError::unavailable(format!(
            "sealed workspace {} is missing seal_hash",
            workspace.workspace_id
        )));
    }
    Ok(WorkspaceLineage {
        workspace_id: Some(workspace.workspace_id.clone()),
        workspace_owner_agent_did: Some(workspace.owner_agent_did.clone()),
        workspace_authority: Some(authority.as_str().to_string()),
        workspace_seal_hash: seal_hash,
    })
}

fn default_authority_for_state(state: &str) -> Result<WorkspaceAuthority, SpawnWorkspaceError> {
    match normalize_workspace_lifecycle_state(state) {
        Some("ready") => Ok(WorkspaceAuthority::ReadWrite),
        Some("sealed") => Ok(WorkspaceAuthority::ReadOnly),
        _ => Err(SpawnWorkspaceError::invalid(format!(
            "isolated workspace in state {state} is not Ready or Sealed"
        ))),
    }
}

fn parse_creation_policy(policy: Option<&str>) -> Result<CreationPolicy, SpawnWorkspaceError> {
    match policy.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("git_worktree_diff") => Ok(CreationPolicy::GitWorktreeDiff),
        Some(other) => Err(SpawnWorkspaceError::invalid(format!(
            "creation_policy '{other}' is not implemented in v1 (only git_worktree_diff)"
        ))),
    }
}

async fn load_workspace(
    node: &EmbeddedNode,
    workspace_id: &str,
    owner_agent_did: &str,
) -> Result<IsolatedWorkspaceDoc, SpawnWorkspaceError> {
    if owner_agent_did.trim().is_empty() {
        return Err(SpawnWorkspaceError::invalid(
            "workspace lookup requires the verified parent principal",
        ));
    }
    match load_isolated_workspace(node, workspace_id, owner_agent_did).await {
        Ok(Some(doc))
            if doc.owner_agent_did == owner_agent_did && doc.workspace_id == workspace_id =>
        {
            Ok(doc)
        }
        Ok(Some(_)) => Err(SpawnWorkspaceError::invalid(
            "workspace owner or identity mismatch",
        )),
        Ok(None) => Err(SpawnWorkspaceError::unavailable(format!(
            "isolated workspace {workspace_id} not found"
        ))),
        Err(error) => Err(SpawnWorkspaceError::unavailable(error.to_string())),
    }
}

fn require_parent_stamp_agrees(
    parent: &ParentWorkspaceStamp,
    workspace: &IsolatedWorkspaceDoc,
) -> Result<(), SpawnWorkspaceError> {
    if parent.workspace_owner()? != workspace.owner_agent_did {
        return Err(SpawnWorkspaceError::invalid(
            "parent workspace scope does not match IsolatedWorkspace owner",
        ));
    }
    if let Some(parent_seal) = nonempty(parent.workspace_seal_hash.as_deref()) {
        match nonempty(workspace.seal_hash.as_deref()) {
            Some(workspace_seal) if workspace_seal == parent_seal => {}
            Some(workspace_seal) => {
                return Err(SpawnWorkspaceError::invalid(format!(
                    "parent workspace_seal_hash {parent_seal} does not match IsolatedWorkspace seal_hash {workspace_seal}"
                )));
            }
            None => {
                return Err(SpawnWorkspaceError::invalid(format!(
                    "parent workspace_seal_hash {parent_seal} does not match IsolatedWorkspace {} (missing seal_hash)",
                    workspace.workspace_id
                )));
            }
        }
    }
    Ok(())
}

async fn require_workspace_placement(
    node: &EmbeddedNode,
    workspace: &IsolatedWorkspaceDoc,
) -> Result<WorkspacePlacementDoc, SpawnWorkspaceError> {
    let placement =
        load_workspace_placement(node, &workspace.workspace_id, &workspace.owner_agent_did)
            .await
            .map_err(|error| SpawnWorkspaceError::unavailable(error.to_string()))?
            .ok_or_else(|| {
                SpawnWorkspaceError::unavailable(format!(
                    "workspace placement for {} not found for principal {}",
                    workspace.workspace_id, workspace.owner_agent_did
                ))
            })?;
    if placement.owner_agent_did != workspace.owner_agent_did
        || placement.workspace_id != workspace.workspace_id
    {
        return Err(SpawnWorkspaceError::invalid(
            "workspace placement owner or identity does not match IsolatedWorkspace",
        ));
    }
    let host_path = Path::new(placement.host_path.trim());
    if !host_path.is_absolute() || !host_path.is_dir() {
        return Err(SpawnWorkspaceError::unavailable(format!(
            "workspace placement for {} is missing a local directory",
            workspace.workspace_id
        )));
    }
    Ok(placement)
}

pub(crate) fn spawn_provision_workspace_id(caused_by_invocation_id: &str) -> String {
    format!(
        "spawn-ws-{}",
        sanitize_id(nonempty(Some(caused_by_invocation_id)).unwrap_or("unknown"))
    )
}

fn spawn_provision_work_unit_id(caused_by_invocation_id: &str) -> String {
    format!(
        "spawn-unit-{}",
        sanitize_id(nonempty(Some(caused_by_invocation_id)).unwrap_or("unknown"))
    )
}

pub(crate) fn unique_child_branch(parent_branch: &str, workspace_id: &str) -> String {
    let parent = sanitize_id(nonempty(Some(parent_branch)).unwrap_or("topic"));
    let id = sanitize_id(workspace_id);
    format!("{parent}-ws-{id}")
}

fn sanitize_id(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        "workspace".to_string()
    } else {
        out
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background_tools::parse_spawn_workspace_arg;
    use serde_json::json;

    #[test]
    fn workspace_arg_parses_inherit_bind_provision() {
        assert_eq!(
            parse_spawn_workspace_arg(&json!("inherit")).unwrap(),
            SpawnWorkspaceArg::Inherit
        );
        assert_eq!(
            parse_spawn_workspace_arg(&json!({"id": "ws-1"})).unwrap(),
            SpawnWorkspaceArg::Bind {
                id: "ws-1".into(),
                authority: None,
            }
        );
        assert_eq!(
            parse_spawn_workspace_arg(&json!({"id": "ws-1", "authority": "readOnly"})).unwrap(),
            SpawnWorkspaceArg::Bind {
                id: "ws-1".into(),
                authority: Some("readOnly".into()),
            }
        );
        assert_eq!(
            parse_spawn_workspace_arg(&json!({"provision": {"policy": "git_worktree_diff"}}))
                .unwrap(),
            SpawnWorkspaceArg::Provision {
                policy: Some("git_worktree_diff".into()),
            }
        );
        assert_eq!(
            parse_spawn_workspace_arg(&json!({"provision": {}})).unwrap(),
            SpawnWorkspaceArg::Provision { policy: None }
        );
    }

    #[test]
    fn inherit_infimum_cannot_outrank_parent() {
        assert_eq!(
            WorkspaceAuthority::ReadOnly.infimum(WorkspaceAuthority::ReadWrite),
            WorkspaceAuthority::ReadOnly
        );
        assert_eq!(
            WorkspaceAuthority::ReadWrite.infimum(WorkspaceAuthority::ReadWrite),
            WorkspaceAuthority::ReadWrite
        );
        assert_eq!(
            WorkspaceAuthority::Integrate.infimum(WorkspaceAuthority::ReadWrite),
            WorkspaceAuthority::Integrate
        );
        assert!(!WorkspaceAuthority::Integrate.bindable_lifecycle_state("ready"));
    }

    #[test]
    fn provision_ids_are_stable_per_tool_call() {
        assert_eq!(
            spawn_provision_workspace_id("internal-spawn-a"),
            "spawn-ws-internal-spawn-a"
        );
        assert_ne!(
            spawn_provision_workspace_id("internal-spawn-a"),
            spawn_provision_workspace_id("internal-spawn-b")
        );
    }

    #[test]
    fn unique_child_branch_does_not_reuse_parent() {
        let branch = unique_child_branch("topic", "spawn-ws-child");
        assert_ne!(branch, "topic");
        assert!(branch.starts_with("topic-ws-"));
        assert_ne!(
            unique_child_branch("topic", "spawn-ws-a"),
            unique_child_branch("topic", "spawn-ws-b")
        );
    }

    #[test]
    fn spawn_is_workspace_bound_when_parent_or_arg_is_set() {
        let unbound = ParentWorkspaceStamp::default();
        assert!(!unbound.spawn_is_workspace_bound(None));
        assert!(unbound.spawn_is_workspace_bound(Some(&SpawnWorkspaceArg::Inherit)));
        assert!(
            unbound.spawn_is_workspace_bound(Some(&SpawnWorkspaceArg::Provision { policy: None }))
        );
        let bound =
            ParentWorkspaceStamp::from_fields("owner", Some("ws-1"), Some("owner"), None, None);
        assert!(bound.spawn_is_workspace_bound(None));
    }

    #[test]
    fn complete_lineage_requires_workspace_and_authority() {
        assert!(complete_lineage_from_bridge(Some("ws-1"), Some("owner"), None, None).is_none());
        assert!(
            complete_lineage_from_bridge(None, Some("owner"), Some("readOnly"), None).is_none()
        );
        let lineage = complete_lineage_from_bridge(
            Some("ws-1"),
            Some("owner"),
            Some("readOnly"),
            Some("abc"),
        )
        .unwrap();
        assert_eq!(lineage.workspace_id.as_deref(), Some("ws-1"));
        assert_eq!(lineage.workspace_authority.as_deref(), Some("readOnly"));
        assert_eq!(lineage.workspace_seal_hash.as_deref(), Some("abc"));
    }

    fn workspace(owner: &str, writer: &str) -> IsolatedWorkspaceDoc {
        IsolatedWorkspaceDoc {
            path_capability: crate::workspace::WorkspacePathCapability::exact_paths(vec![
                "src".into()
            ])
            .unwrap(),
            workspace_id: "shared-label".into(),
            work_unit_id: "unit".into(),
            repository_id: "repo".into(),
            base_sha: "base".into(),
            branch: "branch".into(),
            creation_policy: "git_worktree_diff".into(),
            adapter: "git_worktree".into(),
            owner_agent_did: owner.into(),
            writer_principal: writer.into(),
            integrator_principal: owner.into(),
            instruction_manifest: "{}".into(),
            seal_hash: None,
            lifecycle_state: "ready".into(),
            caused_by_invocation_id: "invocation".into(),
            caused_by_correlation: "correlation".into(),
        }
    }

    #[test]
    fn parent_owner_and_seal_are_checked_independently_of_child_writer() {
        let mut doc = workspace("owner", "child");
        let parent = ParentWorkspaceStamp::from_fields(
            "owner",
            Some("shared-label"),
            Some("owner"),
            Some("readOnly"),
            None,
        );
        require_parent_stamp_agrees(&parent, &doc).unwrap();
        require_principal(&doc, "child", WorkspaceAuthority::ReadWrite).unwrap();
        assert!(require_principal(&doc, "foreign", WorkspaceAuthority::ReadWrite).is_err());
        require_principal(&doc, "foreign", WorkspaceAuthority::ReadOnly).unwrap();
        let foreign = ParentWorkspaceStamp::from_fields(
            "foreign",
            Some("shared-label"),
            Some("foreign"),
            Some("readOnly"),
            None,
        );
        assert!(require_parent_stamp_agrees(&foreign, &doc).is_err());
        doc.lifecycle_state = "sealed".into();
        assert!(stamp_from_workspace(&doc, WorkspaceAuthority::ReadOnly).is_err());
        doc.seal_hash = Some("actual-seal".into());
        let stale = ParentWorkspaceStamp::from_fields(
            "owner",
            Some("shared-label"),
            Some("owner"),
            Some("readOnly"),
            Some("stale-seal"),
        );
        assert!(require_parent_stamp_agrees(&stale, &doc).is_err());
        assert!(stamp_from_workspace(&doc, WorkspaceAuthority::ReadWrite).is_err());
        assert_eq!(
            stamp_from_workspace(&doc, WorkspaceAuthority::ReadOnly)
                .unwrap()
                .workspace_seal_hash
                .as_deref(),
            Some("actual-seal")
        );
    }

    #[tokio::test]
    async fn inheritance_uses_exact_parent_owner_and_preserves_readonly_attenuation() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema(gents_protocol::schemas::ISOLATED_WORKSPACE)
            .await
            .unwrap();
        node.add_schema(gents_protocol::schemas::WORKSPACE_PLACEMENT)
            .await
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        for owner in ["foreign", "owner"] {
            let mut docs = MemoryWorkspaceDocuments::default();
            docs.write_isolated_workspace(workspace(owner, "child"))
                .unwrap();
            docs.write_placement(WorkspacePlacementDoc {
                workspace_id: "shared-label".into(),
                owner_agent_did: owner.into(),
                host_path: directory.path().display().to_string(),
                repository_placement_id: "repo".into(),
                adapter: "git_worktree".into(),
                adapter_version: "1".into(),
                dirty_base: false,
                dirty_base_summary: "".into(),
                provisioning_state: "ready".into(),
                observed_tree_hash: "".into(),
            })
            .unwrap();
            flush_workspace_docs(&node, &docs).await.unwrap();
        }
        let parent = ParentWorkspaceStamp::from_fields(
            "owner",
            Some("shared-label"),
            Some("owner"),
            Some("readOnly"),
            None,
        );
        let lineage = inherit_workspace(&node, &parent, "child").await.unwrap();
        assert_eq!(lineage.workspace_id.as_deref(), Some("shared-label"));
        assert_eq!(lineage.workspace_authority.as_deref(), Some("readOnly"));
        assert_eq!(lineage.workspace_owner_agent_did.as_deref(), Some("owner"));
        let child_parent = ParentWorkspaceStamp::from_fields(
            "child",
            lineage.workspace_id.as_deref(),
            lineage.workspace_owner_agent_did.as_deref(),
            lineage.workspace_authority.as_deref(),
            lineage.workspace_seal_hash.as_deref(),
        );
        let grandchild = inherit_workspace(&node, &child_parent, "grandchild")
            .await
            .unwrap();
        assert_eq!(
            grandchild.workspace_owner_agent_did.as_deref(),
            Some("owner")
        );
        assert_eq!(grandchild.workspace_authority.as_deref(), Some("readOnly"));
        let mut substituted = grandchild.clone();
        substituted.workspace_owner_agent_did = Some("foreign".into());
        assert!(substituted.validate_source(&lineage, true).is_err());
        let mut widened = grandchild.clone();
        widened.workspace_authority = Some("readWrite".into());
        assert!(
            revalidate_stamped_lineage(&node, &child_parent, None, widened, "child")
                .await
                .is_err()
        );
        assert!(grandchild.validate_source(&lineage, false).is_err());

        assert_eq!(
            load_workspace(&node, "shared-label", "owner")
                .await
                .unwrap()
                .owner_agent_did,
            "owner"
        );
        assert!(load_workspace(&node, "shared-label", "missing-owner")
            .await
            .is_err());
        let denied = ParentWorkspaceStamp::from_fields(
            "",
            Some("shared-label"),
            None,
            Some("readOnly"),
            None,
        );
        assert!(inherit_workspace(&node, &denied, "child").await.is_err());
    }
}
