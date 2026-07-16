//! Durable, session-scoped autonomous goals.
//!
//! A goal is control-plane state, not completion-loop memory. Helpers key
//! every read and write by the agent/session pair and select a deterministic
//! canonical row when replication or a create acknowledgement gap leaves
//! duplicates behind.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

pub const GOAL_TRIGGER_KIND: &str = "goal";
pub const GET_GOAL_TOOL_NAME: &str = "get_goal";
pub const UPDATE_GOAL_TOOL_NAME: &str = "update_goal";
pub const BLOCKED_AUDIT_THRESHOLD: i64 = 3;
pub const MAX_INFRASTRUCTURE_RETRIES: i64 = 2;

const GOAL_FIELDS: &str = r#"
    _docID
    goal_id
    session_id
    agent_did
    objective
    status
    token_budget
    tokens_used
    active_time_seconds
    active_started_at
    consecutive_blocked_audits
    last_blocked_request_id
    last_continued_from_request_id
    continuation_sequence
    wrapup_requested
    wrapup_completed
    infrastructure_retry_count
    last_failure
    created_at
    updated_at
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalRequestTerminal {
    Completed,
    Failed,
    Dead,
    Interrupted,
    Superseded,
}

impl GoalRequestTerminal {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "dead" => Some(Self::Dead),
            "interrupted" => Some(Self::Interrupted),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalDecision {
    None,
    Continue,
    Retry,
    Pause,
    Wrapup,
}

/// Advances the durable blocked-condition audit without double-counting a
/// repeated tool call from the same request. A changed reason starts a fresh
/// audit because it is a different blocking condition.
pub fn next_blocked_audit(
    previous_count: i64,
    previous_reason: Option<&str>,
    previous_request_id: Option<&str>,
    reason: &str,
    request_id: &str,
) -> (i64, bool) {
    let audits = if previous_request_id == Some(request_id) {
        previous_count.max(0)
    } else if previous_reason == Some(reason) {
        previous_count.max(0).saturating_add(1)
    } else {
        1
    };
    (audits, audits >= BLOCKED_AUDIT_THRESHOLD)
}

/// Executable mirror of `Goals.decide` in `proofs/Proofs/Goals.lean`.
#[allow(clippy::too_many_arguments)]
pub fn decide_goal_continuation(
    status: GoalStatus,
    terminal: GoalRequestTerminal,
    session_idle: bool,
    child_exists: bool,
    budget_reached: bool,
    has_activity: bool,
    infrastructure_retries: i64,
    wrapup_requested: bool,
    wrapup_completed: bool,
) -> GoalDecision {
    if !session_idle || child_exists {
        return GoalDecision::None;
    }
    match status {
        GoalStatus::Active => match terminal {
            GoalRequestTerminal::Interrupted | GoalRequestTerminal::Superseded => {
                GoalDecision::Pause
            }
            GoalRequestTerminal::Failed | GoalRequestTerminal::Dead => {
                if infrastructure_retries.max(0) < MAX_INFRASTRUCTURE_RETRIES {
                    GoalDecision::Retry
                } else {
                    GoalDecision::Pause
                }
            }
            GoalRequestTerminal::Completed => {
                if !has_activity {
                    GoalDecision::Pause
                } else if budget_reached {
                    GoalDecision::Wrapup
                } else {
                    GoalDecision::Continue
                }
            }
        },
        GoalStatus::BudgetLimited => {
            if wrapup_requested && !wrapup_completed {
                GoalDecision::Wrapup
            } else {
                GoalDecision::None
            }
        }
        GoalStatus::Paused
        | GoalStatus::Blocked
        | GoalStatus::UsageLimited
        | GoalStatus::Complete => GoalDecision::None,
    }
}

impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::UsageLimited => "usage_limited",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "blocked" => Some(Self::Blocked),
            "usage_limited" | "usageLimited" => Some(Self::UsageLimited),
            "budget_limited" | "budgetLimited" => Some(Self::BudgetLimited),
            "complete" => Some(Self::Complete),
            _ => None,
        }
    }

    pub fn accrues_active_time(self) -> bool {
        self == Self::Active
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GoalDocument {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub goal_id: String,
    pub session_id: String,
    pub agent_did: String,
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub token_budget: Option<i64>,
    #[serde(default)]
    pub tokens_used: Option<i64>,
    #[serde(default)]
    pub active_time_seconds: Option<i64>,
    #[serde(default)]
    pub active_started_at: Option<String>,
    #[serde(default)]
    pub consecutive_blocked_audits: Option<i64>,
    #[serde(default)]
    pub last_blocked_request_id: Option<String>,
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
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl GoalDocument {
    pub fn parsed_status(&self) -> Option<GoalStatus> {
        GoalStatus::parse(&self.status)
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

#[derive(Debug, Clone, Serialize)]
pub struct GoalSnapshot {
    pub goal_id: String,
    pub session_id: String,
    pub agent_did: String,
    pub objective: String,
    pub status: String,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub active_time_seconds: i64,
    pub consecutive_blocked_audits: i64,
    pub continuation_sequence: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl GoalSnapshot {
    pub fn from_document(goal: &GoalDocument, now: DateTime<Utc>) -> Self {
        Self {
            goal_id: goal.goal_id.clone(),
            session_id: goal.session_id.clone(),
            agent_did: goal.agent_did.clone(),
            objective: goal.objective.clone(),
            status: goal.status.clone(),
            token_budget: goal.token_budget,
            tokens_used: goal.tokens_used.unwrap_or_default().max(0),
            active_time_seconds: goal.current_active_time_seconds(now),
            consecutive_blocked_audits: goal.consecutive_blocked_audits.unwrap_or_default().max(0),
            continuation_sequence: goal.continuation_sequence(),
            wrapup_requested: goal.wrapup_requested.unwrap_or(false),
            wrapup_completed: goal.wrapup_completed.unwrap_or(false),
            created_at: goal.created_at.clone(),
            updated_at: goal.updated_at.clone(),
        }
    }
}

fn deterministic_goal_id(agent_did: &str, session_id: &str) -> String {
    format!("{}:{agent_did}:{session_id}", agent_did.len())
}

pub async fn load_goals_for_session(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
) -> Result<Vec<GoalDocument>> {
    let agent_did = escape_graphql_string(agent_did);
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    session_id: {{ _eq: "{session_id}" }}
                }},
                order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
            ) {{ {GOAL_FIELDS} }}
        }}"#
    );
    let mut goals = decode_goal_rows(node, &query).await?;
    sort_goals_canonical(&mut goals);
    Ok(goals)
}

