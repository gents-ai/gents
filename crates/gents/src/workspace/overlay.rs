use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use defra_node::EmbeddedNode;
use gents_protocol::request_admission::RequestAdmissionError;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::config_client::ConfigClientError;
use crate::graphql::{
    escape_graphql_string, first_row, graphql_with_transaction_retry, rows, GraphqlResponseError,
};
use crate::tool_surface::{resolve_configured_tool_root, FileToolMode};
use crate::toolset::{workspace_write_sandbox_enforced, WorkspaceAuthority};
use crate::watcher::AgentRequest;

use super::binding::{admit_workspace_binding, new_binding, AdmitBinding, BindingError};
use super::documents::{
    workspace_binding_upsert_mutation, workspace_bindings_upsert_mutation, WorkspaceBindingDoc,
};

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error(transparent)]
    Admission(#[from] RequestAdmissionError),
    #[error("workspace owner node is missing")]
    MissingOwnerNode,
    #[error("node {principal_did} is not authorized for {authority} on workspace {workspace_id}")]
    PrincipalNotAuthorized {
        principal_did: String,
        authority: &'static str,
        workspace_id: String,
    },
    #[error("isolated workspace id {workspace_id} does not match request {request_id}")]
    WorkspaceIdMismatch {
        workspace_id: String,
        request_id: String,
    },
    #[error("workspace placement id {placement_id} does not match request {request_id}")]
    PlacementIdMismatch {
        placement_id: String,
        request_id: String,
    },
    #[error("isolated workspace {workspace_id} in state {lifecycle_state} is not bindable for authority {authority}")]
    NotBindable {
        workspace_id: String,
        lifecycle_state: String,
        authority: &'static str,
    },
    #[error("workspace and placement must match the signed workspace owner")]
    OwnerMismatch,
    #[error(
        "ReadWrite workspace binding requires an enforceable WorkspaceWrite sandbox on this host"
    )]
    WriteSandboxRequired,
    #[error("sealed workspace {workspace_id} is missing seal_hash")]
    MissingSealHash { workspace_id: String },
    #[error("sealed workspace {workspace_id} requires request.workspace_seal_hash")]
    MissingRequestSealHash { workspace_id: String },
    #[error("request workspace_seal_hash {request_hash} does not match workspace seal_hash {workspace_hash}")]
    RequestSealHashMismatch {
        request_hash: String,
        workspace_hash: String,
    },
    #[error("sealed workspace {workspace_id} is missing placement observed_tree_hash")]
    MissingObservedTreeHash { workspace_id: String },
    #[error("placement observed_tree_hash {observed} does not match workspace seal_hash {workspace_hash}")]
    ObservedTreeHashMismatch {
        observed: String,
        workspace_hash: String,
    },
    #[error("sealed workspace {workspace_id} requires live tree hash")]
    MissingLiveTreeHash { workspace_id: String },
    #[error("live tree hash {live} does not match workspace seal_hash {workspace_hash}")]
    LiveTreeHashMismatch {
        live: String,
        workspace_hash: String,
    },
    #[error("multiple Active {label} bindings exist for workspace {workspace_id}; failing closed")]
    MultipleActiveBindings {
        label: &'static str,
        workspace_id: String,
    },
    #[error("workspace binding lacks physical request ID")]
    MissingPhysicalRequestId,
    #[error(
        "workspace binding physical request has different logical identity or workspace owner"
    )]
    PhysicalRequestMismatch,
    #[error("workspace binding is missing its physical request or node identity")]
    BindingIdentityMissing,
    #[error("ambiguous node-scoped workspace")]
    AmbiguousWorkspace,
    #[error("workspace observation omitted IsolatedWorkspace")]
    ObservationOmittedWorkspace,
    #[error("IsolatedWorkspace is missing workspace_id")]
    WorkspaceMissingId,
    #[error("IsolatedWorkspace {workspace_id} is missing {field}")]
    WorkspaceMissingField {
        workspace_id: String,
        field: &'static str,
    },
    #[error("ambiguous node-scoped workspace placement")]
    AmbiguousPlacement,
    #[error("WorkspacePlacement is missing workspace_id")]
    PlacementMissingId,
    #[error("WorkspacePlacement {workspace_id} is missing {field}")]
    PlacementMissingField {
        workspace_id: String,
        field: &'static str,
    },
    #[error("workspace placement host_path must be absolute: {host_path}")]
    HostPathNotAbsolute { host_path: String },
    #[error("workspace placement host_path is not a directory: {}", path.display())]
    HostPathNotDirectory { path: std::path::PathBuf },
    #[error("canonicalizing workspace placement {}", path.display())]
    CanonicalizePlacement {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("persist WorkspaceBinding {binding_ids}")]
    PersistBinding {
        binding_ids: String,
        #[source]
        source: ConfigClientError,
    },
    #[error(transparent)]
    Config(#[from] ConfigClientError),
    #[error(transparent)]
    Rows(#[from] GraphqlResponseError),
    #[error(transparent)]
    Binding(#[from] BindingError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    ToolPolicy(#[from] gents_loop::tool_policy::ToolPolicyError),
    #[error(transparent)]
    RootAdmission(#[from] crate::tool_surface::RootAdmissionError),
    #[error(transparent)]
    Adapter(#[from] super::adapter::AdapterError),
    #[error(transparent)]
    Artifact(#[from] super::artifact::ArtifactError),
    #[error(transparent)]
    Runtime(Box<super::runtime::WorkspaceRuntimeError>),
    #[error("artifact workspace requires a seal")]
    ArtifactRequiresSeal,
    #[error("sealed writer lacks workspace")]
    SealedWriterLacksWorkspace,
    #[error("sealed writer workspace placement missing")]
    SealedWriterPlacementMissing,
    #[error("artifact_write requires an enforceable artifact sandbox on this host")]
    ArtifactSandboxRequired,
    #[error("artifact_write requires a sealed ReadOnly workspace binding")]
    ArtifactRequiresSealedReadOnly,
    #[error("resolve invocation cwd")]
    ResolveInvocationCwd {
        #[source]
        source: std::io::Error,
    },
    #[error("invocation cwd is not a directory")]
    InvocationCwdNotDirectory,
    #[error("workspace-bound request {workspace_id} is missing workspace_authority")]
    MissingWorkspaceAuthority { workspace_id: String },
    #[error("isolated workspace {workspace_id} not found")]
    IsolatedWorkspaceNotFound { workspace_id: String },
    #[error("workspace placement for {workspace_id} not found on this node")]
    PlacementNotFound { workspace_id: String },
    #[error("canonicalizing request cwd {}", path.display())]
    CanonicalizeRequestCwd {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("request cwd {} is not a directory", path.display())]
    RequestCwdNotDirectory { path: PathBuf },
    #[error("request cwd {} is not under workspace root {}", cwd.display(), root.display())]
    RequestCwdOutsideRoot { cwd: PathBuf, root: PathBuf },
    #[error("workspace placement {} escapes operator tool root {}", path.display(), ceiling.display())]
    PlacementEscapesToolRoot { path: PathBuf, ceiling: PathBuf },
}

type Result<T, E = OverlayError> = std::result::Result<T, E>;

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
        .ok_or(OverlayError::MissingOwnerNode)
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
        return Err(OverlayError::PrincipalNotAuthorized {
            principal_did: principal_did.to_owned(),
            authority: authority.as_str(),
            workspace_id: workspace.workspace_id.clone(),
        });
    }
    Ok(())
}

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
                    .ok_or(OverlayError::ArtifactRequiresSeal)?,
            )
            .await?,
        );
    }
    Ok(Some(overlay))
}

