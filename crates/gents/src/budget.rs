//! Execution budgets: the immutable allocation a request subtree runs under.
//!
//! The shape here mirrors the persisted `ExecutionBudget` collection
//! (`gents-schemas/schemas/agent/execution_budget.graphql`): an allocation and
//! a derivation, both serialized as opaque JSON columns exactly like
//! `GraphRun.limits_json`, plus lineage keys.
//!
//! Two invariants shape every type in this module:
//!
//! 1. **The allocation is immutable.** It is minted once and never rewritten.
//!    Growing a child's allowance is a new [`BudgetDerivation::Grant`], not a
//!    mutation, so the durable row stays append-only and a replayed grant is
//!    idempotent rather than cumulative.
//! 2. **Consumption is derived, never stored.** [`BudgetConsumption`] is a
//!    projection over the append-only `InferenceCall` / `AgentToolCall` rows
//!    that already exist. No counter is persisted, so nothing can be reset,
//!    double-counted, or lost to a crash between increment and use.

mod allocation;
mod consumption;
mod derivation;
mod tool_class;

pub use allocation::{BudgetAllocation, CostAccounting};
pub use consumption::BudgetConsumption;
pub use derivation::BudgetDerivation;
pub use tool_class::ToolClass;

use serde::{Deserialize, Serialize};

/// One budget row: the immutable allocation governing a request subtree.
///
/// `budget_group_id` names the subtree this budget participates in; every
/// descendant request carries the same group so consumption can be summed
/// with one indexed query instead of a recursive walk. `parent_budget_id` is
/// one hop up, `None` at the root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionBudget {
    pub budget_id: String,
    pub budget_group_id: String,
    pub parent_budget_id: Option<String>,
    pub depth: u32,
    pub allocation: BudgetAllocation,
    pub derivation: BudgetDerivation,
}

#[cfg(test)]
mod tests;
