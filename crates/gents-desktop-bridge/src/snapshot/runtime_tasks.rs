use std::collections::{HashMap, HashSet};

use gents_desktop_core::client::{ClientStore, TaskRecentRuns};
use gents_protocol::row::AgentRequestRow;

use super::super::types::{
    normalize_optional, turn_state_label, ConversationSummary, EventTriggerView, ScheduleView,
    TaskRecentRunsView, TaskRunSummaryView, TaskView,
};

fn recent_runs_view(runs: &TaskRecentRuns) -> TaskRecentRunsView {
    TaskRecentRunsView {
        total_fires: runs.total_fires,
        last_attempt_at: normalize_optional(runs.last_attempt_at.as_deref()),
        last_status: normalize_optional(runs.last_status.as_deref()),
        last_error: normalize_optional(runs.last_error.as_deref()),
        schedule_count: runs.schedule_count,
        event_trigger_count: runs.event_trigger_count,
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

pub(super) fn request_matches_agent(
    request: &AgentRequestRow,
    agent_did: &str,
    require_source_scope: bool,
) -> bool {
    match request.agent_did.as_deref() {
        Some(request_agent_did) => request_agent_did == agent_did,
        None => !require_source_scope,
    }
}

pub(super) fn recent_runs_for_task_views(
    schedules: &[ScheduleView],
    event_triggers: &[EventTriggerView],
    task_id: &str,
) -> TaskRecentRunsView {
    let matching_schedules = schedules
        .iter()
        .filter(|schedule| schedule.task_id.as_deref() == Some(task_id))
        .collect::<Vec<_>>();
    let matching_events = event_triggers
        .iter()
        .filter(|trigger| trigger.task_id.as_deref() == Some(task_id))
        .collect::<Vec<_>>();

    let total_fires = matching_schedules
        .iter()
        .map(|schedule| schedule.fire_count.unwrap_or(0).max(0) as u64)
        .sum::<u64>()
        + matching_events
            .iter()
            .map(|trigger| trigger.fire_count.unwrap_or(0).max(0) as u64)
            .sum::<u64>();
    let last_attempt_at = matching_schedules
        .iter()
        .filter_map(|schedule| schedule.last_attempt_at.as_deref())
        .chain(
            matching_events
                .iter()
                .filter_map(|trigger| trigger.last_attempt_at.as_deref()),
        )
        .max()
        .map(str::to_owned);

    let (last_status, last_error) = if let Some(target_ts) = last_attempt_at.as_deref() {
        matching_schedules
            .iter()
            .find(|schedule| schedule.last_attempt_at.as_deref() == Some(target_ts))
            .map(|schedule| (schedule.last_status.clone(), schedule.last_error.clone()))
            .or_else(|| {
                matching_events
                    .iter()
                    .find(|trigger| trigger.last_attempt_at.as_deref() == Some(target_ts))
                    .map(|trigger| (trigger.last_status.clone(), trigger.last_error.clone()))
            })
            .unwrap_or((None, None))
    } else {
        (None, None)
    };

    recent_runs_view(&TaskRecentRuns {
        total_fires,
        last_attempt_at,
        last_status,
        last_error,
        schedule_count: matching_schedules.len(),
        event_trigger_count: matching_events.len(),
    })
}

pub(super) fn retain_latest_conversation_summaries(conversations: &mut Vec<ConversationSummary>) {
    let mut seen_sessions = HashSet::new();
    conversations.retain(|conversation| seen_sessions.insert(conversation.session_id.clone()));
}

pub(super) fn request_backed_conversation_summaries(
    store: &ClientStore,
    agent_did: &str,
    require_source_scope: bool,
    tasks: &[TaskView],
    schedules: &[ScheduleView],
    event_triggers: &[EventTriggerView],
) -> Vec<ConversationSummary> {
    let existing_sessions = store
        .conversation_rows(agent_did)
        .into_iter()
        .map(|row| row.session_id.clone())
        .collect::<HashSet<_>>();
    let mut latest_requests_by_session: HashMap<String, &AgentRequestRow> = HashMap::new();

    for request in &store.requests {
        if !request_matches_agent(request, agent_did, require_source_scope) {
            continue;
        }
        let Some(session_id) = normalize_optional(request.session_id.as_deref()) else {
            continue;
        };
        if existing_sessions.contains(&session_id) {
            continue;
        }

        let replace = latest_requests_by_session
            .get(&session_id)
            .is_none_or(|current| compare_request_freshness(request, current).is_gt());
        if replace {
            latest_requests_by_session.insert(session_id, request);
        }
    }

    latest_requests_by_session
        .into_iter()
        .map(|(session_id, request)| {
            let task_tag = conversation_task_tag(
                store,
                agent_did,
                require_source_scope,
                &session_id,
                tasks,
                schedules,
                event_triggers,
            );
            let latest_request_id = store
                .latest_request_id_for_session_for_agent(&session_id, agent_did)
                .unwrap_or_else(|| request.request_id.clone());
            let latest_response =
                store.latest_response_for_request_for_agent(&latest_request_id, agent_did);
            let updated_at = latest_response
                .and_then(|row| {
                    normalize_optional(row.completed_at.as_deref())
                        .or_else(|| normalize_optional(row.created_at.as_deref()))
                })
                .or_else(|| normalize_optional(request.created_at.as_deref()));

            ConversationSummary {
                session_id,
                title: None,
                preview_text: normalize_optional(request.content.as_deref()),
                status: request
                    .lifecycle_state
                    .map(|state| state.as_str().to_string()),
                behavior_id: normalize_optional(request.behavior_id.as_deref()),
                latest_request_id: Some(latest_request_id.clone()),
                task_id: task_tag.as_ref().map(|tag| tag.task_id.clone()),
                task_name: task_tag.as_ref().and_then(|tag| tag.task_name.clone()),
                trigger_id: task_tag.as_ref().and_then(|tag| tag.trigger_id.clone()),
                trigger_kind: task_tag.as_ref().and_then(|tag| tag.trigger_kind.clone()),
                created_at: normalize_optional(request.created_at.as_deref()),
                updated_at,
                turn_state: store
                    .derive_turn_for_request(&latest_request_id)
                    .map(turn_state_label)
                    .map(str::to_owned),
                message_count: None,
                tool_call_count: None,
            }
        })
        .collect()
}

fn compare_request_freshness(
    left: &AgentRequestRow,
    right: &AgentRequestRow,
) -> std::cmp::Ordering {
    left.created_at
        .cmp(&right.created_at)
        .then_with(|| left.request_id.cmp(&right.request_id))
}

#[derive(Debug)]
pub(super) struct ConversationTaskTag {
    pub(super) task_id: String,
    pub(super) task_name: Option<String>,
    pub(super) trigger_id: Option<String>,
    pub(super) trigger_kind: Option<String>,
}

pub(super) fn conversation_task_tag(
    store: &ClientStore,
    agent_did: &str,
    require_source_scope: bool,
    session_id: &str,
    tasks: &[TaskView],
    schedules: &[ScheduleView],
    event_triggers: &[EventTriggerView],
) -> Option<ConversationTaskTag> {
    let mut requests = store.requests_for_session(session_id);
    requests.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.request_id.cmp(&left.request_id))
    });

    requests.into_iter().find_map(|request| {
        if !request_matches_agent(request, agent_did, require_source_scope) {
            return None;
        }
        let trigger_kind = normalize_optional(request.caused_by_trigger_kind.as_deref())?;
        let trigger_id = normalize_optional(request.caused_by_trigger_id.as_deref());
        let task_id = match (trigger_kind.as_str(), trigger_id.as_deref()) {
            ("schedule", Some(trigger_id)) => schedules
                .iter()
                .find(|schedule| schedule.schedule_id == trigger_id)
                .and_then(|schedule| schedule.task_id.clone()),
            ("event", Some(trigger_id)) => event_triggers
                .iter()
                .find(|trigger| trigger.trigger_id == trigger_id)
                .and_then(|trigger| trigger.task_id.clone()),
            _ => None,
        }?;
        let task_name = tasks
            .iter()
            .find(|task| task.task_id == task_id)
            .and_then(|task| task.name.clone());

        Some(ConversationTaskTag {
            task_id,
            task_name,
            trigger_id,
            trigger_kind: Some(trigger_kind),
        })
    })
}

