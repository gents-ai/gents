//! The output-obligation gate seam.
//!
//! Before the loop lets a turn end with no tool effect, it asks whether every
//! configured "you must write a Result before finishing" contract is
//! satisfied. Checking that means reading completed `AgentToolCall` rows for
//! this request from DefraDB, so the check itself cannot live in the guest;
//! [`OutputObligationCheck`] is the seam. `gents`'s `OutputObligationGate`
//! (native, DefraDB-backed) implements it; the loop only ever sees the trait
//! object.

use std::future::Future;
use std::pin::Pin;

/// One obligation the request has not yet satisfied, rendered to a
/// continuation message when the loop finds any unmet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmetOutputObligation {
    pub tool_name: String,
    pub minimum_writes: usize,
    pub completed_writes: usize,
    pub expected_writes: Option<usize>,
    pub expected_count_field: Option<String>,
}

impl UnmetOutputObligation {
    pub fn new(
        tool_name: impl Into<String>,
        minimum_writes: usize,
        completed_writes: usize,
        expected_writes: Option<usize>,
        expected_count_field: Option<String>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            minimum_writes,
            completed_writes,
            expected_writes,
            expected_count_field,
        }
    }
}

/// Checks whether a request's configured output obligations are satisfied.
/// The native implementation reads completed `AgentToolCall` rows; nothing
/// about the check itself is guest-visible.
pub trait OutputObligationCheck: Send + Sync {
    fn unmet<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<UnmetOutputObligation>>> + Send + 'a>>;
}

/// The continuation message threaded back to the model when obligations
/// remain unmet: pure text formatting, no persistence.
pub fn continuation_message(obligations: &[UnmetOutputObligation]) -> String {
    let requirements = obligations
        .iter()
        .map(|obligation| {
            if let Some(expected) = obligation.expected_writes {
                format!(
                    "`{}` exactly {expected} total time(s) ({} completed, {} remaining)",
                    obligation.tool_name,
                    obligation.completed_writes,
                    expected.saturating_sub(obligation.completed_writes),
                )
            } else if let Some(field) = &obligation.expected_count_field {
                format!(
                    "`{}` at least once to declare the exact closed-set size in `{field}` ({} completed)",
                    obligation.tool_name, obligation.completed_writes
                )
            } else {
                format!(
                    "`{}` at least {} total time(s) ({} completed)",
                    obligation.tool_name, obligation.minimum_writes, obligation.completed_writes
                )
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "The request cannot complete yet because its configured output obligation is unmet. \
         Complete the required durable write before answering: {requirements}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_message_states_exact_and_minimum_shapes() {
        let obligations = vec![
            UnmetOutputObligation::new("write_result", 1, 0, Some(3), None),
            UnmetOutputObligation::new("log_event", 2, 1, None, None),
        ];
        let message = continuation_message(&obligations);
        assert!(
            message.contains("`write_result` exactly 3 total time(s) (0 completed, 3 remaining)")
        );
        assert!(message.contains("`log_event` at least 2 total time(s) (1 completed)"));
    }
}
