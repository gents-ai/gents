use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, bail, Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::request::watcher::AgentRequest;
use crate::store::graphql::escape_graphql_string;
#[cfg(test)]
use crate::store::graphql::{first_row, graphql_with_transaction_retry, rows};
use crate::tools::native::{workspace_write_sandbox_enforced, WorkspaceAuthority};
use crate::tools::surface::{resolve_configured_tool_root, FileToolMode};

#[cfg(test)]
use super::binding::{admit_workspace_binding, new_binding, AdmitBinding};
use super::documents::WorkspaceBindingDoc;
#[cfg(test)]
use super::documents::{workspace_binding_upsert_mutation, workspace_bindings_upsert_mutation};

static PROCESS_OPERATOR_TOOL_ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);

pub(crate) fn install_process_operator_tool_root(root: Option<PathBuf>) {
    *PROCESS_OPERATOR_TOOL_ROOT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = root;
}

pub(crate) fn process_operator_tool_root() -> Option<PathBuf> {
    PROCESS_OPERATOR_TOOL_ROOT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IsolatedWorkspaceRecord {
    pub workspace_id: String,
    pub work_unit_id: Option<String>,
    pub caused_by_invocation_id: Option<String>,
    pub owner_node_did: String,
    pub writer_principal: String,
    pub integrator_principal: String,
    pub lifecycle_state: String,
    pub seal_hash: Option<String>,
    pub instruction_manifest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspacePlacementRecord {
    pub workspace_id: String,
    pub owner_node_did: String,
    pub host_path: String,
    pub observed_tree_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceOverlay {
    pub workspace_artifact: Option<super::ArtifactGrant>,
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub authority: WorkspaceAuthority,
    pub instruction_manifest: String,
    #[allow(dead_code)]
    pub seal_hash: Option<String>,
}

pub(crate) fn frozen_instruction_manifest_from_overlay(
    overlay: Option<&WorkspaceOverlay>,
) -> Option<&str> {
    overlay.map(|overlay| overlay.instruction_manifest.as_str())
}

pub(crate) struct WorkspaceBindInput<'a> {
    pub workspace_id: &'a str,
    pub authority: WorkspaceAuthority,
    pub seal_hash: Option<&'a str>,
    pub request_cwd: Option<&'a Path>,
    pub node_did: &'a str,
    pub operator_tool_root: Option<&'a Path>,
    pub workspace_write_sandbox_enforced: bool,
    pub live_tree_hash: Option<&'a str>,
}

pub(crate) fn workspace_authority_file_mode(authority: WorkspaceAuthority) -> FileToolMode {
    match authority {
        WorkspaceAuthority::ReadWrite => FileToolMode::ReadWrite,
        WorkspaceAuthority::ReadOnly | WorkspaceAuthority::Integrate => FileToolMode::ReadOnly,
    }
}

pub(crate) fn request_workspace_owner(request: &AgentRequest) -> Result<&str> {
    gents_protocol::request_admission::validate_workspace_reference(
        request.workspace_id.as_deref(),
        request.workspace_owner_node_did.as_deref(),
        request.workspace_authority.as_deref(),
        request.workspace_seal_hash.as_deref(),
    )?;
    request
        .workspace_owner_node_did
        .as_deref()
        .context("workspace owner node is missing")
}

pub(crate) fn require_workspace_principal(
    workspace: &IsolatedWorkspaceRecord,
    principal_did: &str,
    authority: WorkspaceAuthority,
) -> Result<()> {
    let required = match authority {
        WorkspaceAuthority::ReadOnly => return Ok(()),
        WorkspaceAuthority::ReadWrite => &workspace.writer_principal,
        WorkspaceAuthority::Integrate => &workspace.integrator_principal,
    };
    if principal_did.trim() != required.trim() {
        bail!(
            "Agent {principal_did} is not selected for {} on workspace {}",
            authority.as_str(),
            workspace.workspace_id
        );
    }
    Ok(())
}

#[inline(never)]
#[cfg(test)]
pub(crate) async fn resolve_request_workspace_overlay(
    node: &Arc<EmbeddedNode>,
    request: &AgentRequest,
    execution_generation: &str,
    artifact_requested: bool,
    operator_tool_root: Option<&Path>,
) -> Result<Option<WorkspaceOverlay>> {
    resolve_request_workspace_overlay_on_host(
        node,
        request,
        execution_generation,
        artifact_requested,
        operator_tool_root,
        workspace_write_sandbox_enforced(),
    )
    .await
}

pub(crate) async fn resolve_request_workspace_overlay_with_identity(
    node: &Arc<EmbeddedNode>,
    agent_identity: &dyn crate::store::identity::NodeIdentity,
    request: &AgentRequest,
    execution_generation: &str,
    artifact_requested: bool,
    operator_tool_root: Option<&Path>,
) -> Result<Option<WorkspaceOverlay>> {
    let access = super::runtime::completion_access(node, agent_identity, &request.agent_did)?;
    let Some((workspace, mut overlay)) = load_request_workspace_overlay(
        node,
        Some(&access),
        request,
        artifact_requested,
        operator_tool_root,
        workspace_write_sandbox_enforced(),
    )
    .await?
    else {
        return Ok(None);
    };
    let lineage = crate::request::lifecycle::WorkspaceLineage {
        workspace_id: request.workspace_id.clone(),
        workspace_owner_node_did: request.workspace_owner_node_did.clone(),
        workspace_authority: request.workspace_authority.clone(),
        workspace_seal_hash: request.workspace_seal_hash.clone(),
    };
    super::runtime::materialize_workspace_binding(
        node,
        &request.request_id,
        &request.doc_id,
        agent_identity,
        &lineage,
    )
    .await?;
    if artifact_requested {
        overlay.workspace_artifact = Some(
            super::ArtifactGrant::create_with_access(
                access,
                request,
                execution_generation,
                &overlay.root,
                &workspace.owner_node_did,
                workspace
                    .seal_hash
                    .as_deref()
                    .context("artifact workspace requires a seal")?,
            )
            .await?,
        );
    }
    Ok(Some(overlay))
}

#[cfg(test)]
async fn resolve_request_workspace_overlay_on_host(
    node: &Arc<EmbeddedNode>,
    request: &AgentRequest,
    execution_generation: &str,
    artifact_requested: bool,
    operator_tool_root: Option<&Path>,
    sandbox_enforced: bool,
) -> Result<Option<WorkspaceOverlay>> {
    let Some((workspace, mut overlay)) = load_request_workspace_overlay(
        node,
        None,
        request,
        artifact_requested,
        operator_tool_root,
        sandbox_enforced,
    )
    .await?
    else {
        return Ok(None);
    };
    ensure_request_binding(
        node,
        request,
        &workspace,
        &workspace.owner_node_did,
        overlay.authority,
    )
    .await?;
    if artifact_requested {
        overlay.workspace_artifact = Some(
            super::ArtifactGrant::create(
                node.clone(),
                request,
                execution_generation,
                &overlay.root,
                &workspace.owner_node_did,
                workspace
                    .seal_hash
                    .as_deref()
                    .ok_or_else(|| anyhow!("artifact workspace requires a seal"))?,
            )
            .await?,
        );
    }
    Ok(Some(overlay))
}

#[cfg(test)]
pub(crate) async fn validate_request_workspace_input(
    node: &Arc<EmbeddedNode>,
    request: &AgentRequest,
    artifact_requested: bool,
) -> Result<()> {
    let operator_root = process_operator_tool_root();
    if super::runtime::writer_request_already_sealed(node, request).await? {
        let workspace_id = request
            .workspace_id
            .as_deref()
            .context("sealed writer lacks workspace")?;
        let placement =
            load_workspace_placement(node, workspace_id, request_workspace_owner(request)?)
                .await?
                .context("sealed writer workspace placement missing")?;
        let root = canonicalize_placement_path(&placement.host_path)?;
        require_under_ceiling(&root, operator_root.as_deref())?;
        resolve_bound_cwd(&root, request_workspace_cwd(request).as_deref())?;
        return Ok(());
    }
    load_request_workspace_overlay(
        node,
        None,
        request,
        artifact_requested,
        operator_root.as_deref(),
        workspace_write_sandbox_enforced(),
    )
    .await?;
    Ok(())
}

pub(crate) fn validate_unbound_request_workspace_input(
    request: &AgentRequest,
    artifact_requested: bool,
) -> Result<()> {
    anyhow::ensure!(
        optional_id(request.workspace_id.as_deref()).is_none(),
        "workspace access requires the selected Agent"
    );
    if artifact_requested {
        bail!("artifact_write requires a sealed ReadOnly workspace binding");
    }
    if let Some(cwd) = request_workspace_cwd(request) {
        let cwd = std::fs::canonicalize(&cwd).context("resolve invocation cwd")?;
        anyhow::ensure!(cwd.is_dir(), "invocation cwd is not a directory");
        require_under_ceiling(&cwd, process_operator_tool_root().as_deref())?;
    }
    Ok(())
}

pub(crate) async fn validate_request_workspace_input_with_identity(
    node: &Arc<EmbeddedNode>,
    agent_identity: &dyn crate::store::identity::NodeIdentity,
    request: &AgentRequest,
    artifact_requested: bool,
) -> Result<()> {
    let operator_root = process_operator_tool_root();
    let access = super::runtime::completion_access(node, agent_identity, &request.agent_did)?;
    if super::runtime::writer_request_already_sealed_with_identity(node, agent_identity, request)
        .await?
    {
        let workspace_id = request
            .workspace_id
            .as_deref()
            .context("sealed writer lacks workspace")?;
        let placement = load_workspace_placement_with_access(
            &access,
            workspace_id,
            request_workspace_owner(request)?,
        )
        .await?
        .context("sealed writer workspace placement missing")?;
        let root = canonicalize_placement_path(&placement.host_path)?;
        require_under_ceiling(&root, operator_root.as_deref())?;
        resolve_bound_cwd(&root, request_workspace_cwd(request).as_deref())?;
        return Ok(());
    }
    load_request_workspace_overlay(
        node,
        Some(&access),
        request,
        artifact_requested,
        operator_root.as_deref(),
        workspace_write_sandbox_enforced(),
    )
    .await?;
    Ok(())
}

async fn load_request_workspace_overlay(
    _node: &EmbeddedNode,
    access: Option<&crate::ConfigAccess>,
    request: &AgentRequest,
    artifact_requested: bool,
    operator_tool_root: Option<&Path>,
    sandbox_enforced: bool,
) -> Result<Option<(IsolatedWorkspaceRecord, WorkspaceOverlay)>> {
    if artifact_requested && !sandbox_enforced {
        bail!("artifact_write requires an enforceable artifact sandbox on this host");
    }
    let Some(workspace_id) = optional_id(request.workspace_id.as_deref()) else {
        if artifact_requested {
            bail!("artifact_write requires a sealed ReadOnly workspace binding");
        }
        if let Some(cwd) = request_workspace_cwd(request) {
            let cwd = std::fs::canonicalize(&cwd).context("resolve invocation cwd")?;
            anyhow::ensure!(cwd.is_dir(), "invocation cwd is not a directory");
            require_under_ceiling(&cwd, operator_tool_root)?;
        }
        return Ok(None);
    };
    let authority = match request.workspace_authority.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => WorkspaceAuthority::parse(value)?,
        _ => bail!("workspace-bound request {workspace_id} is missing workspace_authority"),
    };

    let workspace = match access {
        Some(access) => {
            load_isolated_workspace_record_with_access(
                access,
                workspace_id,
                request_workspace_owner(request)?,
            )
            .await?
        }
        #[cfg(test)]
        None => {
            load_isolated_workspace_record(_node, workspace_id, request_workspace_owner(request)?)
                .await?
        }
        #[cfg(not(test))]
        None => bail!("workspace access requires the selected Agent"),
    }
    .ok_or_else(|| anyhow!("isolated workspace {workspace_id} not found"))?;
    require_workspace_principal(&workspace, &request.agent_did, authority)?;
    let node_did = request_workspace_owner(request)?;
    let placement = match access {
        Some(access) => {
            load_workspace_placement_with_access(access, workspace_id, node_did).await?
        }
        #[cfg(test)]
        None => load_workspace_placement(_node, workspace_id, node_did).await?,
        #[cfg(not(test))]
        None => bail!("workspace access requires the selected Agent"),
    }
    .ok_or_else(|| anyhow!("workspace placement for {workspace_id} not found on this node"))?;
    let request_cwd = request_workspace_cwd(request);
    let sealed =
        crate::tools::native::normalize_workspace_lifecycle_state(&workspace.lifecycle_state)
            == Some("sealed");
    // Reject before binding or provider dispatch: a missing grant must never
    // let an artifact-selected request fall through legacy CLI/LSP paths.
    if artifact_requested && (!sealed || authority != WorkspaceAuthority::ReadOnly) {
        bail!("artifact_write requires a sealed ReadOnly workspace binding");
    }
    let live_tree_hash = if sealed {
        Some(super::adapter::working_tree_hash(Path::new(&placement.host_path)).await?)
    } else {
        None
    };
    let overlay = bind_workspace_overlay(
        &workspace,
        &placement,
        WorkspaceBindInput {
            workspace_id,
            authority,
            seal_hash: optional_id(request.workspace_seal_hash.as_deref()),
            request_cwd: request_cwd.as_deref(),
            node_did,
            operator_tool_root,
            workspace_write_sandbox_enforced: sandbox_enforced,
            live_tree_hash: live_tree_hash.as_deref(),
        },
    )?;
    Ok(Some((workspace, overlay)))
}

pub(crate) fn bind_workspace_overlay(
    workspace: &IsolatedWorkspaceRecord,
    placement: &WorkspacePlacementRecord,
    input: WorkspaceBindInput<'_>,
) -> Result<WorkspaceOverlay> {
    if workspace.workspace_id != input.workspace_id {
        bail!(
            "isolated workspace id {} does not match request {}",
            workspace.workspace_id,
            input.workspace_id
        );
    }
    if placement.workspace_id != input.workspace_id {
        bail!(
            "workspace placement id {} does not match request {}",
            placement.workspace_id,
            input.workspace_id
        );
    }
    if !input
        .authority
        .bindable_lifecycle_state(&workspace.lifecycle_state)
    {
        bail!(
            "isolated workspace {} in state {} is not bindable for authority {}",
            input.workspace_id,
            workspace.lifecycle_state,
            input.authority.as_str()
        );
    }
    anyhow::ensure!(
        workspace.owner_node_did == input.node_did && placement.owner_node_did == input.node_did,
        "workspace and placement must match the signed workspace owner"
    );

    if matches!(input.authority, WorkspaceAuthority::ReadWrite)
        && !input.workspace_write_sandbox_enforced
    {
        bail!(
            "ReadWrite workspace binding requires an enforceable WorkspaceWrite sandbox on this host"
        );
    }

    if crate::tools::native::normalize_workspace_lifecycle_state(&workspace.lifecycle_state)
        == Some("sealed")
    {
        let workspace_hash = optional_id(workspace.seal_hash.as_deref()).ok_or_else(|| {
            anyhow!(
                "sealed workspace {} is missing seal_hash",
                input.workspace_id
            )
        })?;
        let request_hash = input.seal_hash.ok_or_else(|| {
            anyhow!(
                "sealed workspace {} requires request.workspace_seal_hash",
                input.workspace_id
            )
        })?;
        if request_hash != workspace_hash {
            bail!(
                "request workspace_seal_hash {request_hash} does not match workspace seal_hash {workspace_hash}"
            );
        }
        let observed = optional_id(placement.observed_tree_hash.as_deref()).ok_or_else(|| {
            anyhow!(
                "sealed workspace {} is missing placement observed_tree_hash",
                input.workspace_id
            )
        })?;
        if observed != workspace_hash {
            bail!(
                "placement observed_tree_hash {observed} does not match workspace seal_hash {workspace_hash}"
            );
        }
        let live = input
            .live_tree_hash
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "sealed workspace {} requires live tree hash",
                    input.workspace_id
                )
            })?;
        if live != workspace_hash {
            bail!("live tree hash {live} does not match workspace seal_hash {workspace_hash}");
        }
    }

    let root = canonicalize_placement_path(&placement.host_path)?;
    require_under_ceiling(&root, input.operator_tool_root)?;
    let cwd = resolve_bound_cwd(&root, input.request_cwd)?;

    Ok(WorkspaceOverlay {
        workspace_artifact: None,
        root,
        cwd,
        authority: input.authority,
        instruction_manifest: workspace.instruction_manifest.clone(),
        seal_hash: workspace.seal_hash.clone(),
    })
}