pub(super) fn task_run_history(
    store: &ClientStore,
    agent_did: &str,
    require_source_scope: bool,
    task_id: &str,
    schedules: &[ScheduleView],
    event_triggers: &[EventTriggerView],
) -> Vec<TaskRunSummaryView> {
    let schedule_ids = schedules
        .iter()
        .filter(|schedule| schedule.task_id.as_deref() == Some(task_id))
        .map(|schedule| schedule.schedule_id.as_str())
        .collect::<Vec<_>>();
    let event_trigger_ids = event_triggers
        .iter()
        .filter(|trigger| trigger.task_id.as_deref() == Some(task_id))
        .map(|trigger| trigger.trigger_id.as_str())
        .collect::<Vec<_>>();

    let mut runs = store
        .requests
        .iter()
        .filter(|request| {
            if !request_matches_agent(request, agent_did, require_source_scope) {
                return false;
            }
            match (
                request.caused_by_trigger_kind.as_deref(),
                request.caused_by_trigger_id.as_deref(),
            ) {
                (Some("schedule"), Some(trigger_id)) => schedule_ids.contains(&trigger_id),
                (Some("event"), Some(trigger_id)) => event_trigger_ids.contains(&trigger_id),
                _ => false,
            }
        })
        .map(|request| TaskRunSummaryView {
            request_id: request.request_id.clone(),
            session_id: normalize_optional(request.session_id.as_deref()),
            behavior_id: normalize_optional(request.behavior_id.as_deref()),
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