pub async fn load_canonical_goal(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
) -> Result<Option<GoalDocument>> {
    Ok(load_goals_for_session(node, agent_did, session_id)
        .await?
        .into_iter()
        .next())
}

pub async fn load_goal_by_id(
    node: &EmbeddedNode,
    agent_did: &str,
    goal_id: &str,
) -> Result<Option<GoalDocument>> {
    let agent_did = escape_graphql_string(agent_did);
    let goal_id = escape_graphql_string(goal_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }}, goal_id: {{ _eq: "{goal_id}" }} }},
                order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
            ) {{ {GOAL_FIELDS} }}
        }}"#
    );
    let mut goals = decode_goal_rows(node, &query).await?;
    sort_goals_canonical(&mut goals);
    Ok(goals.into_iter().next())
}

fn sort_goals_canonical(goals: &mut [GoalDocument]) {
    goals.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.goal_id.cmp(&right.goal_id))
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
}

async fn decode_goal_rows(node: &EmbeddedNode, query: &str) -> Result<Vec<GoalDocument>> {
    let response = node.execute(query).await;
    if response.has_errors() {
        bail!("Goal query failed: {:?}", response.errors);
    }
    serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("Goal"))
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .context("decoding Goal rows")
}

pub async fn set_goal(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<GoalDocument> {
    let existing = load_canonical_goal(node, agent_did, session_id).await?;
    let objective = objective
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| existing.as_ref().map(|goal| goal.objective.clone()))
        .context("a goal objective is required")?;
    let status = status
        .or_else(|| existing.as_ref().and_then(GoalDocument::parsed_status))
        .unwrap_or(GoalStatus::Active);
    let budget =
        token_budget.unwrap_or_else(|| existing.as_ref().and_then(|goal| goal.token_budget));
    if budget.is_some_and(|value| value <= 0) {
        bail!("goal token budget must be positive");
    }

    let now = Utc::now();
    let now_string = now.to_rfc3339();
    if let Some(existing) = existing {
        let active_time = existing.current_active_time_seconds(now);
        // `active_time_seconds` has just absorbed the current active segment,
        // so a still-active goal starts a fresh segment at this write. Keeping
        // the old timestamp here would charge the same elapsed time twice.
        let active_started_at = if status.accrues_active_time() {
            now_string.clone()
        } else {
            String::new()
        };
        let budget_field = optional_int_graphql_field("token_budget", budget);
        let active_started_field = optional_string_graphql_field(
            "active_started_at",
            (!active_started_at.is_empty()).then_some(active_started_at.as_str()),
        );
        let doc_id = escape_graphql_string(&existing.doc_id);
        let agent_did = escape_graphql_string(agent_did);
        let objective = escape_graphql_string(&objective);
        let status = status.as_str();
        let now = escape_graphql_string(&now_string);
        let mutation = format!(
            r#"mutation {{
                update_Goal(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{agent_did}" }} }},
                    input: {{
                        objective: "{objective}",
                        status: "{status}",
                        {budget_field}
                        tokens_used: {tokens_used},
                        active_time_seconds: {active_time},
                        {active_started_field}
                        updated_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#,
            tokens_used = existing.tokens_used.unwrap_or_default().max(0),
        );
        execute_goal_mutation(node, &mutation, "update goal").await?;
        return load_goal_by_doc_id(node, &existing.doc_id)
            .await?
            .context("updated Goal row disappeared");
    }

    let goal_id = deterministic_goal_id(agent_did, session_id);
    let escaped_goal_id = escape_graphql_string(&goal_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_objective = escape_graphql_string(&objective);
    let escaped_now = escape_graphql_string(&now_string);
    let budget_field = optional_int_graphql_field("token_budget", budget);
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        status.accrues_active_time().then_some(now_string.as_str()),
    );
    let mutation = format!(
        r#"mutation {{
            create_Goal(input: {{
                goal_id: "{escaped_goal_id}",
                session_id: "{escaped_session_id}",
                agent_did: "{escaped_agent_did}",
                objective: "{escaped_objective}",
                status: "{status}",
                {budget_field}
                tokens_used: 0,
                active_time_seconds: 0,
                {active_started_field}
                consecutive_blocked_audits: 0,
                continuation_sequence: 0,
                wrapup_requested: false,
                wrapup_completed: false,
                infrastructure_retry_count: 0,
                created_at: "{escaped_now}",
                updated_at: "{escaped_now}"
            }}) {{ _docID }}
        }}"#,
        status = status.as_str(),
    );
    execute_goal_mutation(node, &mutation, "create goal").await?;
    load_canonical_goal(node, agent_did, session_id)
        .await?
        .context("created Goal row not found")
}

pub async fn delete_goal(node: &EmbeddedNode, goal: &GoalDocument) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let agent_did = escape_graphql_string(&goal.agent_did);
    let mutation = format!(
        r#"mutation {{
            delete_Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{agent_did}" }} }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!("delete goal failed: {:?}", response.errors);
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub async fn update_goal_fields(
    node: &EmbeddedNode,
    goal: &GoalDocument,
    fields: &str,
) -> Result<()> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let agent_did = escape_graphql_string(&goal.agent_did);
    let mutation = format!(
        r#"mutation {{
            update_Goal(
                filter: {{ _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{agent_did}" }} }},
                input: {{ {fields} }}
            ) {{ _docID }}
        }}"#
    );
    execute_goal_mutation(node, &mutation, "update goal fields").await
}

pub async fn claim_continuation(
    node: &EmbeddedNode,
    goal: &GoalDocument,
    parent_request_id: &str,
) -> Result<bool> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let agent_did = escape_graphql_string(&goal.agent_did);
    let parent_request_id = escape_graphql_string(parent_request_id);
    let expected_sequence = goal.continuation_sequence();
    let next_sequence = expected_sequence.saturating_add(1);
    let now = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_Goal(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }},
                    continuation_sequence: {{ _eq: {expected_sequence} }}
                }},
                input: {{
                    last_continued_from_request_id: "{parent_request_id}",
                    continuation_sequence: {next_sequence},
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!("claim goal continuation failed: {:?}", response.errors);
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_Goal"))
        .is_some_and(mutation_returned_rows))
}

