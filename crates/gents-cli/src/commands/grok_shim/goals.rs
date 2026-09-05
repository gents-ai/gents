//! Native goal-panel hydration from the runtime's canonical goal owner.
//! Only the last successfully delivered observation is connection-local.
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents::goal::{GoalDocument, GoalStatus};
use serde_json::{json, Value};

use super::projection::{ProjectionEngine, UpdateTimestamps};
use super::turn::{PromptSender, PromptSenderLine};

#[derive(Debug, PartialEq)]
pub(super) enum GoalCommand {
    Status,
    Pause,
    Resume,
    Clear,
}

impl GoalCommand {
    pub(super) fn parse(text: &str) -> Result<Option<Self>> {
        let text = text.trim();
        let mut words = text.split_whitespace();
        if words.next() != Some("/goal") {
            return Ok(None);
        }
        let command = match words.next().unwrap_or("status").to_ascii_lowercase().as_str() {
            "status" => Self::Status,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "clear" => Self::Clear,
            _ => anyhow::bail!("This server supports /goal status, pause, resume, and clear. Create goals with the configured runtime create_goal tool."),
        };
        anyhow::ensure!(
            words.next().is_none(),
            "goal control does not accept extra arguments"
        );
        Ok(Some(command))
    }

    /// Operator controls reuse the runtime transition/clear owners. The ACP
    /// caller must authorize the exact attached session before invoking this.
    pub(super) async fn execute(
        &self,
        node: &EmbeddedNode,
        principal: &str,
        session: &str,
    ) -> Result<String> {
        if *self == Self::Clear {
            let count = gents::goal::delete_goals_for_session(node, principal, session).await?;
            return Ok(if count == 0 {
                "No goal is set."
            } else {
                "Goal cleared."
            }
            .into());
        }
        let Some(mut goal) = gents::goal::load_canonical_goal(node, principal, session).await?
        else {
            return Ok("No goal is set.".into());
        };
        let status = match self {
            Self::Pause => Some(GoalStatus::Paused),
            Self::Resume => Some(GoalStatus::Active),
            _ => None,
        };
        if let Some(status) = status {
            let state = goal.state().context("unrecognized persisted goal state")?;
            // A legitimate but unavailable control is a host-command reply,
            // not a failed model turn inviting the user to retry endlessly.
            // The runtime's transition function remains the legality owner.
            if gents::goal::apply_operator_status_transition(state, status).is_err() {
                return Ok(format!(
                    "Goal remains {}. This control is not available in that state.",
                    goal.status
                ));
            }
            goal =
                gents::goal::set_goal(node, principal, session, None, Some(status), None).await?;
        }
        Ok(format!(
            "Goal: {}\nStatus: {}\nTokens used: {}{}",
            goal.objective,
            goal.status,
            goal.tokens_used.unwrap_or_default().max(0),
            goal.token_budget
                .map(|budget| format!(" / {budget}"))
                .unwrap_or_default()
        ))
    }
}

#[derive(Default)]
pub(super) struct GoalCursor {
    delivered: Option<Value>,
}

impl GoalCursor {
    /// The session observer has already authorized the attached root session.
    /// No refresh_goal_usage/set_goal calls: observation must not mutate goals.
    pub(super) async fn refresh(
        &mut self,
        node: &EmbeddedNode,
        principal: &str,
        session: &str,
        sender: &PromptSender,
        projections: &ProjectionEngine,
    ) -> Result<()> {
        let goal = gents::goal::load_canonical_goal(node, principal, session).await?;
        let observed = goal.as_ref().map(serde_json::to_value).transpose()?;
        if observed == self.delivered {
            return Ok(());
        }
        let update = match goal.as_ref() {
            Some(goal) => project(goal, Utc::now())?,
            None => {
                let id = self
                    .delivered
                    .as_ref()
                    .and_then(|row| row["goal_id"].as_str())
                    .context("missing previously delivered goal identity")?;
                base_update(id, "", "cleared", 0)
            }
        };
        projections
            .session_updates()
            .send(
                session,
                |event_id, total_tokens| {
                    Ok(super::projection::session_notification_for_method(
                        "x.ai/session_notification",
                        session,
                        update,
                        super::projection::stamp_update_meta(
                            event_id,
                            total_tokens,
                            None,
                            None,
                            UpdateTimestamps::default(),
                        ),
                    ))
                },
                PromptSenderLine(sender),
            )
            .await?;
        self.delivered = observed;
        Ok(())
    }
}

