use anyhow::{Context, Result};
use chrono::Utc;
use gents::config_client::ConfigAccess;
use gents::goal::{
    delete_goals_for_session, load_canonical_goal, set_goal_from_access, GoalDocument,
    GoalSnapshot, GoalStatus, GOAL_FIELDS,
};
use gents::graphql::escape_graphql_string;

use crate::cli::args::{
    GoalCommand, GoalResumeArgs, GoalScopeArgs, GoalSetArgs, GoalShowArgs, GoalStatusArg,
};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) async fn dispatch(command: GoalCommand) -> Result<()> {
    match command {
        GoalCommand::Show(args) => goal_show(args).await,
        GoalCommand::Set(args) => goal_set(args).await,
        GoalCommand::ResumeRequest(args) => goal_resume(args).await,
        GoalCommand::Clear(args) => goal_clear(args).await,
    }
}

async fn goal_show(args: GoalShowArgs) -> Result<()> {
    args.output
        .ensure_supported("goal show", &[OutputFormat::Json])?;
    let (access, agent_did) = access_and_did(&args.scope).await?;
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
    let (access, agent_did) = access_and_did(&args.scope).await?;
    let status = args.status.map(GoalStatus::from);
    let budget = if args.clear_token_budget {
        Some(None)
    } else {
        args.token_budget.map(Some)
    };
    let goal = set_goal_from_access(
        &access,
        &agent_did,
        &args.scope.session,
        args.objective.as_deref(),
        status,
        budget,
    )
    .await?;
    print_json(&serde_json::to_value(GoalSnapshot::from_document(
        &goal,
        Utc::now(),
    ))?)
}

async fn goal_resume(args: GoalResumeArgs) -> Result<()> {
    args.output
        .ensure_supported("goal resume-request", &[OutputFormat::Json])?;
    let (access, agent_did) = access_and_did(&args.scope).await?;
    crate::request_helpers::ensure_local_request_signer(args.scope.home.as_deref(), &agent_did)?;
    let identity = gents::identity::RegisteredIdentity::from_registered_did(&agent_did, None)?;
    let receipt = gents::goal::resume_goal_request(
        &access,
        &identity,
        &agent_did,
        &args.scope.session,
        &args.from,
    )
    .await?;
    print_json(&serde_json::to_value(receipt)?)
}

async fn goal_clear(args: GoalShowArgs) -> Result<()> {
    args.output
        .ensure_supported("goal clear", &[OutputFormat::Json])?;
    let (access, agent_did) = access_and_did(&args.scope).await?;
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
            let txn = access.begin_apply_txn().await?;
            let result = async {
                let response = txn
                    .execute(&format!(
                        r#"mutation {{
                        delete_Goal(filter: {{
                            agent_did: {{ _eq: "{agent_did}" }},
                            session_id: {{ _eq: "{session_id}" }}
                        }}) {{ _docID }}
                    }}"#
                    ))
                    .await?;
                txn.execute(&format!(
                    r#"mutation {{
                        delete_GoalCreationClaim(filter: {{
                            agent_did: {{ _eq: "{agent_did}" }},
                            session_id: {{ _eq: "{session_id}" }}
                        }}) {{ _docID }}
                    }}"#
                ))
                .await?;
                Ok::<_, anyhow::Error>(response.pointer("/data/delete_Goal").is_some_and(|value| {
                    value
                        .as_array()
                        .map_or_else(|| value.is_object(), |rows| !rows.is_empty())
                }))
            }
            .await;
            match result {
                Ok(deleted) => {
                    txn.commit().await?;
                    deleted
                }
                Err(error) => {
                    let _ = txn.discard().await;
                    return Err(error);
                }
            }
        }
    };
    print_json(&serde_json::json!({
        "goal_id": goal.goal_id,
        "session_id": goal.session_id,
        "deleted": deleted,
    }))
}

async fn access_and_did(scope: &GoalScopeArgs) -> Result<(ConfigAccess, String)> {
    let agent_did = resolve_agent_did(scope.home.as_deref(), scope.agent_did.as_deref())
        .context("resolving goal owner agent_did")?;
    let (access, _) = resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref())
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
