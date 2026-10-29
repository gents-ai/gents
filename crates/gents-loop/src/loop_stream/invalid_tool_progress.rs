use crate::tool_call_lifecycle::{FailureClass, ToolOutcome};

const INVALID_TOOL_CALL_LIMIT: u8 = 8;

#[derive(Default)]
pub(super) struct InvalidToolProgress {
    invalid_used: u8,
}

impl InvalidToolProgress {
    pub(super) fn remaining(&self) -> usize {
        usize::from(INVALID_TOOL_CALL_LIMIT - self.invalid_used)
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
        self.invalid_used >= INVALID_TOOL_CALL_LIMIT
    }

    pub(super) fn exhaustion_reason(&self) -> String {
        format!(
            "invalid_tool_call_budget_exhausted: limit={}, used={}; repeated invalid arguments, unknown tools, or policy-denied calls exhausted this execution's allowance",
            INVALID_TOOL_CALL_LIMIT, self.invalid_used,
        )
    }
}
