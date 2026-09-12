use super::*;

impl ClientStore {
    pub fn default_behavior_id_for_agent(&self, agent_did: &str) -> Option<&str> {
        let from_principal = self
            .agent_principals
            .iter()
            .find(|row| row.agent_did == agent_did)
            .and_then(|row| row.default_behavior_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if from_principal.is_some() {
            return from_principal;
        }

        let behaviors = self.behavior_rows(agent_did);
        let conventional = gents::default_behavior_id_for_agent(agent_did);
        if let Some(row) = behaviors.iter().find(|row| row.behavior_id == conventional) {
            return Some(row.behavior_id.as_str());
        }
        let enabled = behaviors
            .iter()
            .filter(|row| row.enabled)
            .collect::<Vec<_>>();
        (enabled.len() == 1).then(|| enabled[0].behavior_id.as_str())
    }

    pub fn behavior_rows(&self, agent_did: &str) -> Vec<&AgentBehavior> {
        self.behaviors
            .iter()
            .filter(|row| row.agent_did == agent_did)
            .collect()
    }

    pub fn behavior_row(&self, agent_did: &str, behavior_id: &str) -> Option<&AgentBehavior> {
        self.behaviors
            .iter()
            .find(|row| row.agent_did == agent_did && row.behavior_id == behavior_id)
    }

    pub fn session_behavior_id(&self, session_id: &str, agent_did: Option<&str>) -> Option<String> {
        self.sessions
            .iter()
            .find(|row| {
                row.session_id == session_id
                    && agent_did.is_none_or(|agent_did| row.agent_did == agent_did)
            })
            .and_then(|row| clean_string(Some(&row.behavior_id)))
    }

    pub fn sessions_for_behavior(&self, agent_did: &str, behavior_id: &str) -> Vec<&AgentSession> {
        self.sessions
            .iter()
            .filter(|row| {
                row.agent_did == agent_did
                    && clean_string(Some(&row.behavior_id)).as_deref() == Some(behavior_id)
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
    pub fn tasks_for_behavior(&self, agent_did: &str, behavior_id: &str) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|row| row.agent_did == agent_did && row.behavior_id == behavior_id)
            .collect()
    }

    /// Return every `Trigger` whose `task_id` matches one of the
    /// provided tasks.
    pub fn triggers_for_tasks(&self, task_ids: &[&str]) -> Vec<&Trigger> {
        if task_ids.is_empty() {
            return Vec::new();
        }
        self.triggers
            .iter()
            .filter(|row| task_ids.contains(&row.task_id.as_str()))
            .collect()
    }

    /// Roll up trigger-engine bookkeeping for a `Task` across every
    /// canonical `Trigger` that references it.
    ///
    /// Each trigger has independent `fire_count`,
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
        let triggers: Vec<&Trigger> = self
            .triggers
            .iter()
            .filter(|trigger| trigger.task_id == task_id)
            .collect();
        let observations = triggers
            .iter()
            .filter_map(|trigger| {
                self.trigger_observations
                    .iter()
                    .enumerate()
                    .find(|(index, observation)| {
                        observation.trigger_id == trigger.trigger_id
                            && source_agent_matches(
                                &self.trigger_observation_source_agent_dids,
                                *index,
                                &trigger.agent_did,
                            )
                    })
                    .map(|(_, observation)| observation)
            })
            .collect::<Vec<_>>();

        let total_fires = observations
            .iter()
            .map(|observation| observation.fire_count.unwrap_or(0).max(0) as u64)
            .sum::<u64>();

        // Find the most recent attempt_at across all triggers.
        let all_attempts: Vec<&str> = observations
            .iter()
            .filter_map(|observation| observation.last_attempt_at.as_deref())
            .collect();
        let last_attempt_at = all_attempts.iter().max().map(ToString::to_string);

        // Resolve status + error from the trigger whose timestamp equals the max.
        let (last_status, last_error) = if let Some(ref target_ts) = last_attempt_at {
            let mut pair = None;
            for observation in &observations {
                if observation.last_attempt_at.as_deref() == Some(target_ts.as_str()) {
                    pair = Some((
                        observation.last_status.clone(),
                        observation.last_error.clone(),
                    ));
                    break;
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
            schedule_count: triggers
                .iter()
                .filter(|trigger| {
                    matches!(
                        trigger.source,
                        gents::document_config::TriggerSource::Schedule { .. }
                    )
                })
                .count(),
            event_count: triggers
                .iter()
                .filter(|trigger| {
                    matches!(
                        trigger.source,
                        gents::document_config::TriggerSource::Event { .. }
                    )
                })
                .count(),
        }
    }

    pub fn session_rows(&self, agent_did: &str) -> Vec<&AgentSession> {
        self.sessions
            .iter()
            .filter(|session| session.agent_did == agent_did)
            .collect()
    }
}
