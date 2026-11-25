use anyhow::Context;
use serde_json::Value;

use crate::runner::embedded::observe::{
    classify_request_outcome, RequestEvidence, TerminalObservation,
};
use crate::{OutcomeKind, ProviderReason};
use gents::defra_node::Node as EmbeddedNode;
use gents_documents::TriggerObservation;
use gents_store::graphql::{escape_graphql_string, graphql_with_transaction_retry};

/// What a recorded inference failure says of its provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatedReason {
    pub reason: ProviderReason,
    /// The backend ended the request itself, whatever the subject did.
    pub ended_by_backend: bool,
}

/// External premise: an eval trial reads a request's persisted
/// `failure_reason`, the only record of why inference failed; the durable
/// AgentRequest schema carries no failure kind, so the provider's status and
/// wording are read from that text. Inference admission records exactly
/// `BackendGone` on a call of a backend it no longer serves.
pub fn provider_reason_from_failure(failure_reason: &str) -> Option<StatedReason> {
    if failure_reason == "BackendGone" {
        return Some(StatedReason {
            reason: ProviderReason::Unavailable,
            ended_by_backend: true,
        });
    }
    let worded = |reason| StatedReason {
        reason,
        ended_by_backend: false,
    };
    let reason = failure_reason.to_lowercase();
    if let Some(status) = http_status(&reason) {
        return Some(worded(match status {
            408 | 429 => ProviderReason::Unavailable,
            400..=499 => ProviderReason::Rejected,
            _ => ProviderReason::Unavailable,
        }));
    }
    if ["context", "policy", "content"]
        .iter()
        .any(|named| reason.contains(named))
    {
        return Some(worded(ProviderReason::Rejected));
    }
    if ["connect", "timeout", "timed out", "rate"]
        .iter()
        .any(|named| reason.contains(named))
    {
        return Some(worded(ProviderReason::Unavailable));
    }
    None
}

fn http_status(reason: &str) -> Option<u16> {
    reason
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|token| token.len() == 3 && token.bytes().all(|byte| byte.is_ascii_digit()))
        .filter_map(|token| token.parse::<u16>().ok())
        .find(|status| (400..600).contains(status))
}

pub(super) fn stage_failure_kind(
    observed: Option<&TerminalObservation>,
    collected: bool,
    stopped: bool,
    evidence: &RequestEvidence,
) -> Option<OutcomeKind> {
    if !collected {
        return Some(OutcomeKind::Infrastructure);
    }
    if stopped {
        return Some(OutcomeKind::Runtime);
    }
    match observed {
        Some(observed) => classify_request_outcome(
            observed.terminal_state,
            observed.interrupted_on_deadline,
            evidence,
        )
        .map(outcome_kind),
        None => Some(OutcomeKind::Infrastructure),
    }
}

pub(super) fn outcome_kind(classification: &str) -> OutcomeKind {
    match classification {
        "deadline" => OutcomeKind::Deadline,
        "tool" => OutcomeKind::Tool,
        "provider" => OutcomeKind::Provider,
        "runtime" => OutcomeKind::Runtime,
        "infrastructure" => OutcomeKind::Infrastructure,
        _ => OutcomeKind::Unknown,
    }
}

pub(super) fn stated_provider_reason(evidence: &RequestEvidence) -> Option<StatedReason> {
    let mut stated = evidence
        .inference_calls
        .iter()
        .filter_map(|call| call.failure_reason.as_deref())
        .filter_map(provider_reason_from_failure);
    let first = stated.next()?;
    Some(StatedReason {
        ended_by_backend: first.ended_by_backend || stated.any(|later| later.ended_by_backend),
        ..first
    })
}

/// A Trigger's recorded failure of a fire that wrote no request, as Lean
/// `Eval.FireFailure` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FireFailure {
    Error,
    BudgetExhausted,
    Unacknowledged,
}

