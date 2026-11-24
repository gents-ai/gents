use serde_json::Value;

use crate::runner::embedded::observe::{
    classify_request_outcome, RequestEvidence, TerminalObservation,
};
use crate::{OutcomeKind, ProviderReason};
use gents::defra_node::Node as EmbeddedNode;
use gents_documents::TriggerObservation;
use gents_store::graphql::{escape_graphql_string, graphql_with_transaction_retry};

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
    worded_provider_reason(failure_reason).map(|reason| StatedReason {
        reason,
        ended_by_backend: false,
    })
}

/// What a recorded inference failure says of its provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatedReason {
    pub reason: ProviderReason,
    /// The backend ended the request itself, whatever the subject did.
    pub ended_by_backend: bool,
}

fn worded_provider_reason(failure_reason: &str) -> Option<ProviderReason> {
    let reason = failure_reason.to_lowercase();
    if let Some(status) = http_status(&reason) {
        return Some(match status {
            408 | 429 => ProviderReason::Unavailable,
            400..=499 => ProviderReason::Rejected,
            _ => ProviderReason::Unavailable,
        });
    }
    if ["context", "policy", "content"]
        .iter()
        .any(|named| reason.contains(named))
    {
        return Some(ProviderReason::Rejected);
    }
    if ["connect", "timeout", "timed out", "rate"]
        .iter()
        .any(|named| reason.contains(named))
    {
        return Some(ProviderReason::Unavailable);
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

pub(super) fn stage_outcome(
    ended_by_backend: bool,
    request: Option<OutcomeKind>,
    stated: Option<ProviderReason>,
) -> Option<(OutcomeKind, Option<ProviderReason>)> {
    let kind = request?;
    if ended_by_backend && matches!(kind, OutcomeKind::Provider | OutcomeKind::Unknown) {
        return Some((OutcomeKind::Provider, Some(ProviderReason::Unavailable)));
    }
    Some((kind, stated.filter(|_| kind == OutcomeKind::Provider)))
}

pub(super) async fn trigger_failure(
    node: &EmbeddedNode,
    node_did: &str,
    since: &str,
    source_doc_id: Option<&str>,
    trial_id: &str,
    stage_id: &str,
) -> Option<OutcomeKind> {
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
    let failing = match graphql_with_transaction_retry(node, &query, "eval trial trigger status")
        .await
    {
        Ok(response) => match errored_triggers(response.data.as_ref()) {
            Some(errored) => errored,
            None => {
                tracing::warn!(
                    trial_id = %trial_id,
                    stage_id = %stage_id,
                    "eval trial trigger status read had no Trigger list or a row that did not parse"
                );
                return Some(OutcomeKind::Infrastructure);
            }
        },
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %trial_id,
                stage_id = %stage_id,
                "eval trial trigger status could not be read"
            );
            return Some(OutcomeKind::Infrastructure);
        }
    };
    if failing.is_empty() {
        return None;
    }
    let kind = if failing.iter().any(|trigger| {
        [Some("error"), Some(budget_exhausted)].contains(&trigger.last_status.as_deref())
    }) {
        OutcomeKind::Runtime
    } else {
        OutcomeKind::Infrastructure
    };
    tracing::warn!(
        triggers = ?failing,
        outcome = kind.as_str(),
        trial_id = %trial_id,
        stage_id = %stage_id,
        "an eval trial trigger recorded a failed fire; the stage fails"
    );
    Some(kind)
}

fn errored_triggers(data: Option<&Value>) -> Option<Vec<TriggerObservation>> {
    serde_json::from_value(data?.get("Trigger")?.clone()).ok()
}

#[cfg(test)]
mod tests;
