use anyhow::{Context, Result};
use chrono::Utc;
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::operations::goal::{
    GoalExpected, GoalOperationError, GoalSetInput, GoalStartInput,
};
use gents_store::config_client::ConfigAccess;

use crate::cli::args::{
    GoalCommand, GoalResumeArgs, GoalResumeOnArgs, GoalScopeArgs, GoalSetArgs, GoalShowArgs,
    GoalStartArgs, GoalStatusArg,
};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_config_access};
use gents_service::home_state::resolve_node_did;

pub(crate) async fn dispatch(command: GoalCommand) -> Result<()> {
    match command {
        GoalCommand::Show(args) => goal_show(args).await,
        GoalCommand::Start(args) => goal_start(args).await,
        GoalCommand::Set(args) => goal_set(args).await,
        GoalCommand::ResumeRequest(args) => goal_resume(args).await,
        GoalCommand::ResumeOn(args) => goal_resume_on(args).await,
        GoalCommand::Clear(args) => goal_clear(args).await,
    }
}

async fn goal_show(args: GoalShowArgs) -> Result<()> {
    args.output
        .ensure_supported("automation goal show", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    print_json(&goal_show_value(&access, &node_did, &args.scope.session, Utc::now()).await?)
}

async fn goal_show_value(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    now: chrono::DateTime<Utc>,
) -> Result<serde_json::Value> {
    let goal = gents_automation::views::canonical_goal(access, node_did, session_id)
        .await?
        .with_context(|| format!("no durable goal for session {session_id}"))?;
    let mut value = serde_json::to_value(gents_automation::goal::snapshot(&goal, now))?;
    value["auto_resume_at_reset"] = goal.auto_resume_at_reset.unwrap_or(false).into();
    let blocked = gents_automation::goal::blocked_turn::blocked_goal_turn(
        access, node_did, session_id,
    )
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
        .ensure_supported("automation goal set", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    print_json(&goal_set_value(&access, &node_did, &args).await?)
}

async fn goal_set_value(
    access: &ConfigAccess,
    node_did: &str,
    args: &GoalSetArgs,
) -> Result<serde_json::Value> {
    let session_id = args.scope.session.as_str();
    let observed =
        gents_automation::operations::goal::observe_revision(access, node_did, session_id)
            .await?
            .ok_or_else(|| failure(GoalOperationError::NoGoal(session_id.to_owned()), session_id))?;
    let input = GoalSetInput {
        objective: args.objective.clone(),
        token_budget: if args.clear_token_budget {
            Some(None)
        } else {
            args.token_budget.map(Some)
        },
        status: args.status.map(GoalStatus::from),
        auto_resume: args.auto_resume,
        expected: Some(GoalExpected::Revision(observed)),
        ..Default::default()
    };
    let goal =
        gents_automation::operations::goal::set(access, node_did, &args.scope.session, &input)
            .await
            .map_err(|error| failure(error, &args.scope.session))?;
    Ok(serde_json::to_value(gents_automation::goal::snapshot(
        &goal,
        Utc::now(),
    ))?)
}

async fn goal_start(args: GoalStartArgs) -> Result<()> {
    args.output
        .ensure_supported("automation goal start", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    let session_id = args.scope.session.as_str();
    let agent = access
        .transact_readonly("goal.start.session", |txn| {
            let node_did = node_did.as_str();
            Box::pin(async move {
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn, node_did, session_id,
                )
                .await
            })
        })
        .await?
        .map(|session| session.session.agent_did)
        .ok_or_else(|| {
            failure(
                GoalOperationError::SessionMissing(session_id.to_owned()),
                session_id,
            )
        })?;
    let request = crate::request_helpers::goal_request(
        &access,
        &node_did,
        session_id,
        &agent,
        &args.objective,
    )
    .await?;
    let input = GoalStartInput {
        session_id: session_id.to_owned(),
        objective: args.objective.clone(),
        token_budget: args.token_budget,
    };
    let disposition =
        gents_automation::operations::goal::start(&access, &node_did, &input, &request)
            .await
            .map_err(|error| failure(error, session_id))?;
    let goal = gents_automation::views::canonical_goal(&access, &node_did, session_id)
        .await?
        .with_context(|| format!("no durable goal for session {session_id}"))?;
    let mut value = serde_json::to_value(gents_automation::goal::snapshot(&goal, Utc::now()))?;
    value["request_id"] = request.request_id.clone().into();
    value["disposition"] = match disposition {
        gents_automation::goal::GoalBackedRequestDisposition::Created => "created",
        gents_automation::goal::GoalBackedRequestDisposition::Idempotent => "idempotent",
    }
    .into();
    print_json(&value)
}

/// A refused Goal operation, shown with the command that corrects it.
fn failure(error: GoalOperationError, session_id: &str) -> anyhow::Error {
    let next = crate::next_call::command(error.recovery(), Some(session_id));
    let message = format!("{error}\nNext: {next}");
    anyhow::Error::new(error).context(message)
}

async fn goal_resume(args: GoalResumeArgs) -> Result<()> {
    args.output
        .ensure_supported("automation goal resume-request", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    let actor = access
        .actor_did()
        .context("Goal resume requires an authenticated client identity")?;
    let identity = gents_store::identity::RegisteredIdentity::from_registered_did(actor, None)?;
    let receipt = gents_automation::goal::resume_goal_request(
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
        .ensure_supported("automation goal resume-on", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    let actor = access
        .actor_did()
        .context("Goal resume requires an authenticated client identity")?;
    let identity = gents_store::identity::RegisteredIdentity::from_registered_did(actor, None)?;
    let backend_id = crate::commands::config::profile::backend_for_account(
        &access,
        &node_did,
        &args.account,
        args.provider.as_deref(),
    )
    .await?;
    let (home, graphql) = (args.scope.home.as_deref(), args.scope.graphql.as_deref());
    let receipt = gents_automation::goal::resume_goal_on_account(
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
        .ensure_supported("automation goal clear", &[OutputFormat::Json])?;
    let (access, node_did) = access_and_did(&args.scope).await?;
    let goal = gents_automation::views::canonical_goal(&access, &node_did, &args.scope.session)
        .await?
        .with_context(|| format!("no durable goal for session {}", args.scope.session))?;
    let deleted = gents_automation::goal::delete_goals_for_session_from_access(
        &access,
        &node_did,
        &args.scope.session,
    )
    .await?
        > 0;
    print_json(&serde_json::json!({
        "goal_id": goal.goal_id,
        "session_id": goal.session_id,
        "deleted": deleted,
    }))
}

async fn access_and_did(
    scope: &GoalScopeArgs,
) -> Result<(gents_store::config_client::ConfigAccess, String)> {
    let node_did = resolve_node_did(scope.home.as_deref(), scope.node_did.as_deref())
        .context("resolving goal owner node_did")?;
    let (access, _) = resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref())
        .await
        .context("resolving durable-goal access")?;
    Ok((access, node_did))
}

impl From<GoalStatusArg> for GoalStatus {
    fn from(value: GoalStatusArg) -> Self {
        match value {
            GoalStatusArg::Active => Self::Active,
            GoalStatusArg::Paused => Self::Paused,
            GoalStatusArg::Blocked => Self::Blocked,
            GoalStatusArg::Complete => Self::Complete,
        }
    }
}
