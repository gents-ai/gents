use anyhow::{bail, Context, Result};
use chrono::Utc;
use defra_agent::config_client::ConfigAccess;
use defra_agent::goal::{
    apply_operator_status_transition, delete_goals_for_session, deterministic_goal_id,
    load_canonical_goal, set_goal, GoalDocument, GoalSnapshot, GoalState, GoalStatus,
    BLOCKED_AUDIT_THRESHOLD, GOAL_FIELDS,
};
use defra_agent::graphql::escape_graphql_string;

use crate::cli::args::{GoalCommand, GoalScopeArgs, GoalSetArgs, GoalShowArgs, GoalStatusArg};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) async fn dispatch(command: GoalCommand) -> Result<()> {
    match command {
        GoalCommand::Show(args) => goal_show(args).await,
        GoalCommand::Set(args) => goal_set(args).await,
        GoalCommand::Clear(args) => goal_clear(args).await,
    }
}

async fn goal_show(args: GoalShowArgs) -> Result<()> {
    args.output
        .ensure_supported("goal show", &[OutputFormat::Json])?;
    let (access, agent_did) = access_and_did(&args.scope, false).await?;
    let goal = load_goal(&access, &agent_did, &args.scope.session)
        .await?
        .with_context(|| format!("no durable goal for session {}", args.scope.session))?;
    print_json(&serde_json::to_value(GoalSnapshot::from_document(
        &goal,
        Utc::now(),
    ))?)
}

async fn goal_set(args: GoalSetArgs) -> Result<()> {
    args.output
        .ensure_supported("goal set", &[OutputFormat::Json])?;
    let (access, agent_did) = access_and_did(&args.scope, true).await?;
    let status = args.status.map(GoalStatus::from);
    let budget = if args.clear_token_budget {
        Some(None)
    } else {
        args.token_budget.map(Some)
    };
    let goal = match &access {
        ConfigAccess::Local(node) => {
            set_goal(
                node,
                &agent_did,
                &args.scope.session,
                args.objective.as_deref(),
                status,
                budget,
            )
            .await?
        }
        ConfigAccess::Graphql(_) => {
            set_goal_over_graphql(
                &access,
                &agent_did,
                &args.scope.session,
                args.objective.as_deref(),
                status,
                budget,
            )
            .await?
        }
    };
    print_json(&serde_json::to_value(GoalSnapshot::from_document(
        &goal,
        Utc::now(),
    ))?)
}

async fn goal_clear(args: GoalShowArgs) -> Result<()> {
    args.output
        .ensure_supported("goal clear", &[OutputFormat::Json])?;
    let (access, agent_did) = access_and_did(&args.scope, true).await?;
    let goal = load_goal(&access, &agent_did, &args.scope.session)
        .await?
        .with_context(|| format!("no durable goal for session {}", args.scope.session))?;
    let deleted = match &access {
        ConfigAccess::Local(node) => {
            delete_goals_for_session(node, &agent_did, &args.scope.session).await? > 0
        }
        ConfigAccess::Graphql(_) => {
            let agent_did = escape_graphql_string(&agent_did);
            let session_id = escape_graphql_string(&args.scope.session);
            let response = access
                .execute(&format!(
                    r#"mutation {{
                        delete_Goal(filter: {{
                            agent_did: {{ _eq: "{agent_did}" }},
                            session_id: {{ _eq: "{session_id}" }}
                        }}) {{ _docID }}
                    }}"#
                ))
                .await?;
            response.pointer("/data/delete_Goal").is_some_and(|value| {
                value
                    .as_array()
                    .map_or_else(|| value.is_object(), |rows| !rows.is_empty())
            })
        }
    };
    print_json(&serde_json::json!({
        "goal_id": goal.goal_id,
        "session_id": goal.session_id,
        "deleted": deleted,
    }))
}

async fn access_and_did(scope: &GoalScopeArgs, write: bool) -> Result<(ConfigAccess, String)> {
    let agent_did = resolve_agent_did(scope.home.as_deref(), scope.agent_did.as_deref())
        .context("resolving goal owner agent_did")?;
    let (access, _) = resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref(), write)
        .await
        .context("resolving durable-goal access")?;
    Ok((access, agent_did))
}

async fn load_goal(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
) -> Result<Option<GoalDocument>> {
    if let ConfigAccess::Local(node) = access {
        return load_canonical_goal(node, agent_did, session_id).await;
    }
    let agent_did = escape_graphql_string(agent_did);
    let session_id = escape_graphql_string(session_id);
    let response = access
        .execute(&format!(
            r#"{{
                Goal(
                    filter: {{
                        agent_did: {{ _eq: "{agent_did}" }},
                        session_id: {{ _eq: "{session_id}" }}
                    }},
                    order: [{{ created_at: ASC }}, {{ goal_id: ASC }}]
                ) {{ {GOAL_FIELDS} }}
            }}"#
        ))
        .await?;
    let mut goals: Vec<GoalDocument> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .context("decoding durable Goal rows")?;
    goals.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.goal_id.cmp(&right.goal_id))
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
    Ok(goals.into_iter().next())
}

