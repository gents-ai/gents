use super::invalid_tool_progress::InvalidToolProgress;
use crate::live_output::{COMMAND_OUTPUT_META_PREFIX, COMMAND_OUTPUT_VOLATILE_FIELDS};
use crate::tool_call_lifecycle::{FailureClass, ToolOutcome};
use crate::truncation::floor_char_boundary;

/// Identical failing dispatches of one call before further repeats are
/// intercepted, when `InferenceExecution.repeated_tool_failure_limit` is unset.
pub const DEFAULT_REPEATED_TOOL_FAILURE_LIMIT: usize = 3;
/// Lean `RepeatedToolFailure.suppressionLimit`: one notice, then stop. A fixed protocol, not a budget.
const SUPPRESSION_LIMIT: u32 = 2;
/// Lean `RepeatedToolSuccess.limit`, fixed by the model.
const SUCCESS_LIMIT: u32 = 8;
/// Bounds the error included in the notice and stop reason; the full error
/// identity still determines whether dispatched calls failed identically.
const ERROR_EXCERPT_BYTES: usize = 512;

pub const REPEATED_TOOL_FAILURE_PREFIX: &str = "repeated_tool_failure: ";
pub const REPEATED_TOOL_SUCCESS_PREFIX: &str = "repeated_tool_success: ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RepeatDecision {
    Dispatch,
    Suppress,
    Stop,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreakKind {
    Failure,
    Success,
}

struct Streak {
    kind: StreakKind,
    tool: String,
    args: String,
    identity: String,
    excerpt: String,
    count: u32,
}

pub(super) struct RepeatedToolProgress {
    streak: Option<Streak>,
    suppressed: u32,
    failure_limit: u32,
}

impl RepeatedToolProgress {
    /// A limit beyond `u32::MAX` saturates; the streak count cannot exceed it.
    pub(super) fn new(failure_limit: usize) -> Self {
        Self {
            streak: None,
            suppressed: 0,
            failure_limit: u32::try_from(failure_limit).unwrap_or(u32::MAX),
        }
    }

    fn repeats_streak(&self, tool: &str, args: &str) -> bool {
        self.streak.as_ref().is_some_and(|streak| {
            streak.kind == StreakKind::Failure
                && streak.tool == tool
                && streak.args == args
                && streak.count >= self.failure_limit
        })
    }

    pub(super) fn decide(&self, tool: &str, args: &str) -> RepeatDecision {
        if !self.repeats_streak(tool, args) {
            RepeatDecision::Dispatch
        } else if self.suppressed + 1 < SUPPRESSION_LIMIT {
            RepeatDecision::Suppress
        } else {
            RepeatDecision::Stop
        }
    }

    pub(super) fn reset(&mut self) {
        self.streak = None;
        self.suppressed = 0;
    }

    pub(super) fn record_dispatched(
        &mut self,
        tool: &str,
        args: &str,
        outcome: &ToolOutcome,
        command_envelope: bool,
    ) {
        let identity = match outcome {
            ToolOutcome::Completed(text) => stable_result_identity(text, command_envelope)
                .map(|identity| (StreakKind::Success, identity, text)),
            ToolOutcome::Failed { class, text, .. } if !InvalidToolProgress::charges(outcome) => {
                error_identity(*class, text, command_envelope)
                    .map(|identity| (StreakKind::Failure, identity, text))
            }
            _ => None,
        };
        let Some((kind, identity, text)) = identity else {
            self.reset();
            return;
        };
        match self.streak.as_mut() {
            Some(streak)
                if streak.kind == kind
                    && streak.tool == tool
                    && streak.args == args
                    && streak.identity == identity =>
            {
                streak.count += 1;
                streak.excerpt = error_excerpt(text).to_owned();
            }
            _ => {
                self.suppressed = 0;
                self.streak = Some(Streak {
                    kind,
                    tool: tool.to_owned(),
                    args: args.to_owned(),
                    identity,
                    excerpt: error_excerpt(text).to_owned(),
                    count: 1,
                });
            }
        }
    }

    /// Every dispatched read in a batch settles. Isolate a possible eighth
    /// success so later reads cannot execute before its result decides the stop.
    pub(super) fn success_may_stop(&self, tool: &str, args: &str) -> bool {
        self.streak.as_ref().is_some_and(|streak| {
            streak.kind == StreakKind::Success
                && streak.count + 1 >= SUCCESS_LIMIT
                && streak.tool == tool
                && streak.args == args
        })
    }

    pub(super) fn success_stop_reason(&self) -> Option<String> {
        self.streak.as_ref().filter(|streak| {
            streak.kind == StreakKind::Success && streak.count >= SUCCESS_LIMIT
        }).map(|streak| format!(
            "{REPEATED_TOOL_SUCCESS_PREFIX}tool={}, identical_successes={}; the tool returned the same result with the same arguments on every call; change the arguments or take a different approach",
            streak.tool, streak.count,
        ))
    }

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
                failures = streak.count,
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
            streak.count,
            self.suppressed + 1,
            streak.excerpt,
        )
    }
}

fn error_identity(class: FailureClass, text: &str, command_envelope: bool) -> Option<String> {
    stable_result_identity(text, command_envelope)
        .map(|stable| format!("{}\n{stable}", class.as_str()))
}

fn stable_result_identity(text: &str, command_envelope: bool) -> Option<String> {
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
    Some(stable)
}

fn error_excerpt(error: &str) -> &str {
    &error[..floor_char_boundary(error, ERROR_EXCERPT_BYTES)]
}

#[cfg(test)]
#[path = "repeated_tool_failure/success_tests.rs"]
mod success_tests;