fn resolve_bound_cwd(root: &Path, cwd: Option<&Path>) -> Result<PathBuf> {
    let Some(cwd) = cwd else {
        return Ok(root.to_path_buf());
    };
    let canonical = std::fs::canonicalize(cwd)
        .with_context(|| format!("canonicalizing request cwd {}", cwd.display()))?;
    anyhow::ensure!(
        canonical.is_dir(),
        "request cwd {} is not a directory",
        canonical.display()
    );
    let admission = crate::tools::surface::resolve_admitted_tool_root(&canonical, [root])?;
    anyhow::ensure!(
        admission.admitted().is_some(),
        "request cwd {} is not under workspace root {}",
        canonical.display(),
        root.display()
    );
    Ok(canonical)
}

fn canonicalize_placement_path(host_path: &str) -> Result<PathBuf> {
    let path = Path::new(host_path.trim());
    if !path.is_absolute() {
        bail!("workspace placement host_path must be absolute: {host_path}");
    }
    if !path.is_dir() {
        bail!(
            "workspace placement host_path is not a directory: {}",
            path.display()
        );
    }
    std::fs::canonicalize(path)
        .with_context(|| format!("canonicalizing workspace placement {}", path.display()))
}

pub(crate) fn require_under_ceiling(path: &Path, operator_tool_root: Option<&Path>) -> Result<()> {
    let operator = operator_tool_root
        .map(resolve_configured_tool_root)
        .transpose()?;
    if let Some(ceiling) = operator.as_deref() {
        let admission = crate::tools::surface::resolve_admitted_tool_root(path, [ceiling])?;
        if admission.admitted().is_none() {
            bail!(
                "workspace placement {} escapes operator tool root {}",
                path.display(),
                ceiling.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn optional_id(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(crate) fn request_workspace_cwd(request: &AgentRequest) -> Option<PathBuf> {
    request.input.cwd.as_deref().map(PathBuf::from)
}

#[cfg(test)]
async fn ensure_request_binding(
    node: &Arc<EmbeddedNode>,
    request: &AgentRequest,
    workspace: &IsolatedWorkspaceRecord,
    node_did: &str,
    authority: WorkspaceAuthority,
) -> Result<()> {
    let existing =
        load_workspace_bindings_for(node, &workspace.workspace_id, &workspace.owner_node_did)
            .await?;
    let mut candidate = new_binding(
        &workspace.workspace_id,
        &request.request_id,
        &request.doc_id,
        authority,
        node_did,
        optional_id(request.workspace_seal_hash.as_deref())
            .or(optional_id(workspace.seal_hash.as_deref())),
    );
    let response = graphql_with_transaction_retry(
        node,
        &isolated_workspace_record_query(&workspace.workspace_id, &workspace.owner_node_did),
        "test workspace physical identity",
    )
    .await?;
    candidate.workspace_doc_id = Some(
        response
            .data
            .as_ref()
            .and_then(|data| data["IsolatedWorkspace"].as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row["_docID"].as_str())
            .context("test workspace physical identity missing")?
            .to_owned(),
    );
    let release_previous = matches!(
        authority,
        WorkspaceAuthority::ReadWrite | WorkspaceAuthority::Integrate
    ) && previous_exclusive_is_stale(
        node,
        &workspace.workspace_id,
        &existing,
        &request.request_id,
        matches!(authority, WorkspaceAuthority::Integrate),
    )
    .await?;
    match admit_workspace_binding(
        &workspace.workspace_id,
        &workspace.lifecycle_state,
        optional_id(workspace.seal_hash.as_deref()),
        &existing,
        candidate,
        release_previous,
    )? {
        AdmitBinding::Reuse(_) => Ok(()),
        AdmitBinding::Create { binding, release } => {
            for released in release {
                persist_workspace_binding_doc(node, &request.agent_did, &released).await?;
            }
            persist_workspace_binding_doc(node, &request.agent_did, &binding).await
        }
    }
}

#[cfg(test)]
pub(super) async fn previous_exclusive_is_stale(
    node: &EmbeddedNode,
    workspace_id: &str,
    existing: &[WorkspaceBindingDoc],
    request_id: &str,
    integrate: bool,
) -> Result<bool> {
    let Some(active) = previous_exclusive_binding(workspace_id, existing, request_id, integrate)?
    else {
        return Ok(false);
    };
    Ok(!request_is_live(
        node,
        &active.request_doc_id,
        &active.request_id,
        workspace_id,
        &active.owner_node_did,
    )
    .await?)
}

#[cfg(test)]
pub(super) async fn previous_exclusive_is_stale_with_access(
    access: &crate::ConfigAccess,
    workspace_id: &str,
    existing: &[WorkspaceBindingDoc],
    request_id: &str,
    integrate: bool,
) -> Result<bool> {
    let Some(active) = previous_exclusive_binding(workspace_id, existing, request_id, integrate)?
    else {
        return Ok(false);
    };
    let query = request_liveness_query(&active.request_doc_id)?;
    let response = access.execute(&query).await?;
    let values = response
        .pointer("/data/AgentRequest")
        .context("workspace request query omitted rows")?;
    let mut rows: Vec<AgentRequestRow> = serde_json::from_value(values.clone())?;
    anyhow::ensure!(
        rows.len() == 1,
        "workspace binding request is unavailable or ambiguous"
    );
    Ok(!validate_request_liveness(
        rows.remove(0),
        &active.request_id,
        workspace_id,
        &active.owner_node_did,
    )?)
}

pub(super) fn previous_exclusive_binding<'a>(
    workspace_id: &str,
    existing: &'a [WorkspaceBindingDoc],
    request_id: &str,
    integrate: bool,
) -> Result<Option<&'a WorkspaceBindingDoc>> {
    let others: Vec<_> = existing
        .iter()
        .filter(|binding| {
            let exclusive = if integrate {
                binding.is_active_integrate()
            } else {
                binding.is_active_read_write()
            };
            exclusive && binding.request_id != request_id
        })
        .collect();
    if others.len() > 1 {
        let label = if integrate { "Integrate" } else { "ReadWrite" };
        anyhow::bail!(
            "multiple Active {label} bindings exist for workspace {workspace_id}; failing closed"
        );
    }
    Ok(others.into_iter().next())
}

fn request_lifecycle_is_live(lifecycle_state: Option<RequestLifecycleState>) -> bool {
    let Some(state) = lifecycle_state else {
        return false;
    };
    !state.is_terminal()
}

#[cfg(test)]
async fn request_is_live(
    node: &EmbeddedNode,
    request_doc_id: &str,
    request_id: &str,
    workspace_id: &str,
    node_did: &str,
) -> Result<bool> {
    let query = request_liveness_query(request_doc_id)?;
    let response =
        graphql_with_transaction_retry(node, &query, "load AgentRequest liveness").await?;
    let Some(row) = first_row::<AgentRequestRow>(&response, "AgentRequest")? else {
        bail!("workspace binding request is unavailable");
    };
    validate_request_liveness(row, request_id, workspace_id, node_did)
}

pub(super) fn request_liveness_query(request_doc_id: &str) -> Result<String> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "workspace binding lacks physical request ID"
    );
    let escaped = escape_graphql_string(request_doc_id);
    Ok(format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                request_id
                node_did
                lifecycle_state
                workspace_id workspace_owner_node_did
            }}
        }}"#
    ))
}

