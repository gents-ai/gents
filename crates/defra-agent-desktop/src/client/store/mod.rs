mod indexing;
mod turns;

use std::collections::HashMap;
use std::sync::Arc;

use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentConversationRow, AgentMessageRow, AgentPrincipalRow, AgentRequestRow,
    AgentResponseRow, AgentRuntimeRow, AgentSessionRow, AgentToolCallRow, AgentToolResultRow,
    CompactionEntryRow, EventTriggerRow, InferenceBackendRow, InferenceProfileRow, ScheduleRow,
    TaskRow, ToolSelectionRow, ToolServiceRegistryRow,
};
use serde::Serialize;

use self::indexing::{clean_string, indexes_to_refs};

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientStoreRows {
    pub agent_principals: Vec<AgentPrincipalRow>,
    pub behaviors: Vec<AgentBehaviorRow>,
    pub runtimes: Vec<AgentRuntimeRow>,
    pub conversations: Vec<AgentConversationRow>,
    pub requests: Vec<AgentRequestRow>,
    pub responses: Vec<AgentResponseRow>,
    pub messages: Vec<AgentMessageRow>,
    pub sessions: Vec<AgentSessionRow>,
    pub tool_calls: Vec<AgentToolCallRow>,
    pub tool_results: Vec<AgentToolResultRow>,
    pub compaction_entries: Vec<CompactionEntryRow>,
    pub tasks: Vec<TaskRow>,
    pub schedules: Vec<ScheduleRow>,
    pub event_triggers: Vec<EventTriggerRow>,
    pub tool_selections: Vec<ToolSelectionRow>,
    pub inference_backends: Vec<InferenceBackendRow>,
    pub inference_profiles: Vec<InferenceProfileRow>,
    pub tool_service_registries: Vec<ToolServiceRegistryRow>,
}

#[derive(Debug, Clone)]
pub struct ClientStore {
    pub agent_principals: Vec<AgentPrincipalRow>,
    pub behaviors: Vec<AgentBehaviorRow>,
    pub runtimes: Vec<AgentRuntimeRow>,
    pub conversations: Vec<AgentConversationRow>,
    pub requests: Vec<AgentRequestRow>,
    pub responses: Vec<AgentResponseRow>,
    pub messages: Vec<AgentMessageRow>,
    pub sessions: Vec<AgentSessionRow>,
    pub tool_calls: Vec<AgentToolCallRow>,
    pub tool_results: Vec<AgentToolResultRow>,
    pub compaction_entries: Vec<CompactionEntryRow>,
    pub tasks: Vec<TaskRow>,
    pub schedules: Vec<ScheduleRow>,
    pub event_triggers: Vec<EventTriggerRow>,
    pub tool_selections: Vec<ToolSelectionRow>,
    pub inference_backends: Vec<InferenceBackendRow>,
    pub inference_profiles: Vec<InferenceProfileRow>,
    pub tool_service_registries: Vec<ToolServiceRegistryRow>,
    conversations_by_agent_did: HashMap<String, Vec<usize>>,
    messages_by_session_id: HashMap<String, Vec<usize>>,
    requests_by_session_id: HashMap<String, Vec<usize>>,
    tool_calls_by_session_id: HashMap<String, Vec<usize>>,
    tool_results_by_session_id: HashMap<String, Vec<usize>>,
    runtimes_by_agent_did: HashMap<String, usize>,
    latest_response_by_request_id: HashMap<String, usize>,
    request_index_by_id: HashMap<String, usize>,
}

#[derive(Debug)]
pub struct TranscriptView<'a> {
    pub messages: Vec<&'a AgentMessageRow>,
    pub tool_calls: Vec<&'a AgentToolCallRow>,
    pub tool_results: Vec<&'a AgentToolResultRow>,
}

/// Aggregated recent-run bookkeeping for a task, rolled up across all
/// triggers (Schedule + EventTrigger) that reference it.
///
/// The apply path owns the `Task` description while the trigger engine
/// owns per-trigger fire bookkeeping on `Schedule` and `EventTrigger`.
/// Operators looking at a single task need to see "how often has this
/// task actually been fired, and what happened last time?" without
/// having to click into every trigger individually -- this struct rolls
/// those numbers up for the Task detail view.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TaskRecentRuns {
    pub total_fires: u64,
    pub last_attempt_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub schedule_count: usize,
    pub event_trigger_count: usize,
}

impl Default for ClientStore {
    fn default() -> Self {
        Self::from_rows(ClientStoreRows::default())
    }
}