pub(crate) async fn validate_request_workspace_input(
    node: &Arc<EmbeddedNode>,
    request: &AgentRequest,
    artifact_requested: bool,
) -> Result<()> {
    let operator_root = process_operator_tool_root();
    if super::runtime::writer_request_already_sealed(node, request)
        .await
        .map_err(|error| OverlayError::Runtime(Box::new(error)))?
    {
        let workspace_id = request
            .workspace_id
            .as_deref()
            .ok_or(OverlayError::SealedWriterLacksWorkspace)?;
        let placement =
            load_workspace_placement(node, workspace_id, request_workspace_owner(request)?)
                .await?
                .ok_or(OverlayError::SealedWriterPlacementMissing)?;
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
    if artifact_requested && !sandbox_enforced {
        return Err(OverlayError::ArtifactSandboxRequired);
    }
    let Some(workspace_id) = optional_id(request.workspace_id.as_deref()) else {
        if artifact_requested {
            return Err(OverlayError::ArtifactRequiresSealedReadOnly);
        }
        if let Some(cwd) = request_workspace_cwd(request) {
            let cwd = std::fs::canonicalize(&cwd)
                .map_err(|source| OverlayError::ResolveInvocationCwd { source })?;
            if !cwd.is_dir() {
                return Err(OverlayError::InvocationCwdNotDirectory);
            }
            require_under_ceiling(&cwd, operator_tool_root)?;
        }
        return Ok(None);
    };
    let authority = match request.workspace_authority.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => WorkspaceAuthority::parse(value)?,
        _ => {
            return Err(OverlayError::MissingWorkspaceAuthority {
                workspace_id: workspace_id.to_owned(),
            })
        }
    };

    let workspace =
        load_isolated_workspace_record(node, workspace_id, request_workspace_owner(request)?)
            .await?
            .ok_or_else(|| OverlayError::IsolatedWorkspaceNotFound {
                workspace_id: workspace_id.to_owned(),
            })?;
    require_workspace_principal(&workspace, &request.node_did, authority)?;
    let node_did = request_workspace_owner(request)?;
    let placement = load_workspace_placement(node, workspace_id, node_did)
        .await?
        .ok_or_else(|| OverlayError::PlacementNotFound {
            workspace_id: workspace_id.to_owned(),
        })?;
    let request_cwd = request_workspace_cwd(request);
    let sealed = crate::toolset::normalize_workspace_lifecycle_state(&workspace.lifecycle_state)
        == Some("sealed");
    // Reject before binding or provider dispatch: a missing grant must never
    // let an artifact-selected request fall through legacy CLI/LSP paths.
    if artifact_requested && (!sealed || authority != WorkspaceAuthority::ReadOnly) {
        return Err(OverlayError::ArtifactRequiresSealedReadOnly);
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

fn validate_bound_workspace(
    workspace: &IsolatedWorkspaceRecord,
    placement: &WorkspacePlacementRecord,
    input: &WorkspaceBindInput<'_>,
) -> Result<()> {
    if workspace.workspace_id != input.workspace_id {
        return Err(OverlayError::WorkspaceIdMismatch {
            workspace_id: workspace.workspace_id.clone(),
            request_id: input.workspace_id.to_owned(),
        });
    }
    if placement.workspace_id != input.workspace_id {
        return Err(OverlayError::PlacementIdMismatch {
            placement_id: placement.workspace_id.clone(),
            request_id: input.workspace_id.to_owned(),
        });
    }
    if !input
        .authority
        .bindable_lifecycle_state(&workspace.lifecycle_state)
    {
        return Err(OverlayError::NotBindable {
            workspace_id: input.workspace_id.to_owned(),
            lifecycle_state: workspace.lifecycle_state.clone(),
            authority: input.authority.as_str(),
        });
    }
    if workspace.owner_node_did != input.node_did || placement.owner_node_did != input.node_did {
        return Err(OverlayError::OwnerMismatch);
    }

    if matches!(input.authority, WorkspaceAuthority::ReadWrite)
        && !input.workspace_write_sandbox_enforced
    {
        return Err(OverlayError::WriteSandboxRequired);
    }

    if crate::toolset::normalize_workspace_lifecycle_state(&workspace.lifecycle_state)
        == Some("sealed")
    {
        let workspace_hash = optional_id(workspace.seal_hash.as_deref()).ok_or_else(|| {
            OverlayError::MissingSealHash {
                workspace_id: input.workspace_id.to_owned(),
            }
        })?;
        let request_hash = input
            .seal_hash
            .ok_or_else(|| OverlayError::MissingRequestSealHash {
                workspace_id: input.workspace_id.to_owned(),
            })?;
        if request_hash != workspace_hash {
            return Err(OverlayError::RequestSealHashMismatch {
                request_hash: request_hash.to_owned(),
                workspace_hash: workspace_hash.to_owned(),
            });
        }
        let observed = optional_id(placement.observed_tree_hash.as_deref()).ok_or_else(|| {
            OverlayError::MissingObservedTreeHash {
                workspace_id: input.workspace_id.to_owned(),
            }
        })?;
        if observed != workspace_hash {
            return Err(OverlayError::ObservedTreeHashMismatch {
                observed: observed.to_owned(),
                workspace_hash: workspace_hash.to_owned(),
            });
        }
        let live = input
            .live_tree_hash
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| OverlayError::MissingLiveTreeHash {
                workspace_id: input.workspace_id.to_owned(),
            })?;
        if live != workspace_hash {
            return Err(OverlayError::LiveTreeHashMismatch {
                live: live.to_owned(),
                workspace_hash: workspace_hash.to_owned(),
            });
        }
    }
    Ok(())
}

pub(crate) fn bind_workspace_overlay(
    workspace: &IsolatedWorkspaceRecord,
    placement: &WorkspacePlacementRecord,
    input: WorkspaceBindInput<'_>,
) -> Result<WorkspaceOverlay> {
    validate_bound_workspace(workspace, placement, &input)?;

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
    let canonical =
        std::fs::canonicalize(cwd).map_err(|source| OverlayError::CanonicalizeRequestCwd {
            path: cwd.to_path_buf(),
            source,
        })?;
    if !canonical.is_dir() {
        return Err(OverlayError::RequestCwdNotDirectory {
            path: canonical.clone(),
        });
    }
    let admission = crate::tool_surface::resolve_admitted_tool_root(&canonical, [root])?;
    if admission.admitted().is_none() {
        return Err(OverlayError::RequestCwdOutsideRoot {
            cwd: canonical.clone(),
            root: root.to_path_buf(),
        });
    }
    Ok(canonical)
}

fn canonicalize_placement_path(host_path: &str) -> Result<PathBuf> {
    let path = Path::new(host_path.trim());
    if !path.is_absolute() {
        return Err(OverlayError::HostPathNotAbsolute {
            host_path: host_path.to_owned(),
        });
    }
    if !path.is_dir() {
        return Err(OverlayError::HostPathNotDirectory {
            path: path.to_owned(),
        });
    }
    std::fs::canonicalize(path).map_err(|source| OverlayError::CanonicalizePlacement {
        path: path.to_owned(),
        source,
    })
}

pub(crate) fn require_under_ceiling(path: &Path, operator_tool_root: Option<&Path>) -> Result<()> {
    let operator = operator_tool_root
        .map(resolve_configured_tool_root)
        .transpose()?;
    if let Some(ceiling) = operator.as_deref() {
        let admission = crate::tool_surface::resolve_admitted_tool_root(path, [ceiling])?;
        if admission.admitted().is_none() {
            return Err(OverlayError::PlacementEscapesToolRoot {
                path: path.to_path_buf(),
                ceiling: ceiling.to_path_buf(),
            });
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
    node: &Arc<EmbeddedNode>,
    request: &AgentRequest,
    workspace: &IsolatedWorkspaceRecord,
    node_did: &str,
    authority: WorkspaceAuthority,
) -> Result<()> {
    let existing =
        load_workspace_bindings_for(node, &workspace.workspace_id, &workspace.owner_node_did)
            .await?;
    let candidate = new_binding(
        &workspace.workspace_id,
        &request.request_id,
        &request.doc_id,
        authority,
        node_did,
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
        return Err(OverlayError::MultipleActiveBindings {
            label,
            workspace_id: workspace_id.to_owned(),
        });
    }
    let Some(active) = others.into_iter().next() else {
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
    node_did: &str,
) -> Result<bool> {
    if request_doc_id.trim().is_empty() {
        return Err(OverlayError::MissingPhysicalRequestId);
    }
    let escaped = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                request_id
                node_did
                lifecycle_state
                workspace_id workspace_owner_node_did
            }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "load AgentRequest liveness").await?;
    let Some(row) = first_row::<AgentRequestRow>(&response, "AgentRequest")? else {
        return Ok(false);
    };
    if !(row.request_id == request_id
        && row.workspace_id.as_deref() == Some(workspace_id)
        && row.workspace_owner_node_did.as_deref() == Some(node_did)
        && row
            .node_did
            .as_deref()
            .is_some_and(|did| !did.trim().is_empty()))
    {
        return Err(OverlayError::PhysicalRequestMismatch);
    }
    Ok(request_lifecycle_is_live(row.lifecycle_state))
}

pub(super) async fn load_workspace_bindings_for(
    node: &EmbeddedNode,
    workspace_id: &str,
    node_did: &str,
) -> Result<Vec<WorkspaceBindingDoc>> {
    let owner = escape_graphql_string(node_did);
    let escaped = escape_graphql_string(workspace_id);
    let query = format!(
        r#"{{
            WorkspaceBinding(
                filter: {{ workspace_id: {{ _eq: "{escaped}" }}, owner_node_did: {{ _eq: "{owner}" }} }}
            ) {{
                binding_id
                workspace_id
                request_id
                request_doc_id
                authority
                owner_node_did
                seal_hash
                lifecycle_state
            }}
        }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "load WorkspaceBinding").await?;
    let bindings = rows::<WorkspaceBindingDoc>(&response, "WorkspaceBinding")?;
    if !bindings.iter().all(|binding| {
        binding.workspace_id == workspace_id
            && !binding.request_doc_id.trim().is_empty()
            && !binding.owner_node_did.trim().is_empty()
    }) {
        return Err(OverlayError::BindingIdentityMissing);
    }
    Ok(bindings)
}

pub(super) async fn persist_workspace_binding_doc(
    node: &std::sync::Arc<EmbeddedNode>,
    doc: &WorkspaceBindingDoc,
) -> Result<()> {
    persist_workspace_binding_docs(node, std::slice::from_ref(doc)).await
}

pub(super) async fn persist_workspace_binding_docs(
    node: &std::sync::Arc<EmbeddedNode>,
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
        .map_err(|source| OverlayError::PersistBinding {
            binding_ids: docs
                .iter()
                .map(|doc| doc.binding_id.as_str())
                .collect::<Vec<_>>()
                .join(","),
            source,
        })?;
    Ok(())
}

#[derive(Deserialize)]
struct IsolatedWorkspaceRow {
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

pub(crate) fn decode_isolated_workspace_record_response(
    response: &serde_json::Value,
) -> Result<Option<IsolatedWorkspaceRecord>> {
    let rows = response
        .pointer("/data/IsolatedWorkspace")
        .and_then(serde_json::Value::as_array)
        .ok_or(OverlayError::ObservationOmittedWorkspace)?;
    if rows.len() > 1 {
        return Err(OverlayError::AmbiguousWorkspace);
    }
    rows.first()
        .cloned()
        .map(serde_json::from_value::<IsolatedWorkspaceRow>)
        .transpose()?
        .map(decode_isolated_workspace_record)
        .transpose()
}

fn decode_isolated_workspace_record(row: IsolatedWorkspaceRow) -> Result<IsolatedWorkspaceRecord> {
    let workspace_id = optional_id(row.workspace_id.as_deref())
        .ok_or(OverlayError::WorkspaceMissingId)?
        .to_string();
    let missing = |field: &'static str| OverlayError::WorkspaceMissingField {
        workspace_id: workspace_id.clone(),
        field,
    };
    let owner_node_did = optional_id(row.owner_node_did.as_deref())
        .ok_or_else(|| missing("owner_node_did"))?
        .to_string();
    let lifecycle_state = optional_id(row.lifecycle_state.as_deref())
        .ok_or_else(|| missing("lifecycle_state"))?
        .to_string();
    let writer_principal = optional_id(row.writer_principal.as_deref())
        .ok_or_else(|| missing("writer_principal"))?
        .to_string();
    let integrator_principal = optional_id(row.integrator_principal.as_deref())
        .ok_or_else(|| missing("integrator_principal"))?
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

pub(crate) async fn load_isolated_workspace_record(
    node: &EmbeddedNode,
    workspace_id: &str,
    node_did: &str,
) -> Result<Option<IsolatedWorkspaceRecord>> {
    let query = isolated_workspace_record_query(workspace_id, node_did);
    let response = graphql_with_transaction_retry(node, &query, "load IsolatedWorkspace").await?;
    let mut found = rows::<IsolatedWorkspaceRow>(&response, "IsolatedWorkspace")?;
    if found.len() > 1 {
        return Err(OverlayError::AmbiguousWorkspace);
    }
    found
        .pop()
        .map(decode_isolated_workspace_record)
        .transpose()
}

async fn load_workspace_placement(
    node: &EmbeddedNode,
    workspace_id: &str,
    node_did: &str,
) -> Result<Option<WorkspacePlacementRecord>> {
    let escaped_workspace = escape_graphql_string(workspace_id);
    let escaped_owner = escape_graphql_string(node_did);
    let query = format!(
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
    );
    let response = graphql_with_transaction_retry(node, &query, "load WorkspacePlacement").await?;
    let mut found = rows::<WorkspacePlacementRow>(&response, "WorkspacePlacement")?;
    if found.len() > 1 {
        return Err(OverlayError::AmbiguousPlacement);
    }
    let Some(row) = found.pop() else {
        return Ok(None);
    };
    let workspace_id = optional_id(row.workspace_id.as_deref())
        .ok_or(OverlayError::PlacementMissingId)?
        .to_string();
    let missing = |field: &'static str| OverlayError::PlacementMissingField {
        workspace_id: workspace_id.clone(),
        field,
    };
    let owner_node_did = optional_id(row.owner_node_did.as_deref())
        .ok_or_else(|| missing("owner_node_did"))?
        .to_string();
    let host_path = optional_id(row.host_path.as_deref())
        .ok_or_else(|| missing("host_path"))?
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