#[allow(clippy::too_many_arguments)]
async fn set_goal_over_graphql(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    objective: Option<&str>,
    status: Option<GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> Result<GoalDocument> {
    let existing = load_goal(access, agent_did, session_id).await?;
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
    let objective = escape_graphql_string(&objective);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_now = escape_graphql_string(&now_string);
    let budget_field = budget
        .map(|value| format!("token_budget: {value},"))
        .unwrap_or_else(|| "token_budget: null,".to_string());
    let started_field = if status.accrues_active_time() {
        format!(r#"active_started_at: "{escaped_now}","#)
    } else {
        "active_started_at: null,".to_string()
    };

    let mutation = if let Some(existing) = existing {
        let pre = existing
            .state()
            .context("existing Goal has an unknown status")?;
        let post = apply_operator_status_transition(pre, status)?;
        let resumed = pre.status != GoalStatus::Active && post.status == GoalStatus::Active;
        let doc_id = escape_graphql_string(&existing.doc_id);
        let active_time = existing.current_active_time_seconds(now);
        let reset_fields = if resumed {
            "last_blocked_request_id: null, last_blocked_reason: null, last_failure: null, infrastructure_retry_count: 0, completion_evidence: null,"
        } else {
            ""
        };
        format!(
            r#"mutation {{
                update_Goal(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }}
                    }},
                    input: {{
                        objective: "{objective}",
                        status: "{status}",
                        {budget_field}
                        tokens_used: {tokens_used},
                        active_time_seconds: {active_time},
                        {started_field}
                        consecutive_blocked_audits: {blocked_audits},
                        wrapup_requested: {wrapup_requested},
                        wrapup_completed: {wrapup_completed},
                        {reset_fields}
                        updated_at: "{escaped_now}"
                    }}
                ) {{ _docID }}
            }}"#,
            status = post.status.as_str(),
            tokens_used = existing.tokens_used.unwrap_or_default().max(0),
            blocked_audits = post.blocked_audits,
            wrapup_requested = post.wrapup_requested,
            wrapup_completed = post.wrapup_completed,
        )
    } else {
        let goal_id = escape_graphql_string(&deterministic_goal_id(agent_did, session_id));
        let initial_state = GoalState {
            status,
            blocked_audits: if status == GoalStatus::Blocked {
                BLOCKED_AUDIT_THRESHOLD
            } else {
                0
            },
            wrapup_requested: status == GoalStatus::BudgetLimited,
            wrapup_completed: status == GoalStatus::Complete,
        };
        format!(
            r#"mutation {{
                create_Goal(input: {{
                    goal_id: "{goal_id}",
                    session_id: "{escaped_session_id}",
                    agent_did: "{escaped_agent_did}",
                    objective: "{objective}",
                    status: "{status}",
                    {budget_field}
                    tokens_used: 0,
                    active_time_seconds: 0,
                    {started_field}
                    consecutive_blocked_audits: {blocked_audits},
                    continuation_sequence: 0,
                    wrapup_requested: {wrapup_requested},
                    wrapup_completed: {wrapup_completed},
                    infrastructure_retry_count: 0,
                    created_at: "{escaped_now}",
                    updated_at: "{escaped_now}"
                }}) {{ _docID }}
            }}"#,
            status = status.as_str(),
            blocked_audits = initial_state.blocked_audits,
            wrapup_requested = initial_state.wrapup_requested,
            wrapup_completed = initial_state.wrapup_completed,
        )
    };
    access.execute(&mutation).await?;
    load_goal(access, agent_did, session_id)
        .await?
        .context("durable Goal disappeared after write")
}

impl From<GoalStatusArg> for GoalStatus {
    fn from(value: GoalStatusArg) -> Self {
        match value {
            GoalStatusArg::Active => Self::Active,
            GoalStatusArg::Paused => Self::Paused,
            GoalStatusArg::Blocked => Self::Blocked,
            GoalStatusArg::UsageLimited => Self::UsageLimited,
            GoalStatusArg::BudgetLimited => Self::BudgetLimited,
            GoalStatusArg::Complete => Self::Complete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_cli_status_values_cover_runtime_vocabulary() {
        let values = [
            GoalStatusArg::Active,
            GoalStatusArg::Paused,
            GoalStatusArg::Blocked,
            GoalStatusArg::UsageLimited,
            GoalStatusArg::BudgetLimited,
            GoalStatusArg::Complete,
        ]
        .map(|value| GoalStatus::from(value).as_str());
        assert_eq!(
            values,
            [
                "active",
                "paused",
                "blocked",
                "usage_limited",
                "budget_limited",
                "complete"
            ]
        );
    }
}