impl ClientStore {
    pub fn default_behavior_id_for_agent(&self, agent_did: &str) -> Option<&str> {
        self.agent_principals
            .iter()
            .find(|row| row.agent_did == agent_did)
            .and_then(|row| row.default_behavior_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub fn behavior_rows(&self, agent_did: &str) -> Vec<&AgentBehaviorRow> {
        self.behaviors
            .iter()
            .filter(|row| row.agent_did.as_deref() == Some(agent_did))
            .collect()
    }

    pub fn behavior_row(&self, agent_did: &str, behavior_id: &str) -> Option<&AgentBehaviorRow> {
        self.behaviors.iter().find(|row| {
            row.agent_did.as_deref() == Some(agent_did) && row.behavior_id == behavior_id
        })
    }

    pub fn session_behavior_id(&self, session_id: &str, agent_did: Option<&str>) -> Option<String> {
        self.conversations
            .iter()
            .find(|row| {
                row.session_id == session_id
                    && agent_did.map_or(true, |agent_did| {
                        row.agent_did.as_deref() == Some(agent_did)
                    })
            })
            .and_then(|row| clean_string(row.behavior_id.as_deref()))
            .or_else(|| {
                self.sessions
                    .iter()
                    .find(|row| row.session_id == session_id)
                    .and_then(|row| clean_string(row.behavior_id.as_deref()))
            })
    }

    pub fn conversations_for_behavior(
        &self,
        agent_did: &str,
        behavior_id: &str,
    ) -> Vec<&AgentConversationRow> {
        self.conversations
            .iter()
            .filter(|row| {
                row.agent_did.as_deref() == Some(agent_did)
                    && clean_string(row.behavior_id.as_deref()).as_deref() == Some(behavior_id)
            })
            .collect()
    }

    pub fn requests_for_behavior(
        &self,
        agent_did: &str,
        behavior_id: &str,
    ) -> Vec<&AgentRequestRow> {
        self.requests
            .iter()
            .filter(|row| {
                row.agent_did.as_deref() == Some(agent_did)
                    && clean_string(row.behavior_id.as_deref()).as_deref() == Some(behavior_id)
            })
            .collect()
    }

    /// Return every `Task` bound to the given behavior.
    ///
    /// `Task` rows are not scoped by `agent_did` — they carry a single
    /// `behavior_id` and are addressed globally by `task_id`. The
    /// `_agent_did` parameter is kept so call sites that pass an agent scope
    /// (today's behavior-diagnostics view, for example) stay ergonomic; the
    /// filter is intentionally behavior-scoped only.
    pub fn tasks_for_behavior(&self, _agent_did: &str, behavior_id: &str) -> Vec<&TaskRow> {
        self.tasks
            .iter()
            .filter(|row| clean_string(row.behavior_id.as_deref()).as_deref() == Some(behavior_id))
            .collect()
    }

    /// Return every `Schedule` whose `task_id` matches one of the provided
    /// tasks. Useful for listing the schedules attached to a behavior
    /// indirectly (via its tasks).
    pub fn schedules_for_tasks(&self, task_ids: &[&str]) -> Vec<&ScheduleRow> {
        if task_ids.is_empty() {
            return Vec::new();
        }
        self.schedules
            .iter()
            .filter(|row| {
                row.task_id
                    .as_deref()
                    .is_some_and(|task_id| task_ids.iter().any(|candidate| *candidate == task_id))
            })
            .collect()
    }

    /// Return every `EventTrigger` whose `task_id` matches one of the
    /// provided tasks. Mirrors `schedules_for_tasks` so manage views can
    /// list the triggers attached to a behavior indirectly (via its
    /// tasks).
    pub fn event_triggers_for_tasks(&self, task_ids: &[&str]) -> Vec<&EventTriggerRow> {
        if task_ids.is_empty() {
            return Vec::new();
        }
        self.event_triggers
            .iter()
            .filter(|row| {
                row.task_id
                    .as_deref()
                    .is_some_and(|task_id| task_ids.iter().any(|candidate| *candidate == task_id))
            })
            .collect()
    }

    /// Roll up the trigger-engine bookkeeping for a `Task` across every
    /// `Schedule` and `EventTrigger` that references it.
    ///
    /// Both trigger kinds carry their own independent `fire_count`,
    /// `last_attempt_at`, `last_status`, and `last_error` fields. This
    /// helper sums the fires and picks the most recent `last_attempt_at`
    /// (lexicographic max on the ISO-8601 timestamp strings -- the
    /// trigger engine always writes RFC3339/Z-suffixed stamps, so
    /// lexical order matches chronological order), then surfaces the
    /// status/error from the trigger that produced that most-recent
    /// attempt. Used by the Task detail view to show operators a single
    /// rolled-up "Recent Runs" summary instead of forcing them to click
    /// into each individual trigger.
    pub fn recent_runs_for_task(&self, task_id: &str) -> TaskRecentRuns {
        let schedules: Vec<&ScheduleRow> = self
            .schedules
            .iter()
            .filter(|s| s.task_id.as_deref() == Some(task_id))
            .collect();
        let events: Vec<&EventTriggerRow> = self
            .event_triggers
            .iter()
            .filter(|t| t.task_id.as_deref() == Some(task_id))
            .collect();

        let total_fires = schedules
            .iter()
            .map(|s| s.fire_count.unwrap_or(0).max(0) as u64)
            .sum::<u64>()
            + events
                .iter()
                .map(|t| t.fire_count.unwrap_or(0).max(0) as u64)
                .sum::<u64>();

        // Find the most recent attempt_at across all triggers.
        let all_attempts: Vec<&str> = schedules
            .iter()
            .filter_map(|s| s.last_attempt_at.as_deref())
            .chain(events.iter().filter_map(|t| t.last_attempt_at.as_deref()))
            .collect();
        let last_attempt_at = all_attempts.iter().max().map(ToString::to_string);

        // Resolve status + error from the trigger whose timestamp
        // equals the max. Ties (two triggers firing in the same second
        // on the same task) resolve in favor of the first schedule
        // found, then the first event trigger found -- rare in
        // practice, and the operator still sees the aggregate
        // fire-count.
        let (last_status, last_error) = if let Some(ref target_ts) = last_attempt_at {
            let mut pair = None;
            for s in &schedules {
                if s.last_attempt_at.as_deref() == Some(target_ts.as_str()) {
                    pair = Some((s.last_status.clone(), s.last_error.clone()));
                    break;
                }
            }
            if pair.is_none() {
                for t in &events {
                    if t.last_attempt_at.as_deref() == Some(target_ts.as_str()) {
                        pair = Some((t.last_status.clone(), t.last_error.clone()));
                        break;
                    }
                }
            }
            pair.unwrap_or((None, None))
        } else {
            (None, None)
        };

        TaskRecentRuns {
            total_fires,
            last_attempt_at,
            last_status,
            last_error,
            schedule_count: schedules.len(),
            event_trigger_count: events.len(),
        }
    }

    pub fn conversation_rows(&self, agent_did: &str) -> Vec<&AgentConversationRow> {
        self.conversations_by_agent_did
            .get(agent_did)
            .into_iter()
            .flat_map(|indexes| indexes.iter())
            .map(|index| &self.conversations[*index])
            .collect()
    }

    pub fn transcript(&self, session_id: &str) -> TranscriptView<'_> {
        TranscriptView {
            messages: indexes_to_refs(&self.messages, self.messages_by_session_id.get(session_id)),
            tool_calls: indexes_to_refs(
                &self.tool_calls,
                self.tool_calls_by_session_id.get(session_id),
            ),
            tool_results: indexes_to_refs(
                &self.tool_results,
                self.tool_results_by_session_id.get(session_id),
            ),
        }
    }

    pub fn requests_for_session(&self, session_id: &str) -> Vec<&AgentRequestRow> {
        indexes_to_refs(&self.requests, self.requests_by_session_id.get(session_id))
    }

    pub fn latest_request_id_for_session(&self, session_id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|row| row.session_id == session_id)
            .and_then(|row| clean_string(row.latest_request_id.as_deref()))
            .or_else(|| {
                self.requests_by_session_id
                    .get(session_id)
                    .and_then(|indexes| indexes.last().copied())
                    .map(|index| self.requests[index].request_id.clone())
            })
    }

