use anyhow::{Context, Result};
use chrono::Utc;
use gents::config_client::ConfigAccess;
use gents::goal::{
    delete_goals_for_session, load_canonical_goal, set_goal_from_access, GoalDocument,
    GoalSnapshot, GoalStatus, GOAL_FIELDS,
};
use gents::graphql::escape_graphql_string;

use crate::cli::args::{
    GoalCommand, GoalResumeArgs, GoalResumeOnArgs, GoalScopeArgs, GoalSetArgs, GoalShowArgs,
    GoalStatusArg,
};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_agent_did, resolve_config_access};

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
    let (access, agent_did) = access_and_did(&args.scope).await?;
    print_json(&goal_show_value(&access, &agent_did, &args.scope.session, Utc::now()).await?)
}

/// `goal show`'s JSON: the Goal's snapshot, the operator's
/// `auto_resume_at_reset` opt-in and, when its latest request stopped where
/// the operator can act, `blocked`.
async fn goal_show_value(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    now: chrono::DateTime<Utc>,
) -> Result<serde_json::Value> {
    let goal = load_goal(access, agent_did, session_id)
        .await?
        .with_context(|| format!("no durable goal for session {session_id}"))?;
    let mut value = serde_json::to_value(GoalSnapshot::from_document(&goal, now))?;
    value["auto_resume_at_reset"] = goal.auto_resume_at_reset.unwrap_or(false).into();
    let blocked = gents::blocked_turn::blocked_goal_turn(access, agent_did, session_id)
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
    let (access, agent_did) = access_and_did(&args.scope).await?;
    print_json(&goal_set_value(&access, &agent_did, &args).await?)
}

