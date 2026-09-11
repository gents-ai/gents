use gents_desktop_core::client::{ClientStore, TaskRecentRuns};
use gents_protocol::row::{AgentRequestRow, AgentResponseRow};
use gents_protocol::session::AgentSession;

use super::super::types::{
    normalize_optional, turn_state_label, SessionSummary, TaskRecentRunsView, TaskRunSummaryView,
    TaskView, TriggerView,
};

fn recent_runs_view(runs: &TaskRecentRuns) -> TaskRecentRunsView {
    TaskRecentRunsView {
        total_fires: runs.total_fires,
        last_attempt_at: normalize_optional(runs.last_attempt_at.as_deref()),
        last_status: normalize_optional(runs.last_status.as_deref()),
        last_error: normalize_optional(runs.last_error.as_deref()),
        schedule_count: runs.schedule_count,
        event_count: runs.event_count,
    }
}

pub(super) fn source_matches_agent(
    sources: &[Option<String>],
    row_index: usize,
    agent_did: &str,
    require_source_scope: bool,
) -> bool {
    match sources.get(row_index).and_then(|source| source.as_deref()) {
        Some(source_agent_did) => source_agent_did == agent_did,
        None => !require_source_scope,
    }
}

pub(super) fn request_matches_agent(request: &AgentRequestRow, agent_did: &str) -> bool {
    request.agent_did.as_deref() == Some(agent_did)
}

pub(super) fn recent_runs_for_task_views(
    triggers: &[TriggerView],
    agent_did: &str,
    task_id: &str,
) -> TaskRecentRunsView {
    let matching = triggers
        .iter()
        .filter(|trigger| {
            trigger.config.agent_did == agent_did && trigger.config.task_id == task_id
        })
        .collect::<Vec<_>>();
    let latest = matching
        .iter()
        .filter(|trigger| trigger.last_attempt_at.is_some())
        .max_by(|left, right| {
            left.last_attempt_at
                .cmp(&right.last_attempt_at)
                .then_with(|| left.config.trigger_id.cmp(&right.config.trigger_id))
        });
    recent_runs_view(&TaskRecentRuns {
        total_fires: matching
            .iter()
            .map(|trigger| trigger.fire_count.unwrap_or(0).max(0) as u64)
            .sum(),
        last_attempt_at: latest.and_then(|trigger| trigger.last_attempt_at.clone()),
        last_status: latest.and_then(|trigger| trigger.last_status.clone()),
        last_error: latest.and_then(|trigger| trigger.last_error.clone()),
        schedule_count: matching
            .iter()
            .filter(|trigger| {
                matches!(
                    trigger.config.source,
                    gents::document_config::TriggerSource::Schedule { .. }
                )
            })
            .count(),
        event_count: matching
            .iter()
            .filter(|trigger| {
                matches!(
                    trigger.config.source,
                    gents::document_config::TriggerSource::Event { .. }
                )
            })
            .count(),
    })
}

/// Canonical session indexes are the only list owner. A missing physical head
/// remains missing; request caches never synthesize sessions or select an older head.
pub(super) fn session_summaries(
    sessions: &[AgentSession],
    requests: &[AgentRequestRow],
    responses: &[AgentResponseRow],
    agent_did: &str,
    tasks: &[TaskView],
    triggers: &[TriggerView],
) -> Vec<SessionSummary> {
    let mut summaries = sessions
        .iter()
        .filter(|session| session.agent_did == agent_did)
        .map(|session| {
            let observation = session.observation.as_ref();
            let indexed = observation.and_then(|value| value.latest_request.as_ref());
            let mut exact_requests = requests.iter().filter(|request| {
                indexed.is_some_and(|indexed| {
                    request.doc_id.as_deref() == Some(indexed.request_doc_id.as_str())
                        && request.request_id == indexed.request_id
                }) && request.agent_did.as_deref() == Some(session.agent_did.as_str())
                    && request.session_id.as_deref() == Some(session.session_id.as_str())
                    && request.requester_did == session.requester_did
            });
            let candidate = exact_requests.next();
            let request = if exact_requests.next().is_none() {
                candidate
            } else {
                None
            };
            let mut exact_responses = responses.iter().filter(|response| {
                request.is_some_and(|request| {
                    response.request_doc_id == request.doc_id
                        && response.request_id.as_deref() == Some(request.request_id.as_str())
                        && response.agent_did == request.agent_did
                        && response.session_id == request.session_id
                        && response.requester_did == request.requester_did
                })
            });
            let response_candidate = exact_responses.next();
            let response = if exact_responses.next().is_none() {
                response_candidate
            } else {
                None
            };
            let lifecycle = request
                .and_then(|request| request.lifecycle_state)
                .or_else(|| indexed.map(|indexed| indexed.lifecycle_state));
            let trigger_id = request.and_then(|request| request.caused_by_trigger_id.clone());
            let trigger_kind = request.and_then(|request| request.caused_by_trigger_kind.clone());
            // Durable creation provenance wins over a mutable trigger's display label.
            let task_id = session
                .provenance
                .as_ref()
                .and_then(|value| value.task_id.clone())
                .or_else(|| {
                    triggers
                        .iter()
                        .find(|trigger| {
                            trigger.config.agent_did == session.agent_did
                                && Some(trigger.config.trigger_id.as_str()) == trigger_id.as_deref()
                        })
                        .map(|trigger| trigger.config.task_id.clone())
                });
            let task_name = task_id
                .as_deref()
                .and_then(|id| tasks.iter().find(|task| task.task_id == id))
                .and_then(|task| task.name.clone());
            SessionSummary {
                session_id: session.session_id.clone(),
                agent_did: session.agent_did.clone(),
                requester_did: session.requester_did.clone(),
                latest_request_doc_id: indexed.map(|indexed| indexed.request_doc_id.clone()),
                closed_at: session.closed_at.clone(),
                tags: session.tags.clone(),
                provenance: session.provenance.clone(),
                title: session.title.as_ref().map(|title| title.text.clone()),
                preview_text: observation.and_then(|value| value.preview.clone()),
                status: lifecycle.map(|state| state.as_str().to_owned()),
                behavior_id: Some(session.behavior_id.clone()),
                latest_request_id: indexed.map(|indexed| indexed.request_id.clone()),
                task_id,
                task_name,
                trigger_id,
                trigger_kind,
                created_at: Some(session.created_at.clone()),
                updated_at: observation.map(|value| value.last_activity_at.clone()),
                turn_state: lifecycle
                    .and_then(|state| {
                        gents_protocol::client_protocol::derive_persisted_attempt(
                            state.as_str(),
                            request.is_some_and(|request| request.superseded_by_request.is_some()),
                            response.and_then(|response| response.status.as_deref()),
                        )
                    })
                    .map(turn_state_label)
                    .map(str::to_owned),
                message_count: None,
                tool_call_count: None,
            }
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.session_id.cmp(&right.session_id))
            .then_with(|| left.requester_did.cmp(&right.requester_did))
    });
    summaries
}

