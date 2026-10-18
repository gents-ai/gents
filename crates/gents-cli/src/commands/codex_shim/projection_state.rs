#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectionStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentTool {
    New,
    Message,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentProjection {
    pub(super) tool: AgentTool,
    pub(super) status: ProjectionStatus,
    pub(super) receiver_thread_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ToolProjectionStatus {
    Mcp(ProjectionStatus),
    Agent(AgentProjection),
    Command(ProjectionStatus),
    DeferredFileChange,
    FileChange(ProjectionStatus),
}

impl ToolProjectionStatus {
    pub(super) fn command_status(&self) -> ProjectionStatus {
        match self {
            Self::Command(status) => *status,
            Self::Mcp(_) | Self::Agent(_) | Self::DeferredFileChange | Self::FileChange(_) => {
                ProjectionStatus::InProgress
            }
        }
    }
}
