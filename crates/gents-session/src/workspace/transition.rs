use super::documents::{
    IsolatedWorkspaceDoc, LIFECYCLE_CLEANED, LIFECYCLE_CLEANING, LIFECYCLE_PROVISIONING,
    LIFECYCLE_PROVISION_FAILED, LIFECYCLE_READY, LIFECYCLE_SEALED,
};
use gents_loop::tool_policy::normalize_workspace_lifecycle_state;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceLifecycleState {
    Provisioning,
    Ready,
    ProvisionFailed,
    Sealed,
    Cleaning,
    Cleaned,
}

impl WorkspaceLifecycleState {
    pub fn parse(value: &str) -> Option<Self> {
        match normalize_workspace_lifecycle_state(value)? {
            LIFECYCLE_PROVISIONING => Some(Self::Provisioning),
            LIFECYCLE_READY => Some(Self::Ready),
            LIFECYCLE_PROVISION_FAILED => Some(Self::ProvisionFailed),
            LIFECYCLE_SEALED => Some(Self::Sealed),
            LIFECYCLE_CLEANING => Some(Self::Cleaning),
            LIFECYCLE_CLEANED => Some(Self::Cleaned),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => LIFECYCLE_PROVISIONING,
            Self::Ready => LIFECYCLE_READY,
            Self::ProvisionFailed => LIFECYCLE_PROVISION_FAILED,
            Self::Sealed => LIFECYCLE_SEALED,
            Self::Cleaning => LIFECYCLE_CLEANING,
            Self::Cleaned => LIFECYCLE_CLEANED,
        }
    }
}

impl std::fmt::Display for WorkspaceLifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceTransitionError {
    #[error("unknown workspace lifecycle state `{state}`")]
    UnknownState { state: String },
    #[error("sealing workspace from `{from}` requires a non-empty seal_hash")]
    SealRequiresHash { from: WorkspaceLifecycleState },
    #[error("illegal workspace lifecycle transition from `{from}` to `{to}`")]
    IllegalTransition {
        from: WorkspaceLifecycleState,
        to: WorkspaceLifecycleState,
    },
}

pub fn transition_workspace_lifecycle(
    from_state: &str,
    to_state: &str,
    seal_hash: Option<&str>,
) -> Result<WorkspaceLifecycleState, WorkspaceTransitionError> {
    let from = WorkspaceLifecycleState::parse(from_state).ok_or_else(|| {
        WorkspaceTransitionError::UnknownState {
            state: from_state.to_owned(),
        }
    })?;
    let to = WorkspaceLifecycleState::parse(to_state).ok_or_else(|| {
        WorkspaceTransitionError::UnknownState {
            state: to_state.to_owned(),
        }
    })?;
    let has_seal = seal_hash.is_some_and(|hash| !hash.trim().is_empty());
    match (from, to) {
        (WorkspaceLifecycleState::Provisioning, WorkspaceLifecycleState::Ready)
        | (WorkspaceLifecycleState::Provisioning, WorkspaceLifecycleState::ProvisionFailed)
        | (WorkspaceLifecycleState::Sealed, WorkspaceLifecycleState::Cleaning)
        | (WorkspaceLifecycleState::ProvisionFailed, WorkspaceLifecycleState::Cleaning)
        | (WorkspaceLifecycleState::Cleaning, WorkspaceLifecycleState::Cleaned) => Ok(to),
        (
            WorkspaceLifecycleState::Provisioning | WorkspaceLifecycleState::Ready,
            WorkspaceLifecycleState::Sealed,
        ) => {
            if has_seal {
                Ok(to)
            } else {
                Err(WorkspaceTransitionError::SealRequiresHash { from })
            }
        }
        _ => Err(WorkspaceTransitionError::IllegalTransition { from, to }),
    }
}

pub fn step_isolated_workspace(
    workspace: &IsolatedWorkspaceDoc,
    to_state: WorkspaceLifecycleState,
) -> Result<IsolatedWorkspaceDoc, WorkspaceTransitionError> {
    let next = transition_workspace_lifecycle(
        &workspace.lifecycle_state,
        to_state.as_str(),
        workspace.seal_hash.as_deref(),
    )?;
    let mut updated = workspace.clone();
    updated.lifecycle_state = next.as_str().to_owned();
    Ok(updated)
}