    pub fn latest_runtime(&self, agent_did: &str) -> Option<&AgentRuntimeRow> {
        self.runtimes_by_agent_did
            .get(agent_did)
            .map(|index| &self.runtimes[*index])
    }

    pub fn latest_response_for_request(&self, request_id: &str) -> Option<&AgentResponseRow> {
        self.latest_response_by_request_id
            .get(request_id)
            .map(|index| &self.responses[*index])
    }

    pub fn request_row(&self, request_id: &str) -> Option<&AgentRequestRow> {
        self.request_index_by_id
            .get(request_id)
            .map(|index| &self.requests[*index])
    }

    pub fn row_count(&self) -> usize {
        self.agent_principals.len()
            + self.behaviors.len()
            + self.runtimes.len()
            + self.conversations.len()
            + self.requests.len()
            + self.responses.len()
            + self.messages.len()
            + self.sessions.len()
            + self.tool_calls.len()
            + self.tool_results.len()
            + self.compaction_entries.len()
            + self.tasks.len()
            + self.schedules.len()
            + self.event_triggers.len()
            + self.tool_selections.len()
            + self.inference_backends.len()
            + self.inference_profiles.len()
            + self.tool_service_registries.len()
    }

    pub fn approx_serialized_bytes(&self) -> usize {
        serde_json::to_vec(&ClientStoreRows {
            agent_principals: self.agent_principals.clone(),
            behaviors: self.behaviors.clone(),
            runtimes: self.runtimes.clone(),
            conversations: self.conversations.clone(),
            requests: self.requests.clone(),
            responses: self.responses.clone(),
            messages: self.messages.clone(),
            sessions: self.sessions.clone(),
            tool_calls: self.tool_calls.clone(),
            tool_results: self.tool_results.clone(),
            compaction_entries: self.compaction_entries.clone(),
            tasks: self.tasks.clone(),
            schedules: self.schedules.clone(),
            event_triggers: self.event_triggers.clone(),
            tool_selections: self.tool_selections.clone(),
            inference_backends: self.inference_backends.clone(),
            inference_profiles: self.inference_profiles.clone(),
            tool_service_registries: self.tool_service_registries.clone(),
        })
        .map(|bytes| bytes.len())
        .unwrap_or_default()
    }

