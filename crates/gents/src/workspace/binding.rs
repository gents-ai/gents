use super::documents::{
    binding_id_for, WorkspaceBindingDoc, BINDING_ACTIVE, BINDING_RELEASED, LIFECYCLE_SEALED,
};
use crate::toolset::{normalize_workspace_lifecycle_state, WorkspaceAuthority};
use gents_loop::tool_policy::ToolPolicyError;

#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    #[error("workspace bindings must share their node owner")]
    OwnerMismatch,
    #[error(transparent)]
    Authority(#[from] ToolPolicyError),
    #[error("isolated workspace {workspace_id} in state {workspace_state} is not bindable for authority {authority}")]
    NotBindable {
        workspace_id: String,
        workspace_state: String,
        authority: &'static str,
    },
    #[error("binding workspace_id {binding_workspace_id} does not match {workspace_id}")]
    WorkspaceMismatch {
        binding_workspace_id: String,
        workspace_id: String,
    },
    #[error("sealed workspace {workspace_id} is missing seal_hash")]
    MissingSealHash { workspace_id: String },
    #[error("binding seal_hash {hash} does not match workspace seal_hash {workspace_hash}")]
    SealHashMismatch {
        hash: String,
        workspace_hash: String,
    },
    #[error("multiple Active {label} bindings exist for workspace {workspace_id}; failing closed")]
    MultipleActive {
        label: &'static str,
        workspace_id: String,
    },
    #[error("Active {authority} binding for request {request_id} points at request_doc_id {existing_request_doc_id}, not {request_doc_id}")]
    RequestDocMismatch {
        authority: &'static str,
        request_id: String,
        existing_request_doc_id: String,
        request_doc_id: String,
    },
    #[error("unique Active {label} binding already exists for workspace {workspace_id}")]
    UniqueActiveExists {
        label: &'static str,
        workspace_id: String,
    },
}

type Result<T, E = BindingError> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitBinding {
    Reuse(WorkspaceBindingDoc),
    Create {
        binding: WorkspaceBindingDoc,
        release: Vec<WorkspaceBindingDoc>,
    },
}

