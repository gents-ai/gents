use std::sync::Arc;

use gents_inference::resolved::ResolvedAgent;

use super::AgentToolSurfaceConfig;

/// A resolved Agent and the tool surface configuration selected with it.
///
/// Both halves are resolved by one owner from one configuration read, so the
/// pair never mixes two revisions of an Agent. Its Debug value covers both
/// halves and is what the runtime configuration fingerprint and the
/// reconcile diff compare.
#[derive(Clone, Debug)]
pub struct AgentWithTools {
    pub agent: Arc<ResolvedAgent>,
    pub tools: Arc<AgentToolSurfaceConfig>,
}

impl AgentWithTools {
    pub fn new(agent: ResolvedAgent, tools: AgentToolSurfaceConfig) -> Self {
        Self {
            agent: Arc::new(agent),
            tools: Arc::new(tools),
        }
    }

    /// Identity of everything an Agent slot builds once and keeps: the
    /// inference slot fingerprint plus the tool surface configuration.
    pub fn slot_fingerprint(&self) -> String {
        format!(
            "{}|tools={:?}",
            gents_inference::connection_fingerprint::agent_slot_fingerprint(&self.agent),
            self.tools
        )
    }
}
