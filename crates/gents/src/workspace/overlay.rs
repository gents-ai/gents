//! Isolated-workspace overlay: load identity + local placement, bind the
//! request-scoped tool root, and fail closed when the authority cannot run.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::graphql::{escape_graphql_string, first_row, graphql_with_transaction_retry, rows};
use crate::tool_surface::{resolve_configured_tool_root, FileToolMode};
use crate::toolset::{workspace_write_sandbox_enforced, WorkspaceAuthority};
use crate::watcher::AgentRequest;

use super::binding::{admit_workspace_binding, new_binding, AdmitBinding};
use super::documents::{
    workspace_binding_upsert_mutation, workspace_bindings_upsert_mutation, WorkspaceBindingDoc,
};

static PROCESS_OPERATOR_TOOL_ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Process `--tool-root` used by spawn provision, callback, and overlay.
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
    pub owner_agent_did: String,
    pub writer_principal: String,
    pub integrator_principal: String,
    pub lifecycle_state: String,
    pub seal_hash: Option<String>,
    pub instruction_manifest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspacePlacementRecord {
    pub workspace_id: String,
    pub owner_agent_did: String,
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

/// Bound overlays always yield `Some`, including empty `{}` manifests.
/// Unbound requests (`None`) fall through to the live cwd→tool-root walk.
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
    pub agent_did: &'a str,
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

/// Signed reference scope; admission authenticates where the tuple came from.
pub(crate) fn request_workspace_owner(request: &AgentRequest) -> Result<&str> {
    gents_protocol::request_admission::validate_workspace_reference(
        request.workspace_id.as_deref(),
        request.workspace_owner_agent_did.as_deref(),
        request.workspace_authority.as_deref(),
        request.workspace_seal_hash.as_deref(),
    )?;
    request
        .workspace_owner_agent_did
        .as_deref()
        .context("workspace owner principal is missing")
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
            "principal {principal_did} is not authorized for {} on workspace {}",
            authority.as_str(),
            workspace.workspace_id
        );
    }
    Ok(())
}

/// Ordinary unbound requests stay on the behavior tool root. Artifact execution
/// requires a valid sealed binding before the request can dispatch any tool.
#[inline(never)]
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
        &workspace.owner_agent_did,
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
                &workspace.owner_agent_did,
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

/// Admission uses the same workspace owner without creating a binding or grant.
pub(crate) async fn validate_request_workspace_input(
    node: &EmbeddedNode,
    request: &AgentRequest,
    artifact_requested: bool,
) -> Result<()> {
    let operator_root = process_operator_tool_root();
    // A sealed writer may only complete its interrupted terminal flush. The
    // daemon checks this same exact receipt owner before considering any tools.
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
        request,
        artifact_requested,
        operator_root.as_deref(),
        workspace_write_sandbox_enforced(),
    )
    .await?;
    Ok(())
}

async fn load_request_workspace_overlay(
    node: &EmbeddedNode,
    request: &AgentRequest,
    artifact_requested: bool,
    operator_tool_root: Option<&Path>,
    sandbox_enforced: bool,
) -> Result<Option<(IsolatedWorkspaceRecord, WorkspaceOverlay)>> {
    // Enforce the model's host eligibility before database reads, binding,
    // grant allocation or provider dispatch, not after a wasted model turn.
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

    let workspace =
        load_isolated_workspace_record(node, workspace_id, request_workspace_owner(request)?)
            .await?
            .ok_or_else(|| anyhow!("isolated workspace {workspace_id} not found"))?;
    require_workspace_principal(&workspace, &request.agent_did, authority)?;
    let agent_did = request_workspace_owner(request)?;
    let placement = load_workspace_placement(node, workspace_id, agent_did)
        .await?
        .ok_or_else(|| {
            anyhow!("workspace placement for {workspace_id} not found on this principal")
        })?;
    let request_cwd = request_workspace_cwd(request);
    let sealed = crate::toolset::normalize_workspace_lifecycle_state(&workspace.lifecycle_state)
        == Some("sealed");
    // Reject before binding or provider dispatch: absence of a grant must never
    // let an artifact-selected request fall through legacy CLI/LSP launch paths.
    if artifact_requested && (!sealed || authority != WorkspaceAuthority::ReadOnly) {
        bail!("artifact_write requires a sealed ReadOnly workspace binding");
    }
    let live_tree_hash = if sealed {
        Some(super::adapter::working_tree_hash(Path::new(
            &placement.host_path,
        ))?)
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
            agent_did,
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
        workspace.owner_agent_did == input.agent_did
            && placement.owner_agent_did == input.agent_did,
        "workspace and placement must match the signed workspace owner"
    );

    if matches!(input.authority, WorkspaceAuthority::ReadWrite)
        && !input.workspace_write_sandbox_enforced
    {
        bail!(
            "ReadWrite workspace binding requires an enforceable WorkspaceWrite sandbox on this host"
        );
    }

    if crate::toolset::normalize_workspace_lifecycle_state(&workspace.lifecycle_state)
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
        canonical.is_dir() && canonical.starts_with(root),
        "request cwd {} is not a directory under workspace root {}",
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
        if !path.starts_with(ceiling) {
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

async fn ensure_request_binding(
    node: &EmbeddedNode,
    request: &AgentRequest,
    workspace: &IsolatedWorkspaceRecord,
    agent_did: &str,
    authority: WorkspaceAuthority,
) -> Result<()> {
    let existing =
        load_workspace_bindings_for(node, &workspace.workspace_id, &workspace.owner_agent_did)
            .await?;
    let candidate = new_binding(
        &workspace.workspace_id,
        &request.request_id,
        &request.doc_id,
        authority,
        agent_did,
        optional_id(request.workspace_seal_hash.as_deref())
            .or(optional_id(workspace.seal_hash.as_deref())),
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
                persist_workspace_binding_doc(node, &released).await?;
            }
            persist_workspace_binding_doc(node, &binding).await
        }
    }
}