fn base_update(id: &str, objective: &str, status: &str, elapsed_ms: u64) -> Value {
    // Grok's optional worker/verifier orchestration is not used by Gents'
    // durable goal owner. Do not reinterpret continuation attempts as rounds.
    json!({"sessionUpdate":"goal_updated", "goal_id":id, "objective":objective,
        "status":status, "phase":"idle", "elapsed_ms":elapsed_ms,
        "total_deliverables":0, "completed_deliverables":0,
        "total_worker_rounds":0, "total_verify_rounds":0})
}

fn project(goal: &GoalDocument, now: DateTime<Utc>) -> Result<Value> {
    let status = match goal
        .parsed_status()
        .context("unrecognized persisted goal status")?
    {
        GoalStatus::Active => "active",
        GoalStatus::Paused => "user_paused",
        GoalStatus::Blocked => "blocked",
        // Stock explicitly renders unfamiliar status strings as paused.
        // Preserve the real cause rather than claiming an infrastructure
        // failure or a token-budget limit; pause_message explains it.
        GoalStatus::UsageLimited => "usage_limited",
        GoalStatus::BudgetLimited => "budget_limited",
        GoalStatus::Complete => "complete",
    };
    let elapsed_ms = (goal.current_active_time_seconds(now) as u64).saturating_mul(1000);
    let mut update = base_update(&goal.goal_id, &goal.objective, status, elapsed_ms);
    update["tokens_used"] = json!(goal.tokens_used.unwrap_or_default().max(0));
    if let Some(budget) = goal.token_budget {
        update["token_budget"] = json!(budget);
    }
    let reason = if goal.parsed_status() == Some(GoalStatus::UsageLimited) {
        Some("Gents usage limit reached; waiting for runtime admission".to_owned())
    } else {
        goal.last_blocked_reason
            .clone()
            .or_else(|| goal.last_failure.clone())
    };
    if let Some(reason) = reason {
        update["pause_message"] = json!(reason);
    }
    update["_meta"] = json!({"gents/goalStatus":goal.status,
        "gents/continuationSequence":goal.continuation_sequence(),
        "gents/goalOrchestration":"runtime-owned; no stock worker/verifier phases"});
    Ok(update)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_controls_are_exact_commands_not_prompt_substrings() {
        for (text, expected) in [
            ("/goal", GoalCommand::Status),
            (" /goal STATUS ", GoalCommand::Status),
            ("/goal pause", GoalCommand::Pause),
            ("/goal resume", GoalCommand::Resume),
            ("/goal clear", GoalCommand::Clear),
        ] {
            assert_eq!(GoalCommand::parse(text).unwrap(), Some(expected));
        }
        for text in ["Explain /goal pause", "/goals pause", "hello"] {
            assert_eq!(GoalCommand::parse(text).unwrap(), None);
        }
        assert!(GoalCommand::parse("/goal pause extra").is_err());
        assert!(GoalCommand::parse("/goal build something").is_err());
    }

    #[tokio::test]
    async fn goal_observation_is_scoped_read_only_and_retries_failed_delivery() {
        use super::super::projection::BoundModelContext;
        use gents::graphql::ensure_no_errors;
        use std::sync::Arc;
        let directory = tempfile::tempdir().unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(directory.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .unwrap(),
        );
        gents::schema::ensure_runtime_schemas(&node).await.unwrap();
        let projections = ProjectionEngine::new(
            node.clone(),
            BoundModelContext::new("model".into(), "Model".into(), 1000),
        );
        let buffer = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        let mut cursor = GoalCursor::default();
        for (id, agent, session) in [
            ("owned", "principal", "session"),
            ("foreign-agent", "other", "session"),
            ("foreign-session", "principal", "other"),
        ] {
            let result = node.execute(&format!(r#"mutation {{create_Goal(input: {{
                goal_id:"{id}", agent_did:"{agent}", session_id:"{session}", objective:"Objective {id}",
                status:"active", tokens_used:12, active_time_seconds:3, created_at:"2026-09-01T00:00:00Z"
            }}) {{_docID}}}}"#)).await;
            ensure_no_errors(&result, "seed goal").unwrap();
        }
        let before = node
            .execute("{Goal {goal_id status tokens_used}}")
            .await
            .data;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        let failed = PromptSender::Live {
            outbound: super::super::server::AcpOutbound::for_frames(tx),
        };
        assert!(cursor
            .refresh(&node, "principal", "session", &failed, &projections)
            .await
            .is_err());
        assert!(cursor.delivered.is_none());
        cursor
            .refresh(&node, "principal", "session", &sender, &projections)
            .await
            .unwrap();
        cursor
            .refresh(&node, "principal", "session", &sender, &projections)
            .await
            .unwrap();
        assert_eq!(buffer.lock().await.len(), 1);
        let delivered: Value = serde_json::from_str(&buffer.lock().await[0]).unwrap();
        assert_eq!(delivered["params"]["update"]["goal_id"], "owned");
        assert_eq!(
            before,
            node.execute("{Goal {goal_id status tokens_used}}")
                .await
                .data
        );
        let limited = node.execute(r#"mutation {update_Goal(filter:{goal_id:{_eq:"owned"}}, input:{status:"budget_limited"}) {_docID}}"#).await;
        ensure_no_errors(&limited, "budget-limited fixture").unwrap();
        let reply = GoalCommand::Pause
            .execute(&node, "principal", "session")
            .await
            .unwrap();
        assert!(reply.contains("Goal remains budget_limited"));
        assert_eq!(
            gents::goal::load_canonical_goal(&node, "principal", "session")
                .await
                .unwrap()
                .unwrap()
                .status,
            "budget_limited"
        );
        let removed = node
            .execute(r#"mutation {delete_Goal(filter:{goal_id:{_eq:"owned"}}) {_docID}}"#)
            .await;
        ensure_no_errors(&removed, "remove goal fixture").unwrap();
        cursor
            .refresh(&node, "principal", "session", &sender, &projections)
            .await
            .unwrap();
        cursor
            .refresh(&node, "principal", "session", &sender, &projections)
            .await
            .unwrap();
        let lines = buffer.lock().await;
        assert_eq!(lines.len(), 2);
        let cleared: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(cleared["params"]["update"]["status"], "cleared");
        assert_eq!(cleared["params"]["update"]["goal_id"], "owned");
    }

    #[test]
    fn native_goal_snapshot_preserves_budget_usage_and_runtime_active_time() {
        let mut goal: GoalDocument = serde_json::from_value(json!({
            "_docID":"physical-goal", "goal_id":"goal-1", "session_id":"s", "agent_did":"a",
            "objective":"Finish the feature", "status":"active", "token_budget":1000,
            "tokens_used":123, "active_time_seconds":10,
            "active_started_at":"2026-09-01T00:00:00Z", "continuation_sequence":4
        }))
        .unwrap();
        let now = "2026-09-01T00:00:05Z".parse().unwrap();
        let update = project(&goal, now).unwrap();
        assert_eq!(update["sessionUpdate"], "goal_updated");
        assert_eq!(update["elapsed_ms"], 15000);
        assert_eq!(update["tokens_used"], 123);
        assert_eq!(update["token_budget"], 1000);
        assert_eq!(update["total_worker_rounds"], 0);
        for (runtime, native) in [
            ("paused", "user_paused"),
            ("blocked", "blocked"),
            ("usage_limited", "usage_limited"),
            ("budget_limited", "budget_limited"),
            ("complete", "complete"),
        ] {
            goal.status = runtime.into();
            let update = project(&goal, now).unwrap();
            assert_eq!(update["status"], native);
            assert_eq!(update["_meta"]["gents/goalStatus"], runtime);
            assert_eq!(update["elapsed_ms"], 10000);
        }
        goal.status = "unknown".into();
        assert!(project(&goal, now).is_err());
    }
}
