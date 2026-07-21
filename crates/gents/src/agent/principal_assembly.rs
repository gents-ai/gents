//! Single-principal-per-snapshot assembly.
//!
//! Both production construction paths
//! (`document_view::snapshot::resolve_document_runtime_snapshot_from_view`
//! and `GentsBuilder::build`) funnel their principal/behavior Arc
//! construction through `assemble_principal_and_behaviors`. This is the
//! sole place where `Arc::new(AgentPrincipal { ... })` happens during a
//! snapshot build, and where the principal Arc is cloned into each
//! `AgentBehavior`.
//!
//! Lean's `behavior_id_determines_principal` theorem (`Identity.Properties`)
//! is structural at the type level via this helper: every `AgentBehavior`
//! in a snapshot holds a clone of the same `Arc<AgentPrincipal>`. The
//! loader-dedup proptest in `tests/identity_conformance_proptest.rs`
//! drives this helper directly and asserts `Arc::ptr_eq` across all
//! behaviors — if the body of this function regresses to per-iteration
//! Arc construction, the proptest turns red.

use std::sync::Arc;

use crate::config::AgentBehavior;
use crate::identity::AgentPrincipal;

/// Error returned when a behavior factory closure fails during assembly.
///
/// Carries the `behavior_id` so the caller can route the failure into the
/// `unavailable_behaviors` map without losing which behavior failed.
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
/// The caller supplies `principal_data` (the resolved `AgentPrincipal`
/// fields) and an iterator of per-behavior factory closures. The helper:
///
/// 1. Constructs `Arc::new(principal_data)` **exactly once**.
/// 2. Calls each factory with `principal.clone()` (sharing the single
///    Arc).
/// 3. Returns the principal Arc and a `Vec<Result<Arc<AgentBehavior>, E>>`
///    so the caller can route per-behavior failures into its
///    `unavailable_behaviors` map without short-circuiting the loop.
///
/// **The single-principal invariant lives in this function's body.** Any
/// change that moves the `Arc::new(...)` inside the factory loop is the
/// bug class the loader-dedup proptest fences.
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
