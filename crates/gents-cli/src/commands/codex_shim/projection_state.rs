//! Codex-independent state used to decide whether a durable observation is
//! semantically new. Wire-protocol types belong in the emit/read boundary,
//! not in the stream's equality and de-duplication model.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectionStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ToolProjectionStatus {
    Mcp(ProjectionStatus),
    Command(ProjectionStatus),
    DeferredFileChange,
    FileChange(ProjectionStatus),
}

impl ToolProjectionStatus {
    pub(super) fn command_status(&self) -> ProjectionStatus {
        match self {
            Self::Command(status) => *status,
            Self::Mcp(_) | Self::DeferredFileChange | Self::FileChange(_) => {
                ProjectionStatus::InProgress
            }
        }
    }
}
