use super::TerminalFields;
use crate::tool_call_lifecycle::{CancelCause, FailureClass, ToolCallState};
use anyhow::{Context, Result};
use gents_protocol::output::PayloadPresentation;

pub(crate) fn render_presentation(
    source: &str,
    presentation: &PayloadPresentation,
) -> Result<String> {
    match presentation {
        PayloadPresentation::Full => Ok(source.to_owned()),
        PayloadPresentation::Composed { parts } => {
            let mut rendered = String::new();
            for part in parts {
                match part {
                    gents_protocol::output::PresentationPart::Literal { text } => {
                        rendered.push_str(text)
                    }
                    gents_protocol::output::PresentationPart::OutputRange {
                        start_byte,
                        end_byte,
                    } => {
                        let start = usize::try_from(*start_byte)?;
                        let end = usize::try_from(*end_byte)?;
                        anyhow::ensure!(
                            start <= end
                                && end <= source.len()
                                && source.is_char_boundary(start)
                                && source.is_char_boundary(end),
                            "tool presentation range is not a UTF-8 source boundary"
                        );
                        rendered.push_str(&source[start..end]);
                    }
                }
            }
            Ok(rendered)
        }
    }
}

pub(crate) fn terminal_output_tail(source: &str, budget: usize) -> &str {
    let mut start = source.len().saturating_sub(budget);
    while !source.is_char_boundary(start) {
        start += 1;
    }
    &source[start..]
}

pub(crate) fn terminal_diagnostic_presentation(
    raw: &str,
    cause: &str,
    tail_budget: usize,
) -> Result<PayloadPresentation> {
    use gents_protocol::output::PresentationPart;

    let tail = terminal_output_tail(raw, tail_budget);
    let start = raw.len() - tail.len();
    let parts = if start == raw.len() {
        vec![PresentationPart::Literal {
            text: cause.to_owned(),
        }]
    } else {
        vec![
            PresentationPart::OutputRange {
                start_byte: u64::try_from(start)?,
                end_byte: u64::try_from(raw.len())?,
            },
            PresentationPart::Literal { text: "\n".into() },
            PresentationPart::Literal {
                text: cause.to_owned(),
            },
        ]
    };
    Ok(PayloadPresentation::Composed { parts })
}

pub(super) struct TerminalOutputPlan<'a> {
    pub(super) raw: &'a str,
    pub(super) presentation: PayloadPresentation,
    pub(super) rendered: String,
    pub(super) diagnostic: bool,
}

pub(super) fn diagnostic_replay_matches(
    raw: &str,
    cause: &str,
    stored: &PayloadPresentation,
) -> Result<bool> {
    use gents_protocol::output::PresentationPart;
    let budget = match stored {
        PayloadPresentation::Composed { parts } => match parts.first() {
            Some(PresentationPart::Literal { .. }) if parts.len() == 1 => 0,
            Some(PresentationPart::OutputRange { start_byte, .. }) => {
                match usize::try_from(*start_byte)
                    .ok()
                    .and_then(|start| raw.len().checked_sub(start))
                {
                    Some(budget) => budget,
                    None => return Ok(false),
                }
            }
            _ => return Ok(false),
        },
        PayloadPresentation::Full => return Ok(false),
    };
    Ok(terminal_diagnostic_presentation(raw, cause, budget)? == *stored)
}

pub(super) fn terminal_output_plan<'a>(
    prefix: &'a str,
    text: &'a str,
    pending_raw: Option<&'a str>,
    prepared: Option<&PayloadPresentation>,
    state: ToolCallState,
    output_budget: Option<usize>,
) -> Result<TerminalOutputPlan<'a>> {
    let candidate_raw = if prepared.is_some() {
        pending_raw.unwrap_or(prefix)
    } else {
        text
    };
    if state != ToolCallState::Completed && !candidate_raw.starts_with(prefix) {
        let output_budget =
            output_budget.context("interrupted tool call has no resolved output budget")?;
        let presentation = terminal_diagnostic_presentation(prefix, text, output_budget)?;
        let rendered = render_presentation(prefix, &presentation)?;
        return Ok(TerminalOutputPlan {
            raw: prefix,
            presentation,
            rendered,
            diagnostic: true,
        });
    }
    anyhow::ensure!(
        candidate_raw.starts_with(prefix),
        "terminal raw tool result is not an exact extension of persisted raw output"
    );
    let presentation = prepared.cloned().unwrap_or(PayloadPresentation::Full);
    let rendered = render_presentation(candidate_raw, &presentation)?;
    anyhow::ensure!(
        rendered == text,
        "prepared tool presentation does not reconstruct to the terminal native text"
    );
    Ok(TerminalOutputPlan {
        raw: candidate_raw,
        presentation,
        rendered,
        diagnostic: false,
    })
}

pub(super) fn terminal_metadata_matches(
    row: &serde_json::Value,
    fields: TerminalFields<'_>,
    terminal_status: &str,
) -> bool {
    row["status"].as_str() == Some(terminal_status)
        && row["tool_failure_class"].as_str() == fields.failure.map(FailureClass::as_str)
        && row["cancel_cause"].as_str() == fields.cancel.map(CancelCause::as_str)
}