pub(super) fn validate_request_liveness(
    row: AgentRequestRow,
    request_id: &str,
    workspace_id: &str,
    node_did: &str,
) -> Result<bool> {
    anyhow::ensure!(
        row.request_id == request_id
            && row.workspace_id.as_deref() == Some(workspace_id)
            && row.workspace_owner_node_did.as_deref() == Some(node_did)
            && row
                .node_did
                .as_deref()
                .is_some_and(|did| !did.trim().is_empty()),
        "workspace binding physical request has different logical identity or workspace owner"
    );
    anyhow::ensure!(
        row.lifecycle_state.is_some(),
        "workspace binding request has no lifecycle state"
    );
    Ok(request_lifecycle_is_live(row.lifecycle_state))
}

#[cfg(test)]
pub(super) async fn load_workspace_bindings_for(
    node: &EmbeddedNode,
    workspace_id: &str,
    node_did: &str,
) -> Result<Vec<WorkspaceBindingDoc>> {
    let query = workspace_bindings_query(workspace_id, node_did);
    let response = graphql_with_transaction_retry(node, &query, "load WorkspaceBinding").await?;
    validate_workspace_bindings(
        rows::<WorkspaceBindingDoc>(&response, "WorkspaceBinding")?,
        workspace_id,
    )
}

