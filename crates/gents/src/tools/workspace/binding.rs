use anyhow::{bail, Result};

use super::documents::{
    binding_id_for, WorkspaceBindingDoc, BINDING_ACTIVE, BINDING_RELEASED, LIFECYCLE_SEALED,
};
use crate::tools::native::{normalize_workspace_lifecycle_state, WorkspaceAuthority};

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
    anyhow::ensure!(
        !candidate.owner_node_did.trim().is_empty()
            && existing
                .iter()
                .filter(|binding| binding.workspace_id == workspace_id)
                .all(|binding| binding.owner_node_did == candidate.owner_node_did),
        "workspace bindings must share their node owner"
    );
    let authority = WorkspaceAuthority::parse(&candidate.authority)?;
    if !authority.bindable_lifecycle_state(workspace_state) {
        bail!(
            "isolated workspace {workspace_id} in state {workspace_state} is not bindable for authority {}",
            authority.as_str()
        );
    }
    if candidate.workspace_id != workspace_id {
        bail!(
            "binding workspace_id {} does not match {workspace_id}",
            candidate.workspace_id
        );
    }

    let mut candidate = candidate;
    if normalize_workspace_lifecycle_state(workspace_state) == Some(LIFECYCLE_SEALED) {
        let workspace_hash = workspace_seal_hash
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("sealed workspace {workspace_id} is missing seal_hash")
            })?;
        match candidate
            .seal_hash
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            None => candidate.seal_hash = Some(workspace_hash.to_string()),
            Some(hash) if hash == workspace_hash => {}
            Some(hash) => bail!(
                "binding seal_hash {hash} does not match workspace seal_hash {workspace_hash}"
            ),
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
            bail!(
                "multiple Active {label} bindings exist for workspace {workspace_id}; failing closed"
            );
        }
        if let Some(existing_active) = active
            .iter()
            .find(|binding| binding.request_id == candidate.request_id)
        {
            if existing_active.request_doc_id != candidate.request_doc_id {
                bail!(
                    "Active ReadWrite binding for request {} points at request_doc_id {}, not {}",
                    candidate.request_id,
                    existing_active.request_doc_id,
                    candidate.request_doc_id
                );
            }
            return Ok(AdmitBinding::Reuse(existing_active.clone()));
        }
        if !others.is_empty() && !release_previous {
            bail!("unique Active {label} binding already exists for workspace {workspace_id}");
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
            bail!(
                "Active ReadOnly binding for request {} points at request_doc_id {}, not {}",
                candidate.request_id,
                existing_active.request_doc_id,
                candidate.request_doc_id
            );
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