async fn goal_set_value(
    access: &ConfigAccess,
    agent_did: &str,
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
        agent_did,
        &args.scope.session,
        args.objective.as_deref(),
        status,
        budget,
        args.auto_resume,
    )
    .await?;
    Ok(serde_json::to_value(GoalSnapshot::from_document(
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

async fn goal_resume_on(args: GoalResumeOnArgs) -> Result<()> {
    args.output
        .ensure_supported("goal resume-on", &[OutputFormat::Json])?;
    let (access, agent_did) = access_and_did(&args.scope).await?;
    crate::request_helpers::ensure_local_request_signer(args.scope.home.as_deref(), &agent_did)?;
    let identity = gents::identity::RegisteredIdentity::from_registered_did(&agent_did, None)?;
    let backend_id = crate::commands::config::profile::backend_for_account(
        &access,
        &agent_did,
        &args.account,
        args.provider.as_deref(),
    )
    .await?;
    let (home, graphql) = (args.scope.home.as_deref(), args.scope.graphql.as_deref());
    let receipt = gents::goal::resume_goal_on_account(
        &access,
        &identity,
        &agent_did,
        &args.scope.session,
        &args.from,
        &backend_id,
        args.with_compaction,
        &|profile| {
            crate::commands::config::profile::bound_slots(home, graphql, &agent_did, profile)
        },
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
    let deleted = match &*access {
        ConfigAccess::Local(node) => {
            delete_goals_for_session(node, &agent_did, &args.scope.session).await? > 0
        }
        ConfigAccess::Graphql(_) => {
            let agent_did = escape_graphql_string(&agent_did);
            let session_id = escape_graphql_string(&args.scope.session);
            let agent_did_ref = &agent_did;
            let session_id_ref = &session_id;
            access
                .transact("cli.goal.clear", move |txn| {
                    Box::pin(async move {
                        let response = txn
                            .execute(&format!(
                                r#"mutation {{
                        delete_Goal(filter: {{
                            agent_did: {{ _eq: "{agent_did_ref}" }},
                            session_id: {{ _eq: "{session_id_ref}" }}
                        }}) {{ _docID }}
                    }}"#
                            ))
                            .await?;
                        txn.execute(&format!(
                            r#"mutation {{
                        delete_GoalCreationClaim(filter: {{
                            agent_did: {{ _eq: "{agent_did_ref}" }},
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
        }
    };
    print_json(&serde_json::json!({
        "goal_id": goal.goal_id,
        "session_id": goal.session_id,
        "deleted": deleted,
    }))
}

async fn access_and_did(scope: &GoalScopeArgs) -> Result<(crate::CommandAccess, String)> {
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

    const DID: &str = "did:key:z6MkTestGoalShowBlocked";

    async fn seeded() -> ConfigAccess {
        let node = std::sync::Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        gents::ensure_agent_principal(node.as_ref(), DID)
            .await
            .unwrap();
        let access = ConfigAccess::Local(node);
        let backend = serde_json::from_value(serde_json::json!({
            "agent_did": DID, "backend_id": "claude", "name": "Claude",
            "provider_kind": "ClaudeCliSubscription", "endpoint": "claude-cli://subscription",
            "auth": {"kind": "principal_oauth"},
        }))
        .unwrap();
        gents::config_client::write_inference_backend_document(&access, &backend)
            .await
            .unwrap();
        let credential = gents::claude_oauth::credential_from_login_tokens(
            DID,
            "claude-subscription",
            &gents::claude_oauth::ClaudeLoginTokens {
                access_token: "access-SECRET".into(),
                refresh_token: "refresh-SECRET".into(),
                expires_in: Some(3600),
                scope: None,
                account_id: Some("IDENTITY".into()),
                organization_uuid: None,
                account_uuid: None,
            },
            Utc::now(),
        );
        gents::oauth_credential::store_sign_in(&access, credential, None)
            .await
            .unwrap();
        for (session, status) in [
            ("session-limited", GoalStatus::UsageLimited),
            ("session-active", GoalStatus::Active),
        ] {
            set_goal_from_access(
                &access,
                DID,
                session,
                Some("ship it"),
                Some(status),
                None,
                None,
            )
            .await
            .unwrap();
        }
        let at = (Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();
        let failure = "provider usage limit reached (resets at 2030-01-01T00:00:00Z): The usage limit has been reached";
        access
            .write(
                "test.goal_show.rows",
                &format!(
                    r#"mutation {{
                        create_AgentRequest(input: {{
                            request_id: "request-limited" purpose: "normal" agent_did: "{DID}"
                            behavior_id: "general" session_id: "session-limited" content: "run"
                            lifecycle_state: "failed" failure_reason: "{failure}" created_at: "{at}"
                        }}) {{ _docID }}
                        create_InferenceCall(input: {{
                            call_id: "call-limited" request_id: "request-limited" call_seq: 1
                            backend_id: "claude" behavior_id: "general" agent_did: "{DID}"
                            call_kind: "inference" attempt: 1 call_state: "failed"
                            failure_reason: "{failure}"
                            queued_at: "{at}" started_at: "{at}" ended_at: "{at}"
                        }}) {{ _docID }}
                    }}"#
                ),
            )
            .await
            .unwrap();
        access
    }

    #[tokio::test]
    async fn blocked_goal_show_carries_the_stopped_turn() {
        let access = seeded().await;
        let limited = goal_show_value(&access, DID, "session-limited", Utc::now())
            .await
            .unwrap();
        assert_eq!(limited["status"], "usage_limited");
        assert_eq!(
            limited.pointer("/blocked/reason"),
            Some(&serde_json::json!("usage_limit"))
        );
        assert_eq!(
            limited.pointer("/blocked/resets_at"),
            Some(&serde_json::json!("2030-01-01T00:00:00Z"))
        );
        assert_eq!(
            limited.pointer("/blocked/account/label"),
            Some(&serde_json::json!("Claude"))
        );
        let active = goal_show_value(&access, DID, "session-active", Utc::now())
            .await
            .unwrap();
        assert_eq!(active.get("blocked"), Some(&serde_json::Value::Null));
        let text = format!("{limited} {active}");
        for secret in ["SECRET", "IDENTITY"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }

    #[tokio::test]
    async fn goal_show_reports_the_operator_auto_resume_opt_in() {
        use crate::cli::args::{Cli, Command};
        use clap::Parser;
        let access = seeded().await;
        let shown = goal_show_value(&access, DID, "session-active", Utc::now())
            .await
            .unwrap();
        assert_eq!(shown["auto_resume_at_reset"], false);
        let Command::Goal {
            command: GoalCommand::Set(args),
        } = Cli::try_parse_from([
            "gents",
            "goal",
            "set",
            "--session",
            "session-active",
            "--auto-resume",
            "on",
        ])
        .unwrap()
        .command
        else {
            panic!("expected goal set");
        };
        // `goal set` prints the model-facing snapshot, which never carries the opt-in.
        let set = goal_set_value(&access, DID, &args).await.unwrap();
        assert_eq!(set.get("auto_resume_at_reset"), None, "{set}");
        let shown = goal_show_value(&access, DID, "session-active", Utc::now())
            .await
            .unwrap();
        assert_eq!(shown["auto_resume_at_reset"], true);
    }

    #[tokio::test]
    async fn resume_on_maps_the_account_to_its_backend() {
        let node = std::sync::Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        gents::ensure_agent_principal(node.as_ref(), DID)
            .await
            .unwrap();
        let access = ConfigAccess::Local(node);
        let backend = serde_json::from_value(serde_json::json!({
            "agent_did": DID, "backend_id": "claude", "name": "Claude",
            "provider_kind": "ClaudeCliSubscription", "endpoint": "claude-cli://subscription",
            "auth": {"kind": "principal_oauth"},
        }))
        .unwrap();
        gents::config_client::write_inference_backend_document(&access, &backend)
            .await
            .unwrap();
        let mut signed = Vec::new();
        for (who, label) in [("a", None), ("b", Some("label-b"))] {
            let credential = gents::claude_oauth::credential_from_login_tokens(
                DID,
                "claude-subscription",
                &gents::claude_oauth::ClaudeLoginTokens {
                    access_token: format!("access-SECRET-{who}"),
                    refresh_token: format!("refresh-SECRET-{who}"),
                    expires_in: Some(3600),
                    scope: None,
                    account_id: Some("IDENTITY".into()),
                    organization_uuid: Some("org-1".into()),
                    account_uuid: Some(format!("account-{who}")),
                },
                Utc::now(),
            );
            signed.push(
                gents::oauth_credential::store_sign_in(&access, credential, label)
                    .await
                    .unwrap()
                    .credential,
            );
        }
        let backend = crate::commands::config::profile::backend_for_account(
            &access,
            DID,
            "label-b",
            Some("claude-subscription"),
        )
        .await
        .unwrap();
        assert_eq!(
            backend,
            format!(
                "claude-subscription-{}",
                signed[1].account_ref.as_deref().unwrap()
            )
        );
        assert!(crate::commands::config::profile::backend_for_account(
            &access, DID, "label-z", None
        )
        .await
        .is_err());
    }

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
