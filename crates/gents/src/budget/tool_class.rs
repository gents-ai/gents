use serde::{Deserialize, Serialize};

/// The family a tool call belongs to, for per-class allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    /// Host file and shell tools built into the runtime.
    Native,
    /// `spawn_subagent` — the only tool that mints a child request.
    Subagent,
    /// Backgrounded execution and its completion notifications.
    BackgroundProcess,
    /// Runtime meta tools (introspection over the agent's own surface).
    Meta,
    /// Tools that rewrite the agent's own configuration documents.
    SelfConfig,
    /// Operator-declared document write/query tools.
    Document,
    /// Memory, session history, and context accounting reads.
    Introspection,
    /// Graph pipeline invocation.
    GraphPipeline,
    /// Tools proxied from an MCP service.
    Mcp,
    /// Operator-declared CLI wrappers.
    Cli,
}

impl ToolClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Subagent => "subagent",
            Self::BackgroundProcess => "background_process",
            Self::Meta => "meta",
            Self::SelfConfig => "self_config",
            Self::Document => "document",
            Self::Introspection => "introspection",
            Self::GraphPipeline => "graph_pipeline",
            Self::Mcp => "mcp",
            Self::Cli => "cli",
        }
    }
}
