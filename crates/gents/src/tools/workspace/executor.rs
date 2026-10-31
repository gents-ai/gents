use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::documents::{IsolatedWorkspaceDoc, WorkspaceDocuments, WorkspacePlacementDoc};

pub(super) mod cleanup;
pub(super) mod create;
pub(super) mod freeze;
pub(super) mod integrate;
pub(super) mod seal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalWorkspaceIdentity {
    pub path_capability: super::WorkspacePathCapability,
    pub workspace_id: String,
    pub work_unit_id: String,
    pub repository_id: String,
    pub base_sha: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryPlacementRef {
    pub repository_id: String,
    pub owner_node_did: String,
    pub host_path: PathBuf,
    pub enabled: bool,
}

pub struct HostExecutorContext<'a> {
    pub owner_node_did: String,
    pub repository: RepositoryPlacementRef,
    pub ceiling: Option<&'a Path>,
    pub capabilities: BTreeSet<String>,
    pub writer_principal: String,
    pub integrator_principal: String,
    pub caused_by_invocation_id: String,
    pub caused_by_correlation: String,
    pub documents: &'a mut dyn WorkspaceDocuments,
}

#[derive(Debug)]
pub enum HostExecuteError {
    Denied {
        reason: String,
    },
    Failed {
        reason: String,
        identity_mismatch: bool,
        outcome: Option<Box<CreateWorkspaceOutcome>>,
    },
}

impl std::fmt::Display for HostExecuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied { reason } => write!(f, "action plan denied: {reason}"),
            Self::Failed { reason, .. } => write!(f, "host action failed: {reason}"),
        }
    }
}

impl std::error::Error for HostExecuteError {}

impl HostExecuteError {
    fn denied(reason: impl Into<String>) -> Self {
        Self::Denied {
            reason: reason.into(),
        }
    }

    fn failed(
        reason: impl Into<String>,
        identity_mismatch: bool,
        outcome: Option<CreateWorkspaceOutcome>,
    ) -> Self {
        Self::Failed {
            reason: reason.into(),
            identity_mismatch,
            outcome: outcome.map(Box::new),
        }
    }

    pub fn identity_mismatch(&self) -> bool {
        matches!(
            self,
            Self::Failed {
                identity_mismatch: true,
                ..
            }
        )
    }

    pub fn outcome(&self) -> Option<&CreateWorkspaceOutcome> {
        match self {
            Self::Failed { outcome, .. } => outcome.as_deref(),
            Self::Denied { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorkspaceOutcome {
    pub workspace: IsolatedWorkspaceDoc,
    pub placement: WorkspacePlacementDoc,
}

pub fn workspace_host_path(
    repository_checkout: &Path,
    workspace_id: &str,
    branch: &str,
    ceiling: Option<&Path>,
) -> Result<PathBuf> {
    let checkout = if repository_checkout.exists() {
        std::fs::canonicalize(repository_checkout).with_context(|| {
            format!(
                "canonicalizing repository checkout {}",
                repository_checkout.display()
            )
        })?
    } else {
        bail!(
            "repository checkout {} does not exist",
            repository_checkout.display()
        );
    };
    let dest = checkout
        .join(".gents")
        .join("workspaces")
        .join(dest_name(workspace_id, branch));
    if dest == checkout {
        bail!("workspace destination collides with the source checkout");
    }
    if let Some(ceiling) = ceiling {
        let ceiling = if ceiling.exists() {
            std::fs::canonicalize(ceiling)
                .with_context(|| format!("canonicalizing ceiling {}", ceiling.display()))?
        } else {
            lexical_absolute(ceiling)
        };
        if !dest.starts_with(&ceiling) {
            bail!(
                "workspace destination {} escapes operator ceiling {}",
                dest.display(),
                ceiling.display()
            );
        }
    }
    Ok(dest)
}

fn dest_name(workspace_id: &str, branch: &str) -> String {
    format!(
        "gents-ws-{}-{}",
        sanitize_fs(workspace_id),
        slug_branch(branch)
    )
}

fn sanitize_fs(value: &str) -> String {
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

fn slug_branch(branch: &str) -> String {
    let stripped = branch
        .strip_prefix("fix/")
        .or_else(|| branch.strip_prefix("feat/"))
        .or_else(|| branch.strip_prefix("feature/"))
        .or_else(|| branch.strip_prefix("docs/"))
        .or_else(|| branch.strip_prefix("perf/"))
        .or_else(|| branch.strip_prefix("chore/"))
        .or_else(|| branch.strip_prefix("agent/"))
        .unwrap_or(branch);
    sanitize_fs(&stripped.replace('/', "-"))
}

fn lexical_absolute(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
