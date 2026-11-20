use crate::tool_call_lifecycle::{FailureClass, ToolOutcome};

/// Invalid-argument, unknown-tool and policy-denied calls an execution may
/// make when `InferenceExecution.max_invalid_tool_calls` is unset.
pub const DEFAULT_MAX_INVALID_TOOL_CALLS: usize = 8;

pub(super) struct InvalidToolProgress {
    limit: usize,
    invalid_used: usize,
}

impl InvalidToolProgress {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            invalid_used: 0,
        }
    }

    pub(super) fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.invalid_used)
    }

    pub(super) fn record(&mut self, outcome: &ToolOutcome) {
        if !self.exhausted() && Self::charges(outcome) {
            self.invalid_used += 1;
        }
    }

    pub(super) fn charges(outcome: &ToolOutcome) -> bool {
        matches!(
            outcome,
            ToolOutcome::Failed {
                class: FailureClass::ArgumentInvalid | FailureClass::PolicyDenied,
                ..
            }
        )
    }

    pub(super) fn exhausted(&self) -> bool {
        self.invalid_used >= self.limit
    }

    pub(super) fn exhaustion_reason(&self) -> String {
        format!(
            "invalid_tool_call_budget_exhausted: limit={}, used={}; repeated invalid arguments, unknown tools, or policy-denied calls exhausted this execution's allowance",
            self.limit, self.invalid_used,
        )
    }
}
