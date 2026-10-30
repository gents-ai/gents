use std::sync::Arc;

use crate::config::ResolvedAgent;
use crate::store::identity::RuntimeNode;

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
