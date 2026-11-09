use super::*;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GoalDocument {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub goal_id: String,
    pub session_id: String,
    pub node_did: String,
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub token_budget: Option<i64>,
    #[serde(default)]
    pub assignment_root_request_doc_id: Option<String>,
    #[serde(default)]
    pub tokens_used: Option<i64>,
    #[serde(default)]
    pub token_usage_baseline: Option<i64>,
    #[serde(default)]
    pub active_time_seconds: Option<i64>,
    #[serde(default)]
    pub active_started_at: Option<String>,
    #[serde(default)]
    pub consecutive_blocked_audits: Option<i64>,
    #[serde(default)]
    pub last_blocked_request_id: Option<String>,
    #[serde(default)]
    pub last_blocked_reason: Option<String>,
    #[serde(default)]
    pub last_continued_from_request_id: Option<String>,
    #[serde(default)]
    pub continuation_sequence: Option<i64>,
    #[serde(default)]
    pub wrapup_requested: Option<bool>,
    #[serde(default)]
    pub wrapup_completed: Option<bool>,
    #[serde(default)]
    pub infrastructure_retry_count: Option<i64>,
    #[serde(default)]
    pub last_failure: Option<String>,
    #[serde(default)]
    pub completion_evidence: Option<String>,
    #[serde(default)]
    pub auto_resume_at_reset: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl GoalDocument {
    pub fn parsed_status(&self) -> Option<GoalStatus> {
        GoalStatus::parse(&self.status)
    }

    pub fn state(&self) -> Option<GoalState> {
        Some(GoalState {
            status: self.parsed_status()?,
            blocked_audits: self.consecutive_blocked_audits.unwrap_or_default().max(0),
            wrapup_requested: self.wrapup_requested.unwrap_or(false),
            wrapup_completed: self.wrapup_completed.unwrap_or(false),
        })
    }

    pub fn current_active_time_seconds(&self, now: DateTime<Utc>) -> i64 {
        let persisted = self.active_time_seconds.unwrap_or_default().max(0);
        if self.parsed_status() != Some(GoalStatus::Active) {
            return persisted;
        }
        let elapsed = self
            .active_started_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|started| (now - started.with_timezone(&Utc)).num_seconds().max(0))
            .unwrap_or_default();
        persisted.saturating_add(elapsed)
    }

    pub fn continuation_sequence(&self) -> i64 {
        self.continuation_sequence.unwrap_or_default().max(0)
    }
}

pub fn snapshot(goal: &GoalDocument, now: DateTime<Utc>) -> GoalSnapshot {
    GoalSnapshot {
        goal_id: goal.goal_id.clone(),
        session_id: goal.session_id.clone(),
        node_did: goal.node_did.clone(),
        objective: goal.objective.clone(),
        status: goal.status.clone(),
        token_budget: goal.token_budget,
        tokens_used: goal.tokens_used.unwrap_or_default().max(0),
        active_time_seconds: goal.current_active_time_seconds(now),
        consecutive_blocked_audits: goal.consecutive_blocked_audits.unwrap_or_default().max(0),
        continuation_sequence: goal.continuation_sequence(),
        wrapup_requested: goal.wrapup_requested.unwrap_or(false),
        wrapup_completed: goal.wrapup_completed.unwrap_or(false),
        last_blocked_reason: goal.last_blocked_reason.clone(),
        last_failure: goal.last_failure.clone(),
        completion_evidence: goal.completion_evidence.clone(),
        created_at: goal.created_at.clone(),
        updated_at: goal.updated_at.clone(),
    }
}
