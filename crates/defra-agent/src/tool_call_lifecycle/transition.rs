//! Transition methods on ToolCallLifecycle.
//!
//! Mirrors `crates/defra-agent/src/lifecycle/transition.rs`. Each transition
//! method calls `ensure_state` at the top to assert the precondition state,
//! then performs the GraphQL mutation atomically, then updates in-memory
//! state on confirmed success.
//!
//! `ensure_state` is verified via Bucket 3 integration tests (Task 25), which
//! exercise it through every transition method's precondition path. There is
//! no standalone unit test — fabricating a stub `Arc<EmbeddedNode>` would
//! require unsafe memory tricks and the integration coverage is sufficient.

use anyhow::{anyhow, Result};

use super::{ToolCallLifecycle, ToolCallState};

/// Error returned when a transition method is called from an illegal
/// pre-state. Programmer error, not a user-visible failure.
#[derive(Debug, thiserror::Error)]
#[error("illegal tool call transition: cannot {method} from state {from:?} (allowed: {allowed:?})")]
pub struct IllegalToolCallTransition {
    pub method: &'static str,
    pub from: ToolCallState,
    pub allowed: Vec<ToolCallState>,
}

impl ToolCallLifecycle {
    /// Assert that the current state is in `allowed`. Returns
    /// `IllegalToolCallTransition` otherwise.
    pub(crate) fn ensure_state(
        &self,
        allowed: &[ToolCallState],
        method: &'static str,
    ) -> Result<()> {
        if allowed.contains(&self.state) {
            Ok(())
        } else {
            Err(anyhow!(IllegalToolCallTransition {
                method,
                from: self.state,
                allowed: allowed.to_vec(),
            }))
        }
    }
}