pub fn admit_workspace_binding(
    workspace_id: &str,
    workspace_state: &str,
    workspace_seal_hash: Option<&str>,
    existing: &[WorkspaceBindingDoc],
    candidate: WorkspaceBindingDoc,
    release_previous: bool,
) -> Result<AdmitBinding> {
    if candidate.owner_node_did.trim().is_empty()
        || !existing
            .iter()
            .filter(|binding| binding.workspace_id == workspace_id)
            .all(|binding| binding.owner_node_did == candidate.owner_node_did)
    {
        return Err(BindingError::OwnerMismatch);
    }
    let authority = WorkspaceAuthority::parse(&candidate.authority)?;
    if !authority.bindable_lifecycle_state(workspace_state) {
        return Err(BindingError::NotBindable {
            workspace_id: workspace_id.to_owned(),
            workspace_state: workspace_state.to_owned(),
            authority: authority.as_str(),
        });
    }
    if candidate.workspace_id != workspace_id {
        return Err(BindingError::WorkspaceMismatch {
            binding_workspace_id: candidate.workspace_id.clone(),
            workspace_id: workspace_id.to_owned(),
        });
    }

    let mut candidate = candidate;
    if normalize_workspace_lifecycle_state(workspace_state) == Some(LIFECYCLE_SEALED) {
        let workspace_hash = workspace_seal_hash
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| BindingError::MissingSealHash {
                workspace_id: workspace_id.to_owned(),
            })?;
        match candidate
            .seal_hash
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            None => candidate.seal_hash = Some(workspace_hash.to_string()),
            Some(hash) if hash == workspace_hash => {}
            Some(hash) => {
                return Err(BindingError::SealHashMismatch {
                    hash: hash.to_owned(),
                    workspace_hash: workspace_hash.to_owned(),
                })
            }
        }
    }

    if matches!(
        authority,
        WorkspaceAuthority::ReadWrite | WorkspaceAuthority::Integrate
    ) {
        let exclusive = |binding: &WorkspaceBindingDoc| {
            binding.workspace_id == workspace_id
                && match authority {
                    WorkspaceAuthority::ReadWrite => binding.is_active_read_write(),
                    WorkspaceAuthority::Integrate => binding.is_active_integrate(),
                    WorkspaceAuthority::ReadOnly => false,
                }
        };
        let label = match authority {
            WorkspaceAuthority::ReadWrite => "ReadWrite",
            WorkspaceAuthority::Integrate => "Integrate",
            WorkspaceAuthority::ReadOnly => "ReadOnly",
        };
        let active: Vec<_> = existing.iter().filter(|b| exclusive(b)).cloned().collect();
        let others: Vec<_> = active
            .iter()
            .filter(|binding| binding.request_id != candidate.request_id)
            .cloned()
            .collect();
        if others.len() > 1 {
            return Err(BindingError::MultipleActive {
                label,
                workspace_id: workspace_id.to_owned(),
            });
        }
        if let Some(existing_active) = active
            .iter()
            .find(|binding| binding.request_id == candidate.request_id)
        {
            if existing_active.request_doc_id != candidate.request_doc_id {
                return Err(BindingError::RequestDocMismatch {
                    authority: "ReadWrite",
                    request_id: candidate.request_id.clone(),
                    existing_request_doc_id: existing_active.request_doc_id.clone(),
                    request_doc_id: candidate.request_doc_id.clone(),
                });
            }
            return Ok(AdmitBinding::Reuse(existing_active.clone()));
        }
        if !others.is_empty() && !release_previous {
            return Err(BindingError::UniqueActiveExists {
                label,
                workspace_id: workspace_id.to_owned(),
            });
        }
        let release = if release_previous {
            others
                .into_iter()
                .map(|mut binding| {
                    binding.lifecycle_state = BINDING_RELEASED.to_string();
                    binding
                })
                .collect()
        } else {
            Vec::new()
        };
        candidate.lifecycle_state = BINDING_ACTIVE.to_string();
        return Ok(AdmitBinding::Create {
            binding: candidate,
            release,
        });
    }

    if let Some(existing_active) = existing.iter().find(|binding| {
        binding.is_active()
            && binding.request_id == candidate.request_id
            && binding.authority == candidate.authority
    }) {
        if existing_active.request_doc_id != candidate.request_doc_id {
            return Err(BindingError::RequestDocMismatch {
                authority: "ReadOnly",
                request_id: candidate.request_id.clone(),
                existing_request_doc_id: existing_active.request_doc_id.clone(),
                request_doc_id: candidate.request_doc_id.clone(),
            });
        }
        return Ok(AdmitBinding::Reuse(existing_active.clone()));
    }
    candidate.lifecycle_state = BINDING_ACTIVE.to_string();
    Ok(AdmitBinding::Create {
        binding: candidate,
        release: Vec::new(),
    })
}

pub fn new_binding(
    workspace_id: &str,
    request_id: &str,
    request_doc_id: &str,
    authority: WorkspaceAuthority,
    owner_node_did: &str,
    seal_hash: Option<&str>,
) -> WorkspaceBindingDoc {
    WorkspaceBindingDoc {
        binding_id: binding_id_for(workspace_id, request_id),
        workspace_id: workspace_id.to_string(),
        request_id: request_id.to_string(),
        request_doc_id: request_doc_id.to_string(),
        authority: authority.as_str().to_string(),
        owner_node_did: owner_node_did.to_string(),
        seal_hash: seal_hash
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        lifecycle_state: BINDING_ACTIVE.to_string(),
    }
}

pub fn release_binding(mut binding: WorkspaceBindingDoc) -> WorkspaceBindingDoc {
    binding.lifecycle_state = BINDING_RELEASED.to_string();
    binding
}
