use std::sync::Arc;

use defra_node::EmbeddedNode;
use tokio::sync::watch;

use crate::config::ProfileConfig;
use crate::hook::FailurePolicy;
use crate::mcp_pool::McpPool;
use crate::retry::RetryPolicy;

mod builder;
mod daemon;
mod runtime;
mod stream_processor;
mod supervision;
#[cfg(test)]
mod tests;

pub use builder::{DefraAgentBuilder, ProfileBuilder};
#[cfg(test)]
pub(crate) use builder::PendingProfileConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLifecycleState {
    Uninitialized,
    Recovering,
    Ready,
    ShuttingDown,
    Shutdown,
}

impl ProcessLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninitialized",
            Self::Recovering => "recovering",
            Self::Ready => "ready",
            Self::ShuttingDown => "shuttingDown",
            Self::Shutdown => "shutdown",
        }
    }
}

pub trait ProcessLifecycleObserver: Send + Sync {
    fn on_process_state_change(&self, state: ProcessLifecycleState);
}

#[derive(Clone)]
pub struct DefraAgent {
    node: Arc<EmbeddedNode>,
    profiles: Vec<Arc<ProfileConfig>>,
    mcp_pool: McpPool,
    local_hostname: String,
    local_subnet: Option<String>,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    process_state_observer: Option<Arc<dyn ProcessLifecycleObserver>>,
}

impl DefraAgent {
    pub fn builder() -> DefraAgentBuilder {
        DefraAgentBuilder::default()
    }

    pub fn profiles(&self) -> &[Arc<ProfileConfig>] {
        &self.profiles
    }

    pub async fn run(self, shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
        runtime::run_agent(self, shutdown).await
    }
}