pub async fn session_token_usage(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
) -> Result<i64> {
    let agent_did = escape_graphql_string(agent_did);
    let session_id = escape_graphql_string(session_id);
    let request_query = format!(
        r#"{{
            AgentRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, session_id: {{ _eq: "{session_id}" }} }}) {{ request_id }}
        }}"#
    );
    let response = node.execute(&request_query).await;
    if response.has_errors() {
        bail!("query goal session requests failed: {:?}", response.errors);
    }
    let mut request_ids = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("request_id").and_then(|value| value.as_str()))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    request_ids.sort();
    request_ids.dedup();
    if request_ids.is_empty() {
        return Ok(0);
    }
    let request_ids = request_ids
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ request_id: {{ _in: [{request_ids}] }}, call_state: {{ _eq: "completed" }} }}
            ) {{ prompt_tokens completion_tokens cached_input_tokens }}
        }}"#
    );
    #[derive(Deserialize)]
    struct UsageRow {
        #[serde(default)]
        prompt_tokens: Option<i64>,
        #[serde(default)]
        completion_tokens: Option<i64>,
        #[serde(default)]
        cached_input_tokens: Option<i64>,
    }
    let response = node.execute(&query).await;
    if response.has_errors() {
        bail!("query goal inference usage failed: {:?}", response.errors);
    }
    let rows: Vec<UsageRow> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("InferenceCall"))
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .context("decoding goal inference usage")?;
    Ok(rows.into_iter().fold(0_i64, |total, row| {
        let fresh_input = row
            .prompt_tokens
            .unwrap_or_default()
            .saturating_sub(row.cached_input_tokens.unwrap_or_default())
            .max(0);
        total.saturating_add(
            fresh_input.saturating_add(row.completion_tokens.unwrap_or_default().max(0)),
        )
    }))
}