pub(super) async fn previous_exclusive_is_stale(
    node: &EmbeddedNode,
    workspace_id: &str,
    existing: &[WorkspaceBindingDoc],
    request_id: &str,
    integrate: bool,
) -> Result<bool> {
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
    let Some(active) = others.into_iter().next() else {
        return Ok(false);
    };
    Ok(!request_is_live(
        node,
        &active.request_doc_id,
        &active.request_id,
        workspace_id,
        &active.owner_agent_did,
    )
    .await?)
}

fn request_lifecycle_is_live(lifecycle_state: Option<RequestLifecycleState>) -> bool {
    let Some(state) = lifecycle_state else {
        return false;
    };
    !state.is_terminal()
}

async fn request_is_live(
    node: &EmbeddedNode,
    request_doc_id: &str,
    request_id: &str,
    workspace_id: &str,
    agent_did: &str,
) -> Result<bool> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "workspace binding lacks physical request ID"
    );
    let escaped = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                request_id
                agent_did
                lifecycle_state
                workspace_id workspace_owner_agent_did
            }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "load AgentRequest liveness").await?;
    let Some(row) = first_row::<AgentRequestRow>(&response, "AgentRequest")? else {
        return Ok(false);
    };
    anyhow::ensure!(
        row.request_id == request_id
            && row.workspace_id.as_deref() == Some(workspace_id)
            && row.workspace_owner_agent_did.as_deref() == Some(agent_did)
            && row
                .agent_did
                .as_deref()
                .is_some_and(|did| !did.trim().is_empty()),
        "workspace binding physical request has different logical identity or workspace owner"
    );
    Ok(request_lifecycle_is_live(row.lifecycle_state))
}

pub(super) async fn load_workspace_bindings_for(
    node: &EmbeddedNode,
    workspace_id: &str,
    agent_did: &str,
) -> Result<Vec<WorkspaceBindingDoc>> {
    let owner = escape_graphql_string(agent_did);
    let escaped = escape_graphql_string(workspace_id);
    let query = format!(
        r#"{{
            WorkspaceBinding(
                filter: {{ workspace_id: {{ _eq: "{escaped}" }}, owner_agent_did: {{ _eq: "{owner}" }} }}
            ) {{
                binding_id
                workspace_id
                request_id
                request_doc_id
                authority
                owner_agent_did
                seal_hash
                lifecycle_state
            }}
        }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "load WorkspaceBinding").await?;
    let bindings = rows::<WorkspaceBindingDoc>(&response, "WorkspaceBinding")?;
    anyhow::ensure!(
        bindings
            .iter()
            .all(|binding| binding.workspace_id == workspace_id
                && !binding.request_doc_id.trim().is_empty()
                && !binding.owner_agent_did.trim().is_empty()),
        "workspace binding is missing its physical request or principal identity"
    );
    Ok(bindings)
}

pub(super) async fn persist_workspace_binding_doc(
    node: &EmbeddedNode,
    doc: &WorkspaceBindingDoc,
) -> Result<()> {
    persist_workspace_binding_docs(node, std::slice::from_ref(doc)).await
}