impl FireFailure {
    fn parse(status: &str) -> Option<Self> {
        match status {
            "error" => Some(Self::Error),
            gents_documents::BUDGET_EXHAUSTED_STATUS => Some(Self::BudgetExhausted),
            gents_automation::triggers::UNACKNOWLEDGED_STATUS => Some(Self::Unacknowledged),
            _ => None,
        }
    }
}

/// Lean `Eval.fireFailureKind`.
pub(crate) fn fire_failure_kind(failures: &[FireFailure]) -> Option<OutcomeKind> {
    if failures
        .iter()
        .any(|failure| *failure != FireFailure::Unacknowledged)
    {
        Some(OutcomeKind::Runtime)
    } else if failures.is_empty() {
        None
    } else {
        Some(OutcomeKind::Infrastructure)
    }
}

/// Lean `Eval.stageOutcome`; `fires` is `None` when the trigger record could
/// not be read.
pub(super) fn stage_outcome(
    ended_by_backend: bool,
    request: Option<OutcomeKind>,
    stated: Option<ProviderReason>,
    fires: Option<&[FireFailure]>,
) -> Option<(OutcomeKind, Option<ProviderReason>)> {
    match request {
        Some(kind)
            if ended_by_backend && matches!(kind, OutcomeKind::Provider | OutcomeKind::Unknown) =>
        {
            Some((OutcomeKind::Provider, Some(ProviderReason::Unavailable)))
        }
        Some(kind) => Some((kind, stated.filter(|_| kind == OutcomeKind::Provider))),
        None => fires
            .map_or(Some(OutcomeKind::Infrastructure), fire_failure_kind)
            .map(|kind| (kind, None)),
    }
}

/// The fires recorded as failed since a stage began, with each Trigger's
/// recorded error for the verdict to carry.
#[derive(Debug, Default)]
pub(super) struct FailedFires {
    pub(super) failures: Vec<FireFailure>,
    pub(super) detail: Option<String>,
}

pub(super) async fn failed_fires(
    node: &EmbeddedNode,
    node_did: &str,
    since: &str,
    source_doc_id: Option<&str>,
) -> anyhow::Result<FailedFires> {
    let node_did = escape_graphql_string(node_did);
    let since = escape_graphql_string(since);
    let unacknowledged = gents_automation::triggers::UNACKNOWLEDGED_STATUS;
    let budget_exhausted = gents_documents::BUDGET_EXHAUSTED_STATUS;
    let source_filter = source_doc_id.map_or_else(String::new, |doc_id| {
        format!(
            r#", last_fired_source_doc_id: {{ _eq: "{}" }}"#,
            escape_graphql_string(doc_id)
        )
    });
    let query = format!(
        r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{node_did}" }}, last_status: {{ _in: ["error", "{unacknowledged}", "{budget_exhausted}"] }}, last_attempt_at: {{ _geq: "{since}" }}{source_filter} }}) {{ trigger_id last_attempt_at last_fired_source_doc_id last_status last_error }} }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "eval trial trigger status").await?;
    let failing = errored_triggers(response.data.as_ref())
        .context("the trigger status read had no Trigger list or a row that did not parse")?;
    let failures = failing
        .iter()
        .map(|trigger| {
            trigger
                .last_status
                .as_deref()
                .and_then(FireFailure::parse)
                .with_context(|| format!("trigger {} has no failed status", trigger.trigger_id))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let detail = (!failing.is_empty()).then(|| {
        failing
            .iter()
            .map(|trigger| {
                format!(
                    "trigger {} {}: {}",
                    trigger.trigger_id,
                    trigger.last_status.as_deref().unwrap_or_default(),
                    trigger.last_error.as_deref().unwrap_or("no error recorded")
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    });
    Ok(FailedFires { failures, detail })
}

fn errored_triggers(data: Option<&Value>) -> Option<Vec<TriggerObservation>> {
    serde_json::from_value(data?.get("Trigger")?.clone()).ok()
}

#[cfg(test)]
mod tests;