pub async fn refresh_goal_usage(node: &EmbeddedNode, goal: &GoalDocument) -> Result<i64> {
    let tokens = session_token_usage(node, &goal.agent_did, &goal.session_id).await?;
    let now = Utc::now();
    let now_string = now.to_rfc3339();
    let active_time = goal.current_active_time_seconds(now);
    let updated_at = escape_graphql_string(&now_string);
    let active_started_field = optional_string_graphql_field(
        "active_started_at",
        goal.parsed_status()
            .is_some_and(GoalStatus::accrues_active_time)
            .then_some(now_string.as_str()),
    );
    update_goal_fields(
        node,
        goal,
        &format!(
            "tokens_used: {tokens}, active_time_seconds: {active_time}, {active_started_field} updated_at: \"{updated_at}\""
        ),
    )
    .await?;
    Ok(tokens)
}

async fn load_goal_by_doc_id(node: &EmbeddedNode, doc_id: &str) -> Result<Option<GoalDocument>> {
    let doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{ Goal(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ {GOAL_FIELDS} }} }}"#
    );
    Ok(decode_goal_rows(node, &query).await?.into_iter().next())
}

async fn execute_goal_mutation(node: &EmbeddedNode, mutation: &str, label: &str) -> Result<()> {
    let response = node.execute(mutation).await;
    if response.has_errors() {
        bail!("{label} failed: {:?}", response.errors);
    }
    Ok(())
}

fn mutation_returned_rows(value: &serde_json::Value) -> bool {
    value.as_array().is_some_and(|rows| !rows.is_empty()) || value.get("_docID").is_some()
}

fn optional_int_graphql_field(name: &str, value: Option<i64>) -> String {
    value
        .map(|value| format!("{name}: {value},"))
        .unwrap_or_else(|| format!("{name}: null,"))
}

fn optional_string_graphql_field(name: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!(r#"{name}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_else(|| format!("{name}: null,"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_status_vocabulary_is_stable() {
        for status in [
            GoalStatus::Active,
            GoalStatus::Paused,
            GoalStatus::Blocked,
            GoalStatus::UsageLimited,
            GoalStatus::BudgetLimited,
            GoalStatus::Complete,
        ] {
            assert_eq!(GoalStatus::parse(status.as_str()), Some(status));
        }
    }

    #[test]
    fn deterministic_id_separates_ambiguous_did_session_pairs() {
        assert_ne!(
            deterministic_goal_id("did:a", "bc"),
            deterministic_goal_id("did:ab", "c")
        );
    }

    #[test]
    fn blocked_audit_requires_three_distinct_requests_for_the_same_condition() {
        let (first, accepted) = next_blocked_audit(0, None, None, "needs approval", "req-1");
        assert_eq!((first, accepted), (1, false));

        let (duplicate, accepted) = next_blocked_audit(
            first,
            Some("needs approval"),
            Some("req-1"),
            "needs approval",
            "req-1",
        );
        assert_eq!((duplicate, accepted), (1, false));

        let (second, accepted) = next_blocked_audit(
            duplicate,
            Some("needs approval"),
            Some("req-1"),
            "needs approval",
            "req-2",
        );
        assert_eq!((second, accepted), (2, false));

        let (third, accepted) = next_blocked_audit(
            second,
            Some("needs approval"),
            Some("req-2"),
            "needs approval",
            "req-3",
        );
        assert_eq!((third, accepted), (3, true));

        let (changed, accepted) = next_blocked_audit(
            third,
            Some("needs approval"),
            Some("req-3"),
            "network unavailable",
            "req-4",
        );
        assert_eq!((changed, accepted), (1, false));
    }
}
