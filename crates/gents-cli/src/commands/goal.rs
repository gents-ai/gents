use anyhow::{Context, Result};
use chrono::Utc;
use gents::goal::{
    delete_goals_for_session, load_canonical_goal, set_goal_from_access, GoalDocument, GoalStatus,
    GOAL_FIELDS,
};
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;

use crate::cli::args::{
    GoalCommand, GoalResumeArgs, GoalResumeOnArgs, GoalScopeArgs, GoalSetArgs, GoalShowArgs,
    GoalStatusArg,
};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_config_access, resolve_node_did};

pub(crate) async fn dispatch(command: GoalCommand) -> Result<()> {
    match command {
        GoalCommand::Show(args) => goal_show(args).await,
        GoalCommand::Set(args) => goal_set(args).await,
        GoalCommand::ResumeRequest(args) => goal_resume(args).await,
        GoalCommand::ResumeOn(args) => goal_resume_on(args).await,
        GoalCommand::Clear(args) => goal_clear(args).await,
    }
}

async fn goal_show(args: GoalShowArgs) -> Result<()> {
    args.output
        .ensure_supported("goal show", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    print_json(&goal_show_value(&access, &node_did, &args.scope.session, Utc::now()).await?)
}

async fn goal_show_value(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    now: chrono::DateTime<Utc>,
) -> Result<serde_json::Value> {
    let goal = load_goal(access, node_did, session_id)
        .await?
        .with_context(|| format!("no durable goal for session {session_id}"))?;
    let mut value = serde_json::to_value(gents::goal::snapshot(&goal, now))?;
    value["auto_resume_at_reset"] = goal.auto_resume_at_reset.unwrap_or(false).into();
    let blocked = gents::goal::blocked_turn::blocked_goal_turn(access, node_did, session_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(session_id, error = %format!("{error:#}"), "blocked turn unreadable");
            None
        });
    value["blocked"] = serde_json::to_value(blocked)?;
    Ok(value)
}

async fn goal_set(args: GoalSetArgs) -> Result<()> {
    args.output
        .ensure_supported("goal set", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    print_json(&goal_set_value(&access, &node_did, &args).await?)
}

async fn goal_set_value(
    access: &ConfigAccess,
    node_did: &str,
    args: &GoalSetArgs,
) -> Result<serde_json::Value> {
    let status = args.status.map(GoalStatus::from);
    let budget = if args.clear_token_budget {
        Some(None)
    } else {
        args.token_budget.map(Some)
    };
    let goal = set_goal_from_access(
        access,
        node_did,
        &args.scope.session,
        args.objective.as_deref(),
        status,
        budget,
        args.auto_resume,
    )
    .await?;
    Ok(serde_json::to_value(gents::goal::snapshot(
        &goal,
        Utc::now(),
    ))?)
}

async fn goal_resume(args: GoalResumeArgs) -> Result<()> {
    args.output
        .ensure_supported("goal resume-request", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    crate::request_helpers::ensure_local_request_signer(args.scope.home.as_deref(), &node_did)?;
    let identity =
        gents::store::identity::RegisteredIdentity::from_registered_did(&node_did, None)?;
    let receipt = gents::goal::resume_goal_request(
        &access,
        &identity,
        &node_did,
        &args.scope.session,
        &args.from,
    )
    .await?;
    print_json(&serde_json::to_value(receipt)?)
}

async fn goal_resume_on(args: GoalResumeOnArgs) -> Result<()> {
    args.output
        .ensure_supported("goal resume-on", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    crate::request_helpers::ensure_local_request_signer(args.scope.home.as_deref(), &node_did)?;
    let identity =
        gents::store::identity::RegisteredIdentity::from_registered_did(&node_did, None)?;
    let backend_id = crate::commands::config::profile::backend_for_account(
        &access,
        &node_did,
        &args.account,
        args.provider.as_deref(),
    )
    .await?;
    let (home, graphql) = (args.scope.home.as_deref(), args.scope.graphql.as_deref());
    let receipt = gents::goal::resume_goal_on_account(
        &access,
        &identity,
        &node_did,
        &args.scope.session,
        &args.from,
        &backend_id,
        args.with_compaction,
        &|profile| crate::commands::config::profile::bound_slots(home, graphql, &node_did, profile),
    )
    .await?;
    print_json(&serde_json::to_value(receipt)?)
}

async fn goal_clear(args: GoalShowArgs) -> Result<()> {
    args.output
        .ensure_supported("goal clear", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    let goal = load_goal(&access, &node_did, &args.scope.session)
        .await?
        .with_context(|| format!("no durable goal for session {}", args.scope.session))?;
    let deleted = if let Some(node) = access.local_node() {
        delete_goals_for_session(node, &node_did, &args.scope.session).await? > 0
    } else {
        let node_did = escape_graphql_string(&node_did);
        let session_id = escape_graphql_string(&args.scope.session);
        let node_did_ref = &node_did;
        let session_id_ref = &session_id;
        access
            .transact_write("cli.goal.clear", move |txn| {
                Box::pin(async move {
                    let response = txn
                        .execute(&format!(
                            r#"mutation {{
                        delete_Goal(filter: {{
                            node_did: {{ _eq: "{node_did_ref}" }},
                            session_id: {{ _eq: "{session_id_ref}" }}
                        }}) {{ _docID }}
                    }}"#
                        ))
                        .await?;
                    txn.execute(&format!(
                        r#"mutation {{
                        delete_GoalCreationClaim(filter: {{
                            node_did: {{ _eq: "{node_did_ref}" }},
                            session_id: {{ _eq: "{session_id_ref}" }}
                        }}) {{ _docID }}
                    }}"#
                    ))
                    .await?;
                    Ok::<_, anyhow::Error>(response.pointer("/data/delete_Goal").is_some_and(
                        |value| {
                            value
                                .as_array()
                                .map_or_else(|| value.is_object(), |rows| !rows.is_empty())
                        },
                    ))
                })
            })
            .await?
    };
    print_json(&serde_json::json!({
        "goal_id": goal.goal_id,
        "session_id": goal.session_id,
        "deleted": deleted,
    }))
}

async fn access_and_did(
    scope: &GoalScopeArgs,
) -> Result<(gents::store::config_client::ConfigAccess, String)> {
    let node_did = resolve_node_did(scope.home.as_deref(), scope.node_did.as_deref())
        .context("resolving goal owner node_did")?;
    let (access, _) = resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref())
        .await
        .context("resolving durable-goal access")?;
    Ok((access, node_did))
}

async fn load_goal(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
) -> Result<Option<GoalDocument>> {
    if let Some(node) = access.local_node() {
        return load_canonical_goal(node, node_did, session_id).await;
    }
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    let response = access
        .execute(&format!(
            r#"{{
                Goal(
                    filter: {{
                        node_did: {{ _eq: "{node_did}" }},
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