pub(super) async fn load_workspace_bindings_for_access(
    access: &crate::ConfigAccess,
    workspace_id: &str,
    node_did: &str,
) -> Result<Vec<WorkspaceBindingDoc>> {
    let response = access
        .execute(&workspace_bindings_query(workspace_id, node_did))
        .await?;
    let rows = response
        .pointer("/data/WorkspaceBinding")
        .context("workspace binding query omitted rows")?;
    validate_workspace_bindings(serde_json::from_value(rows.clone())?, workspace_id)
}

pub(super) fn workspace_bindings_query(workspace_id: &str, node_did: &str) -> String {
    let owner = escape_graphql_string(node_did);
    let escaped = escape_graphql_string(workspace_id);
    format!(
        r#"{{
            WorkspaceBinding(
                filter: {{ workspace_id: {{ _eq: "{escaped}" }}, owner_node_did: {{ _eq: "{owner}" }} }}
            ) {{
                binding_id
                workspace_id
                workspace_doc_id
                request_id
                request_doc_id
                authority
                owner_node_did
                seal_hash
                lifecycle_state
            }}
        }}"#
    )
}

pub(super) fn workspace_bindings_for_parent_query(workspace_doc_id: &str) -> String {
    format!(
        r#"{{ WorkspaceBinding(filter: {{ workspace_doc_id: {{ _eq: "{}" }} }}) {{ binding_id workspace_id workspace_doc_id request_id request_doc_id authority owner_node_did seal_hash lifecycle_state }} }}"#,
        escape_graphql_string(workspace_doc_id)
    )
}