pub(super) async fn persist_workspace_binding_docs(
    node: &EmbeddedNode,
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
    crate::config_client::ConfigAccess::write_local(node, "workspace.upsert_binding", &mutation)
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
struct IsolatedWorkspaceRow {
    workspace_id: Option<String>,
    owner_agent_did: Option<String>,
    writer_principal: Option<String>,
    integrator_principal: Option<String>,
    lifecycle_state: Option<String>,
    seal_hash: Option<String>,
    instruction_manifest: Option<String>,
}

#[derive(Deserialize)]
struct WorkspacePlacementRow {
    workspace_id: Option<String>,
    owner_agent_did: Option<String>,
    host_path: Option<String>,
    observed_tree_hash: Option<String>,
}

pub(crate) fn isolated_workspace_record_query(workspace_id: &str, agent_did: &str) -> String {
    let escaped = escape_graphql_string(workspace_id);
    let owner = escape_graphql_string(agent_did);
    format!(
        r#"{{
            IsolatedWorkspace(
                filter: {{ workspace_id: {{ _eq: "{escaped}" }}, owner_agent_did: {{ _eq: "{owner}" }} }},
                limit: 2
            ) {{
                workspace_id
                owner_agent_did
                writer_principal
                integrator_principal
                lifecycle_state
                seal_hash
                instruction_manifest
            }}
        }}"#
    )
}

pub(crate) fn decode_isolated_workspace_record_response(
    response: &serde_json::Value,
) -> Result<Option<IsolatedWorkspaceRecord>> {
    let rows = response
        .pointer("/data/IsolatedWorkspace")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("workspace observation omitted IsolatedWorkspace"))?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous principal-scoped workspace");
    rows.first()
        .cloned()
        .map(serde_json::from_value::<IsolatedWorkspaceRow>)
        .transpose()?
        .map(decode_isolated_workspace_record)
        .transpose()
}

fn decode_isolated_workspace_record(row: IsolatedWorkspaceRow) -> Result<IsolatedWorkspaceRecord> {
    let workspace_id = optional_id(row.workspace_id.as_deref())
        .ok_or_else(|| anyhow!("IsolatedWorkspace is missing workspace_id"))?
        .to_string();
    let owner_agent_did = optional_id(row.owner_agent_did.as_deref())
        .ok_or_else(|| anyhow!("IsolatedWorkspace {workspace_id} is missing owner_agent_did"))?
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
        owner_agent_did,
        writer_principal,
        integrator_principal,
        lifecycle_state,
        seal_hash: optional_id(row.seal_hash.as_deref()).map(str::to_string),
        instruction_manifest: optional_id(row.instruction_manifest.as_deref())
            .unwrap_or("{}")
            .to_string(),
    })
}

pub(crate) async fn load_isolated_workspace_record(
    node: &EmbeddedNode,
    workspace_id: &str,
    agent_did: &str,
) -> Result<Option<IsolatedWorkspaceRecord>> {
    let query = isolated_workspace_record_query(workspace_id, agent_did);
    let response = graphql_with_transaction_retry(node, &query, "load IsolatedWorkspace").await?;
    let mut found = rows::<IsolatedWorkspaceRow>(&response, "IsolatedWorkspace")?;
    anyhow::ensure!(found.len() <= 1, "ambiguous principal-scoped workspace");
    found
        .pop()
        .map(decode_isolated_workspace_record)
        .transpose()
}

async fn load_workspace_placement(
    node: &EmbeddedNode,
    workspace_id: &str,
    agent_did: &str,
) -> Result<Option<WorkspacePlacementRecord>> {
    let escaped_workspace = escape_graphql_string(workspace_id);
    let escaped_owner = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            WorkspacePlacement(
                filter: {{
                    workspace_id: {{ _eq: "{escaped_workspace}" }},
                    owner_agent_did: {{ _eq: "{escaped_owner}" }}
                }},
                limit: 2
            ) {{
                workspace_id
                owner_agent_did
                host_path
                observed_tree_hash
            }}
        }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "load WorkspacePlacement").await?;
    let mut found = rows::<WorkspacePlacementRow>(&response, "WorkspacePlacement")?;
    anyhow::ensure!(
        found.len() <= 1,
        "ambiguous principal-scoped workspace placement"
    );
    let Some(row) = found.pop() else {
        return Ok(None);
    };
    let workspace_id = optional_id(row.workspace_id.as_deref())
        .ok_or_else(|| anyhow!("WorkspacePlacement is missing workspace_id"))?
        .to_string();
    let owner_agent_did = optional_id(row.owner_agent_did.as_deref())
        .ok_or_else(|| anyhow!("WorkspacePlacement {workspace_id} is missing owner_agent_did"))?
        .to_string();
    let host_path = optional_id(row.host_path.as_deref())
        .ok_or_else(|| anyhow!("WorkspacePlacement {workspace_id} is missing host_path"))?
        .to_string();
    Ok(Some(WorkspacePlacementRecord {
        workspace_id,
        owner_agent_did,
        host_path,
        observed_tree_hash: optional_id(row.observed_tree_hash.as_deref()).map(str::to_string),
    }))
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
pub(crate) mod overlay_tests;