pub(super) fn task_run_history(
    store: &ClientStore,
    agent_did: &str,
    task_id: &str,
    triggers: &[TriggerView],
) -> Vec<TaskRunSummaryView> {
    let trigger_ids = triggers
        .iter()
        .filter(|trigger| {
            trigger.config.agent_did == agent_did && trigger.config.task_id == task_id
        })
        .map(|trigger| trigger.config.trigger_id.as_str())
        .collect::<Vec<_>>();

    let mut runs = store
        .requests
        .iter()
        .filter(|request| {
            if !request_matches_agent(request, agent_did) {
                return false;
            }
            request
                .caused_by_trigger_id
                .as_deref()
                .is_some_and(|trigger_id| trigger_ids.contains(&trigger_id))
        })
        .map(|request| TaskRunSummaryView {
            request_id: request.request_id.clone(),
            request_doc_id: request.doc_id.clone(),
            agent_did: agent_did.to_owned(),
            requester_did: request.requester_did.clone(),
            session_id: normalize_optional(request.session_id.as_deref()),
            behavior_id: request.behavior_id.clone(),
            lifecycle_state: request
                .lifecycle_state
                .map(|state| state.as_str().to_string()),
            execution_origin: normalize_optional(request.execution_origin.as_deref()),
            caused_by_trigger_id: normalize_optional(request.caused_by_trigger_id.as_deref()),
            caused_by_trigger_kind: normalize_optional(request.caused_by_trigger_kind.as_deref()),
            created_at: normalize_optional(request.created_at.as_deref()),
        })
        .collect::<Vec<_>>();

    runs.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.request_id.cmp(&left.request_id))
    });
    runs.truncate(8);
    runs
}

#[cfg(test)]
mod trigger_recent_runs_tests {
    use super::*;

    #[test]
    fn counts_trigger_observations_once_and_keeps_owner_scope() {
        let trigger =
            |owner: &str, id: &str, source: serde_json::Value, fires, at: &str| TriggerView {
                config: serde_json::from_value(serde_json::json!({
                    "agent_did": owner, "trigger_id": id, "task_id": "task", "source": source
                }))
                .unwrap(),
                next_run_at: None,
                last_attempt_at: Some(at.into()),
                last_fired_source_doc_id: None,
                last_status: Some("completed".into()),
                last_error: None,
                fire_count: Some(fires),
            };
        let rows = vec![
            trigger(
                "owner",
                "scheduled-a",
                serde_json::json!({"kind":"schedule","schedule_id":"shared"}),
                2,
                "2026-09-01T00:00:00Z",
            ),
            trigger(
                "owner",
                "scheduled-b",
                serde_json::json!({"kind":"schedule","schedule_id":"shared"}),
                3,
                "2026-09-02T00:00:00Z",
            ),
            trigger(
                "owner",
                "event",
                serde_json::json!({"kind":"event","event_source_id":"source"}),
                4,
                "2026-09-03T00:00:00Z",
            ),
            trigger(
                "foreign",
                "event",
                serde_json::json!({"kind":"event","event_source_id":"source"}),
                100,
                "2026-09-04T00:00:00Z",
            ),
        ];
        let result = recent_runs_for_task_views(&rows, "owner", "task");
        assert_eq!(result.total_fires, 9);
        assert_eq!(result.schedule_count, 2);
        assert_eq!(result.event_count, 1);
        assert_eq!(
            result.last_attempt_at.as_deref(),
            Some("2026-09-03T00:00:00Z")
        );
        assert_eq!(
            recent_runs_for_task_views(&rows, "owner", "other").total_fires,
            0
        );
    }
}
