use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ToolClass;

/// What a request subtree is allowed to spend.
///
/// `Option`: `None` means "unbounded"
///
/// The struct is serializes into the `allocation_json` column
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetAllocation {
    pub max_turns: Option<u32>,
    pub max_tool_calls: Option<u32>,
    pub max_tool_calls_by_class: Option<BTreeMap<ToolClass, u32>>,
    pub deadline: Option<DateTime<Utc>>,
    pub max_total_tokens: Option<u64>,
    pub max_output_bytes: Option<u64>,
    /// The whole subtree, not the direct fan-out.
    pub max_descendants: Option<u32>,
    /// Direct children only.
    pub max_fan_out: Option<u32>,
    pub max_depth: Option<u32>,
    pub accounting: Option<CostAccounting>,
}

impl BudgetAllocation {
    pub fn unbounded() -> Self {
        Self::default()
    }

    pub fn is_unbounded(&self) -> bool {
        *self == Self::default()
    }
}

/// Provider-facing cost annotation carried alongside the allocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CostAccounting(serde_json::Value);

impl CostAccounting {
    pub fn new(value: serde_json::Value) -> Self {
        Self(value)
    }

    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }

    pub fn into_value(self) -> serde_json::Value {
        self.0
    }
}
