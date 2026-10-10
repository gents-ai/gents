//! Node-scoped runtime agent assembly.
//!
//! Both production construction paths
//! (`document_view::snapshot::resolve_document_runtime_snapshot_from_view`
//! and `GentsBuilder::build`) funnel their node/agent Arc
//! construction through `assemble_node_and_agents`. This is the
//! point that binds each resolved agent to the validated DID and signing
//! handle for the node-and-agent key being assembled.

use std::sync::Arc;

use crate::config::ResolvedAgent;
use crate::identity::RuntimeNode;

#[derive(Debug)]
pub struct AgentBuildError {
    pub agent_id: String,
    pub error: anyhow::Error,
}

impl std::fmt::Display for AgentBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

/// Assemble a snapshot's node Arc and agents from pre-resolved
/// inputs.
///
/// The caller supplies `node_data` (the resolved `RuntimeNode`
/// fields) and an iterator of per-agent factory closures. The helper:
///
/// 1. Wraps the validated node data in a runtime handle.
/// 2. Calls each factory with that owner and signing handle.
/// 3. Returns the node Arc and a `Vec<Result<Arc<ResolvedAgent>, E>>`
///    so the caller can route per-agent failures into its
///    `unavailable_agents` map without short-circuiting the loop.
pub fn assemble_node_and_agents<I, F, E>(
    node_data: RuntimeNode,
    agent_factories: I,
) -> (
    Arc<RuntimeNode>,
    Vec<std::result::Result<Arc<ResolvedAgent>, E>>,
)
where
    I: IntoIterator<Item = F>,
    F: FnOnce(Arc<RuntimeNode>) -> std::result::Result<ResolvedAgent, E> + Send,
{
    let node = Arc::new(node_data);
    let agents = agent_factories
        .into_iter()
        .map(|factory| factory(node.clone()).map(Arc::new))
        .collect();
    (node, agents)
}
