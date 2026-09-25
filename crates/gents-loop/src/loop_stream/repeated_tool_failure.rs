//! Refinement of CompletionRetry.RepeatedToolFailure. This is execution-local
//! accounting over the loop's own dispatch record, not a request lifecycle or
//! tool permission owner.

use super::invalid_tool_progress::InvalidToolProgress;
use crate::live_output::{COMMAND_OUTPUT_META_PREFIX, COMMAND_OUTPUT_VOLATILE_FIELDS};
use crate::tool_call_lifecycle::{FailureClass, ToolOutcome};
use crate::truncation::floor_char_boundary;

const FAILURE_LIMIT: u32 = 3;
const SUPPRESSION_LIMIT: u32 = 2;
/// Bound on the error excerpt echoed into notices and the terminal reason.
const ERROR_EXCERPT_BYTES: usize = 512;

/// Prefix of the terminal reason when a model keeps repeating a call that
/// failed identically.
pub const REPEATED_TOOL_FAILURE_PREFIX: &str = "repeated_tool_failure: ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RepeatDecision {
    Dispatch,
    Suppress,
    Stop,
}

struct Streak {
    tool: String,
    args: String,
    error: String,
    excerpt: String,
    failures: u32,
}

#[derive(Default)]
pub(super) struct RepeatedToolFailure {
    streak: Option<Streak>,
    suppressed: u32,
}

impl RepeatedToolFailure {
    fn repeats_streak(&self, tool: &str, args: &str) -> bool {
        self.streak.as_ref().is_some_and(|streak| {
            streak.tool == tool && streak.args == args && streak.failures >= FAILURE_LIMIT
        })
    }

    /// Checked before each call is admitted, including calls later in the
    /// same provider turn.
    pub(super) fn decide(&self, tool: &str, args: &str) -> RepeatDecision {
        if !self.repeats_streak(tool, args) {
            RepeatDecision::Dispatch
        } else if self.suppressed + 1 < SUPPRESSION_LIMIT {
            RepeatDecision::Suppress
        } else {
            RepeatDecision::Stop
        }
    }

    /// A call the persistence hook answers itself.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Called with the outcome the hook accepted for a dispatched call. Only
    /// an ordinary failure with a derivable error identity counts.
    /// `command_envelope` is the dispatched tool's own registration, never
    /// inferred from its text.
    pub(super) fn record_dispatched(
        &mut self,
        tool: &str,
        args: &str,
        outcome: &ToolOutcome,
        command_envelope: bool,
    ) {
        let identity = match outcome {
            ToolOutcome::Failed { class, text, .. } if !InvalidToolProgress::charges(outcome) => {
                error_identity(*class, text, command_envelope).map(|identity| (identity, text))
            }
            _ => None,
        };
        let Some((identity, text)) = identity else {
            self.reset();
            return;
        };
        match self.streak.as_mut() {
            Some(streak)
                if streak.tool == tool && streak.args == args && streak.error == identity =>
            {
                streak.failures += 1;
                streak.excerpt = error_excerpt(text).to_owned();
            }
            _ => {
                *self = Self {
                    streak: Some(Streak {
                        tool: tool.to_owned(),
                        args: args.to_owned(),
                        error: identity,
                        excerpt: error_excerpt(text).to_owned(),
                        failures: 1,
                    }),
                    suppressed: 0,
                };
            }
        }
    }

    /// The typed result that stands in for a suppressed call. It is recorded
    /// through the hook like any dispatched outcome, so the durable transcript
    /// and the next provider input carry the same notice.
    pub(super) fn suppress(&mut self) -> ToolOutcome {
        self.suppressed += 1;
        let streak = self
            .streak
            .as_ref()
            .expect("suppression requires an identical failure streak");
        ToolOutcome::Failed {
            class: FailureClass::PolicyDenied,
            denial: None,
            text: format!(
                "repeated_tool_call_not_run: {tool} was called with these exact arguments {failures} \
                 times in a row and failed each time with the same error, so this call was not run. \
                 Change the arguments or take a different approach; calling it again unchanged ends \
                 this request. Last error: {error}",
                tool = streak.tool,
                failures = streak.failures,
                error = streak.excerpt,
            ),
        }
    }

    pub(super) fn stop_reason(&self) -> String {
        let streak = self
            .streak
            .as_ref()
            .expect("stopping requires an identical failure streak");
        format!(
            "{REPEATED_TOOL_FAILURE_PREFIX}tool={}, identical_failures={}, repeated_after_notice={}; \
             the model kept repeating a call that failed identically with the same arguments; \
             last error: {}",
            streak.tool,
            streak.failures,
            self.suppressed + 1,
            streak.excerpt,
        )
    }
}

