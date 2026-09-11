//! Owner-scoped runtime behavior assembly.
//!
//! Both production construction paths
//! (`document_view::snapshot::resolve_document_runtime_snapshot_from_view`
//! and `GentsBuilder::build`) funnel their principal/behavior Arc
//! construction through `assemble_principal_and_behaviors`. This is the
//! point that binds each resolved behavior to the validated DID and signing
//! handle for the principal-and-behavior key being assembled.

use std::sync::Arc;

use crate::config::ResolvedBehavior;
use crate::identity::RuntimePrincipal;

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

/// Assemble a snapshot's principal Arc and behaviors from pre-resolved
/// inputs.
///
/// The caller supplies `principal_data` (the resolved `RuntimePrincipal`
/// fields) and an iterator of per-behavior factory closures. The helper:
///
/// 1. Wraps the validated principal data in a runtime handle.
/// 2. Calls each factory with that owner and signing handle.
/// 3. Returns the principal Arc and a `Vec<Result<Arc<ResolvedBehavior>, E>>`
///    so the caller can route per-behavior failures into its
///    `unavailable_behaviors` map without short-circuiting the loop.
pub fn assemble_principal_and_behaviors<I, F, E>(
    principal_data: RuntimePrincipal,
    behavior_factories: I,
) -> (
    Arc<RuntimePrincipal>,
    Vec<std::result::Result<Arc<ResolvedBehavior>, E>>,
)
where
    I: IntoIterator<Item = F>,
    F: FnOnce(Arc<RuntimePrincipal>) -> std::result::Result<ResolvedBehavior, E> + Send,
{
    let principal = Arc::new(principal_data);
    let behaviors = behavior_factories
        .into_iter()
        .map(|factory| factory(principal.clone()).map(Arc::new))
        .collect();
    (principal, behaviors)
}
