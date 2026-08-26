use super::*;

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
                    && agent_did.is_none_or(|agent_did| row.agent_did.as_deref() == Some(agent_did))
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
                    .is_some_and(|task_id| task_ids.contains(&task_id))
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
                    .is_some_and(|task_id| task_ids.contains(&task_id))
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
}