/// Stable identity of an ordinary failure: its class and text. Only a result
/// of the command runner, which renders the compact envelope, has fields known
/// to vary between identical runs, and only those fields are dropped. A
/// command result whose envelope does not parse yields no identity, so the
/// call is not counted.
fn error_identity(class: FailureClass, text: &str, command_envelope: bool) -> Option<String> {
    let envelope = command_envelope
        .then(|| text.strip_prefix(COMMAND_OUTPUT_META_PREFIX))
        .flatten();
    let stable = match envelope {
        Some(envelope) => {
            let (metadata, streams) = envelope.split_once('\n')?;
            let mut metadata: serde_json::Value = serde_json::from_str(metadata).ok()?;
            let fields = metadata.as_object_mut()?;
            for field in COMMAND_OUTPUT_VOLATILE_FIELDS {
                fields.remove(field);
            }
            format!("{COMMAND_OUTPUT_META_PREFIX}{metadata}\n{streams}")
        }
        None => text.to_owned(),
    };
    Some(format!("{}\n{stable}", class.as_str()))
}

fn error_excerpt(error: &str) -> &str {
    &error[..floor_char_boundary(error, ERROR_EXCERPT_BYTES)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(class: FailureClass, text: &str) -> ToolOutcome {
        ToolOutcome::Failed {
            class,
            denial: None,
            text: text.into(),
        }
    }

    #[test]
    fn identical_failures_suppress_then_stop() {
        let mut guard = RepeatedToolFailure::default();
        for _ in 0..FAILURE_LIMIT {
            assert_eq!(guard.decide("t", "{}"), RepeatDecision::Dispatch);
            guard.record_dispatched(
                "t",
                "{}",
                &failed(FailureClass::ToolReturnedError, "boom"),
                false,
            );
        }
        assert_eq!(guard.decide("t", "{\"x\":1}"), RepeatDecision::Dispatch);
        assert_eq!(guard.decide("u", "{}"), RepeatDecision::Dispatch);
        assert_eq!(guard.decide("t", "{}"), RepeatDecision::Suppress);
        let notice = guard.suppress();
        assert!(notice.model_facing_text().contains("boom"));
        assert!(InvalidToolProgress::charges(&notice));
        assert_eq!(guard.decide("t", "{}"), RepeatDecision::Stop);
        assert!(guard
            .stop_reason()
            .starts_with(REPEATED_TOOL_FAILURE_PREFIX));
    }

    fn run(exit_code: i32, duration: u64, hint: &str) -> String {
        format!(
            "gents_exec: {{\"ok\":false,\"status\":\"exit_nonzero\",\"exit_code\":{exit_code},\"duration_ms\":{duration},\"hint\":\"{hint}\"}}\nstdout:\n(empty)\nstderr:\nls: missing: No such file"
        )
    }

    #[test]
    fn command_results_ignore_only_volatile_command_metadata() {
        let class = FailureClass::ToolReturnedError;
        let identity = |text: &str| error_identity(class, text, true);
        assert_eq!(identity(&run(2, 3, "a")), identity(&run(2, 41, "b")));
        assert_ne!(identity(&run(2, 3, "a")), identity(&run(1, 3, "a")));
        assert_ne!(
            error_identity(class, "boom", true),
            error_identity(FailureClass::External, "boom", true)
        );
    }

    #[test]
    fn other_tools_are_compared_verbatim_even_in_the_envelope_shape() {
        let class = FailureClass::ToolReturnedError;
        let identity = |text: &str| error_identity(class, text, false);
        assert_ne!(
            identity(&run(2, 3, "retry with --force")),
            identity(&run(2, 3, "check the path"))
        );
        assert_ne!(identity(&run(2, 3, "a")), identity(&run(2, 4, "a")));
        let raw = |hint: &str| format!("{{\"ok\":false,\"hint\":\"{hint}\"}}");
        assert_ne!(identity(&raw("a")), identity(&raw("b")));
        assert!(identity("gents_exec: {not json").is_some());

        let mut guard = RepeatedToolFailure::default();
        for (index, hint) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
            assert_eq!(
                guard.decide("mcp", "{}"),
                RepeatDecision::Dispatch,
                "{index}"
            );
            guard.record_dispatched("mcp", "{}", &failed(class, &run(2, 3, hint)), false);
        }
    }

    #[test]
    fn unparseable_command_metadata_has_no_identity() {
        let class = FailureClass::ToolReturnedError;
        let text = "gents_exec: {not json\nstdout:\n";
        assert_eq!(error_identity(class, text, true), None);
        assert_eq!(error_identity(class, "gents_exec: {}", true), None);
        let mut guard = RepeatedToolFailure::default();
        for _ in 0..FAILURE_LIMIT + 2 {
            assert_eq!(guard.decide("t", "{}"), RepeatDecision::Dispatch);
            guard.record_dispatched("t", "{}", &failed(class, text), true);
        }
    }

    #[test]
    fn invalid_allowance_outcomes_do_not_count() {
        let mut guard = RepeatedToolFailure::default();
        for class in [FailureClass::ArgumentInvalid, FailureClass::PolicyDenied] {
            for _ in 0..FAILURE_LIMIT + 2 {
                assert_eq!(guard.decide("t", "{}"), RepeatDecision::Dispatch);
                guard.record_dispatched("t", "{}", &failed(class, "invalid"), false);
            }
        }
    }

    #[test]
    fn error_excerpt_respects_char_boundaries() {
        let error = "é".repeat(ERROR_EXCERPT_BYTES);
        let excerpt = error_excerpt(&error);
        assert!(excerpt.len() <= ERROR_EXCERPT_BYTES);
        assert!(excerpt.chars().all(|c| c == 'é'));
    }
}
