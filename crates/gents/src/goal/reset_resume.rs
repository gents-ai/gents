//! A usage-limited Goal the operator opted in resumes by itself once the
//! reset its provider reported has passed (Lean `resumeBy`, `Cause.resetReached`).
use super::*;
use crate::identity::AgentIdentity;

/// Resume `goal` through the operator resume transaction when its reported
/// reset is due at `now`; `None` when it is not.
pub async fn resume_at_reset(
    _node: &EmbeddedNode,
    _identity: &dyn AgentIdentity,
    _goal: &GoalDocument,
    _now: DateTime<Utc>,
) -> Result<Option<GoalResumeReceipt>> {
    Ok(None)
}

#[cfg(test)]
use super::operator_resume::support;
#[cfg(test)]
mod contract_tests;
