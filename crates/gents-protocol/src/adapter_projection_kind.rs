use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterProjectionKind {
    AtifTrajectory,
    #[serde(rename = "openai_codex_run_trace")]
    OpenAiCodexRunTrace,
    #[serde(rename = "langgraph_state_history")]
    LangGraphStateHistory,
    MultiAgentTask,
}

impl AdapterProjectionKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::AtifTrajectory => "atif_trajectory",
            Self::OpenAiCodexRunTrace => "openai_codex_run_trace",
            Self::LangGraphStateHistory => "langgraph_state_history",
            Self::MultiAgentTask => "multi_agent_task",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::AtifTrajectory => "Agent Trajectory Interchange Format (ATIF) v1.7",
            Self::OpenAiCodexRunTrace => "OpenAI/Codex Run Trace Projection",
            Self::LangGraphStateHistory => "LangGraph State History Projection",
            Self::MultiAgentTask => "Multi-Agent Task Projection",
        }
    }
}
