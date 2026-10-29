use super::invalid_tool_progress::InvalidToolProgress;
use crate::live_output::{COMMAND_OUTPUT_META_PREFIX, COMMAND_OUTPUT_VOLATILE_FIELDS};
use crate::tool_call_lifecycle::{FailureClass, ToolOutcome};
use crate::truncation::floor_char_boundary;

const FAILURE_LIMIT: u32 = 3;
const SUPPRESSION_LIMIT: u32 = 2;
/// Bounds the error included in the notice and stop reason; the full error
/// identity still determines whether dispatched calls failed identically.
const ERROR_EXCERPT_BYTES: usize = 512;

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
        *self = Self::default();
    }

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
