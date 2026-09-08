use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::ToolClass;

/// A projection of what a budget's subtree has spent so far.
///
/// Not persisted. Every field is a fold over rows that are already durable
/// and append-only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetConsumption {
    pub turns: u32,
    pub tool_calls: u32,
    pub tool_calls_by_class: BTreeMap<ToolClass, u32>,
    pub total_tokens: u64,
    pub descendants: u32,
    pub elapsed: Duration,
}