pub(super) fn validate_workspace_bindings(
    bindings: Vec<WorkspaceBindingDoc>,
    workspace_id: &str,
) -> Result<Vec<WorkspaceBindingDoc>> {
    anyhow::ensure!(
        bindings
            .iter()
            .all(|binding| binding.workspace_id == workspace_id
                && !binding.request_doc_id.trim().is_empty()
                && !binding.owner_node_did.trim().is_empty()),
        "workspace binding is missing its physical request or node identity"
    );
    Ok(bindings)
}

#[cfg(test)]
pub(super) async fn persist_workspace_binding_doc(
    node: &std::sync::Arc<EmbeddedNode>,
    actor_did: &str,
    doc: &WorkspaceBindingDoc,
) -> Result<()> {
    persist_workspace_binding_docs(node, actor_did, std::slice::from_ref(doc)).await
}

#[cfg(test)]
pub(super) async fn persist_workspace_binding_docs(
    node: &std::sync::Arc<EmbeddedNode>,
    actor_did: &str,
    docs: &[WorkspaceBindingDoc],
) -> Result<()> {
    let access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(actor_did.to_owned())?,
    );
    persist_workspace_binding_docs_with_access(&access, docs).await
}

#[cfg(test)]
pub(super) async fn persist_workspace_binding_docs_with_access(
    access: &crate::ConfigAccess,
    docs: &[WorkspaceBindingDoc],
) -> Result<()> {
    if docs.is_empty() {
        return Ok(());
    }
    let mutation = if docs.len() == 1 {
        workspace_binding_upsert_mutation(&docs[0])
    } else {
        workspace_bindings_upsert_mutation(docs)
    };
    access
        .write("workspace.upsert_binding", &mutation)
        .await
        .with_context(|| {
            format!(
                "persist WorkspaceBinding {}",
                docs.iter()
                    .map(|doc| doc.binding_id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })?;
    Ok(())
}

#[derive(Deserialize)]
pub(super) struct IsolatedWorkspaceRow {
    workspace_id: Option<String>,
    work_unit_id: Option<String>,
    caused_by_invocation_id: Option<String>,
    owner_node_did: Option<String>,
    writer_principal: Option<String>,
    integrator_principal: Option<String>,
    lifecycle_state: Option<String>,
    seal_hash: Option<String>,
    instruction_manifest: Option<String>,
}

#[derive(Deserialize)]
struct WorkspacePlacementRow {
    workspace_id: Option<String>,
    owner_node_did: Option<String>,
    host_path: Option<String>,
    observed_tree_hash: Option<String>,
}

pub(crate) fn isolated_workspace_record_query(workspace_id: &str, node_did: &str) -> String {
    let escaped = escape_graphql_string(workspace_id);
    let owner = escape_graphql_string(node_did);
    format!(
        r#"{{
            IsolatedWorkspace(
                filter: {{ workspace_id: {{ _eq: "{escaped}" }}, owner_node_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{
                _docID
                workspace_id
                work_unit_id
                caused_by_invocation_id
                owner_node_did
                writer_principal
                integrator_principal
                lifecycle_state
                seal_hash
                instruction_manifest
            }}
        }}"#
    )
}

pub(super) fn decode_isolated_workspace_record(
    row: IsolatedWorkspaceRow,
) -> Result<IsolatedWorkspaceRecord> {
    let workspace_id = optional_id(row.workspace_id.as_deref())
        .ok_or_else(|| anyhow!("IsolatedWorkspace is missing workspace_id"))?
        .to_string();
    let owner_node_did = optional_id(row.owner_node_did.as_deref())
        .ok_or_else(|| anyhow!("IsolatedWorkspace {workspace_id} is missing owner_node_did"))?
        .to_string();
    let lifecycle_state = optional_id(row.lifecycle_state.as_deref())
        .ok_or_else(|| anyhow!("IsolatedWorkspace {workspace_id} is missing lifecycle_state"))?
        .to_string();
    let writer_principal = optional_id(row.writer_principal.as_deref())
        .ok_or_else(|| anyhow!("IsolatedWorkspace {workspace_id} is missing writer_principal"))?
        .to_string();
    let integrator_principal = optional_id(row.integrator_principal.as_deref())
        .ok_or_else(|| anyhow!("IsolatedWorkspace {workspace_id} is missing integrator_principal"))?
        .to_string();
    Ok(IsolatedWorkspaceRecord {
        workspace_id,
        work_unit_id: row.work_unit_id,
        caused_by_invocation_id: row.caused_by_invocation_id,
        owner_node_did,
        writer_principal,
        integrator_principal,
        lifecycle_state,
        seal_hash: optional_id(row.seal_hash.as_deref()).map(str::to_string),
        instruction_manifest: optional_id(row.instruction_manifest.as_deref())
            .unwrap_or("{}")
            .to_string(),
    })
}

#[cfg(test)]
pub(crate) async fn load_isolated_workspace_record(
    node: &EmbeddedNode,
    workspace_id: &str,
    node_did: &str,
) -> Result<Option<IsolatedWorkspaceRecord>> {
    let query = isolated_workspace_record_query(workspace_id, node_did);
    let response = graphql_with_transaction_retry(node, &query, "load IsolatedWorkspace").await?;
    let mut found = rows::<IsolatedWorkspaceRow>(&response, "IsolatedWorkspace")?;
    anyhow::ensure!(found.len() <= 1, "ambiguous node-scoped workspace");
    found
        .pop()
        .map(decode_isolated_workspace_record)
        .transpose()
}

pub(crate) async fn load_isolated_workspace_record_with_access(
    access: &crate::ConfigAccess,
    workspace_id: &str,
    node_did: &str,
) -> Result<Option<IsolatedWorkspaceRecord>> {
    let response = access
        .execute(&isolated_workspace_record_query(workspace_id, node_did))
        .await?;
    let values = response
        .pointer("/data/IsolatedWorkspace")
        .context("workspace query omitted rows")?;
    let mut found: Vec<IsolatedWorkspaceRow> = serde_json::from_value(values.clone())?;
    anyhow::ensure!(found.len() <= 1, "ambiguous node-scoped workspace");
    found
        .pop()
        .map(decode_isolated_workspace_record)
        .transpose()
}

#[cfg(test)]
async fn load_workspace_placement(
    node: &EmbeddedNode,
    workspace_id: &str,
    node_did: &str,
) -> Result<Option<WorkspacePlacementRecord>> {
    let query = workspace_placement_query(workspace_id, node_did);
    let response = graphql_with_transaction_retry(node, &query, "load WorkspacePlacement").await?;
    decode_workspace_placement(rows::<WorkspacePlacementRow>(
        &response,
        "WorkspacePlacement",
    )?)
}

async fn load_workspace_placement_with_access(
    access: &crate::ConfigAccess,
    workspace_id: &str,
    node_did: &str,
) -> Result<Option<WorkspacePlacementRecord>> {
    let response = access
        .execute(&workspace_placement_query(workspace_id, node_did))
        .await?;
    let values = response
        .pointer("/data/WorkspacePlacement")
        .context("workspace placement query omitted rows")?;
    decode_workspace_placement(serde_json::from_value(values.clone())?)
}

fn workspace_placement_query(workspace_id: &str, node_did: &str) -> String {
    let escaped_workspace = escape_graphql_string(workspace_id);
    let escaped_owner = escape_graphql_string(node_did);
    format!(
        r#"{{
            WorkspacePlacement(
                filter: {{
                    workspace_id: {{ _eq: "{escaped_workspace}" }},
                    owner_node_did: {{ _eq: "{escaped_owner}" }}
                }},
                limit: 2
            ) {{
                workspace_id
                owner_node_did
                host_path
                observed_tree_hash
            }}
        }}"#
    )
}

fn decode_workspace_placement(
    mut found: Vec<WorkspacePlacementRow>,
) -> Result<Option<WorkspacePlacementRecord>> {
    anyhow::ensure!(
        found.len() <= 1,
        "ambiguous node-scoped workspace placement"
    );
    let Some(row) = found.pop() else {
        return Ok(None);
    };
    let workspace_id = optional_id(row.workspace_id.as_deref())
        .ok_or_else(|| anyhow!("WorkspacePlacement is missing workspace_id"))?
        .to_string();
    let owner_node_did = optional_id(row.owner_node_did.as_deref())
        .ok_or_else(|| anyhow!("WorkspacePlacement {workspace_id} is missing owner_node_did"))?
        .to_string();
    let host_path = optional_id(row.host_path.as_deref())
        .ok_or_else(|| anyhow!("WorkspacePlacement {workspace_id} is missing host_path"))?
        .to_string();
    Ok(Some(WorkspacePlacementRecord {
        workspace_id,
        owner_node_did,
        host_path,
        observed_tree_hash: optional_id(row.observed_tree_hash.as_deref()).map(str::to_string),
    }))
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
pub(crate) mod overlay_tests;
