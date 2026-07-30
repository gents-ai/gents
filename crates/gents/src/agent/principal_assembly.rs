//! Single-principal-per-snapshot assembly.

use std::sync::Arc;

use crate::config::AgentBehavior;
use crate::identity::AgentPrincipal;

#[derive(Debug)]
pub struct BehaviorBuildError {
    pub behavior_id: String,
    pub error: anyhow::Error,
}

impl std::fmt::Display for BehaviorBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

/// **The single-principal invariant lives in this function's body.** Any
pub fn assemble_principal_and_behaviors<I, F, E>(
    principal_data: AgentPrincipal,
    behavior_factories: I,
) -> (
    Arc<AgentPrincipal>,
    Vec<std::result::Result<Arc<AgentBehavior>, E>>,
)
where
    I: IntoIterator<Item = F>,
    F: FnOnce(Arc<AgentPrincipal>) -> std::result::Result<AgentBehavior, E> + Send,
{
    let principal = Arc::new(principal_data);
    let behaviors = behavior_factories
        .into_iter()
        .map(|factory| factory(principal.clone()).map(Arc::new))
        .collect();
    (principal, behaviors)
}