    pub fn derive_turn(&self, session_id: &str) -> Option<ClientTurnState> {
        turns::derive_turn(self, session_id)
    }

    pub fn derive_turn_for_request(&self, request_id: &str) -> Option<ClientTurnState> {
        turns::derive_turn_for_request(self, request_id)
    }
}

pub type SharedClientStore = Arc<ClientStore>;

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule_row(
        schedule_id: &str,
        task_id: &str,
        fire_count: Option<i64>,
        last_attempt_at: Option<&str>,
        last_status: Option<&str>,
        last_error: Option<&str>,
    ) -> ScheduleRow {
        ScheduleRow {
            schedule_id: schedule_id.to_string(),
            task_id: Some(task_id.to_string()),
            interval_secs: None,
            enabled: None,
            concurrency: None,
            next_run_at: None,
            last_attempt_at: last_attempt_at.map(str::to_string),
            last_status: last_status.map(str::to_string),
            last_error: last_error.map(str::to_string),
            fire_count,
            created_at: None,
            updated_at: None,
        }
    }

    fn event_trigger_row(
        trigger_id: &str,
        task_id: &str,
        fire_count: Option<i64>,
        last_attempt_at: Option<&str>,
        last_status: Option<&str>,
        last_error: Option<&str>,
    ) -> EventTriggerRow {
        EventTriggerRow {
            trigger_id: trigger_id.to_string(),
            task_id: Some(task_id.to_string()),
            source_collection: None,
            event_kind: None,
            filter: None,
            enabled: None,
            concurrency: None,
            created_at: None,
            updated_at: None,
            last_attempt_at: last_attempt_at.map(str::to_string),
            last_fired_source_doc_id: None,
            last_status: last_status.map(str::to_string),
            last_error: last_error.map(str::to_string),
            fire_count,
        }
    }

    #[test]
    fn recent_runs_aggregates_across_schedules_and_event_triggers() {
        let mut store = ClientStore::default();
        store.schedules.push(schedule_row(
            "s1",
            "task-1",
            Some(3),
            Some("2026-04-22T10:00:00Z"),
            Some("fired"),
            None,
        ));
        store.event_triggers.push(event_trigger_row(
            "t1",
            "task-1",
            Some(5),
            Some("2026-04-22T11:00:00Z"),
            Some("skipped"),
            Some("in-flight"),
        ));

        let runs = store.recent_runs_for_task("task-1");
        assert_eq!(runs.total_fires, 8);
        assert_eq!(
            runs.last_attempt_at.as_deref(),
            Some("2026-04-22T11:00:00Z")
        );
        assert_eq!(runs.last_status.as_deref(), Some("skipped"));
        assert_eq!(runs.last_error.as_deref(), Some("in-flight"));
        assert_eq!(runs.schedule_count, 1);
        assert_eq!(runs.event_trigger_count, 1);
    }

    #[test]
    fn recent_runs_empty_when_no_triggers() {
        let store = ClientStore::default();
        let runs = store.recent_runs_for_task("task-missing");
        assert_eq!(runs, TaskRecentRuns::default());
    }
}
