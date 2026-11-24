use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::report::build::SlotClass;
use crate::SubjectUsage;

pub fn figures(subject: &SubjectUsage) -> [(&'static str, Option<u64>); 19] {
    let first = subject.first_request.as_ref();
    let estimate = first.and_then(|first| first.estimate.as_ref());
    let tools = &subject.tool_calls;
    [
        (
            "first_request_prompt_tokens",
            first.and_then(|first| first.prompt_tokens),
        ),
        (
            "first_request_tool_schema_estimate",
            estimate.map(|estimate| estimate.tool_schemas),
        ),
        (
            "first_request_message_estimate",
            estimate.map(|estimate| estimate.messages),
        ),
        ("input_tokens", subject.turns.input_tokens),
        ("output_tokens", subject.turns.output_tokens),
        ("cached_input_tokens", subject.turns.cached_input_tokens),
        ("model_turns", Some(subject.turns.calls)),
        ("compaction_calls", Some(subject.compaction.calls)),
        ("direct_tool_calls", Some(tools.direct.calls)),
        ("failed_direct_tool_calls", Some(tools.direct.failed)),
        ("script_calls", Some(tools.script.calls)),
        ("failed_script_calls", Some(tools.script.failed)),
        ("inside_script_calls", Some(tools.inside_script.calls)),
        (
            "failed_inside_script_calls",
            Some(tools.inside_script.failed),
        ),
        ("wall_ms", subject.wall_ms),
        ("model_ms", subject.model_ms),
        ("tool_ms", subject.tool_ms),
        (
            "first_request_first_token_ms",
            first.and_then(|first| first.first_token_ms),
        ),
        ("median_first_token_ms", subject.median_first_token_ms),
    ]
}

pub fn figure(subject: &SubjectUsage, name: &str) -> Option<u64> {
    figures(subject)
        .into_iter()
        .find_map(|(named, value)| (named == name).then_some(value))
        .flatten()
}

/// `median` is over the trials that recorded the figure; `total` exists only
/// when every trial of the set recorded it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Figure {
    pub median: Option<u64>,
    pub total: Option<u64>,
    pub reported: u64,
}

impl Figure {
    pub fn of(values: impl IntoIterator<Item = Option<u64>>) -> Self {
        let mut trials = 0u64;
        let mut recorded: Vec<u64> = Vec::new();
        for value in values {
            trials += 1;
            recorded.extend(value);
        }
        recorded.sort_unstable();
        let reported = recorded.len() as u64;
        Self {
            median: median(&recorded),
            total: (reported == trials && trials > 0).then(|| {
                recorded
                    .iter()
                    .fold(0u64, |sum, value| sum.saturating_add(*value))
            }),
            reported,
        }
    }
}

pub fn median(sorted: &[u64]) -> Option<u64> {
    let middle = sorted.len() / 2;
    match sorted.len() {
        0 => None,
        length if length % 2 == 1 => Some(sorted[middle]),
        _ => Some(((u128::from(sorted[middle - 1]) + u128::from(sorted[middle])) / 2) as u64),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectSpend {
    pub trials: u64,
    pub figures: BTreeMap<String, Figure>,
}

impl SubjectSpend {
    pub fn of<'a>(subjects: impl IntoIterator<Item = Option<&'a SubjectUsage>>) -> Self {
        let subjects: Vec<Option<&SubjectUsage>> = subjects.into_iter().collect();
        let names = figures(&SubjectUsage::default()).map(|(name, _)| name);
        Self {
            trials: subjects.len() as u64,
            figures: names
                .into_iter()
                .map(|name| {
                    (
                        name.to_owned(),
                        Figure::of(
                            subjects
                                .iter()
                                .map(|subject| subject.and_then(|subject| figure(subject, name))),
                        ),
                    )
                })
                .collect(),
        }
    }
}

/// A not-evidence slot is an outage or an infrastructure failure, not the
/// subject's cost, so `evidence` is the passing, failing and unknown slots.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectSummary {
    pub evidence: SubjectSpend,
    pub passing: SubjectSpend,
}

impl SubjectSummary {
    pub fn of<'a>(slots: impl IntoIterator<Item = (SlotClass, Option<&'a SubjectUsage>)>) -> Self {
        let evidence: Vec<(SlotClass, Option<&SubjectUsage>)> = slots
            .into_iter()
            .filter(|(class, _)| {
                matches!(
                    class,
                    SlotClass::Pass | SlotClass::Fail | SlotClass::Unknown
                )
            })
            .collect();
        Self {
            evidence: SubjectSpend::of(evidence.iter().map(|(_, subject)| *subject)),
            passing: SubjectSpend::of(
                evidence
                    .iter()
                    .filter(|(class, _)| *class == SlotClass::Pass)
                    .map(|(_, subject)| *subject),
            ),
        }
    }
}
