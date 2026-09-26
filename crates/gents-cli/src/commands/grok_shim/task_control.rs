//! Stock task-button control, delegated to the runtime's process owner.
use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::{graphql::escape_graphql_string, hook::BackgroundExecutionRegistry};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::caused_sessions::{load_caused_sessions, SessionScope};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct KillTaskRequest {
    pub session_id: String,
    pub task_id: String,
    #[serde(default)]
    pub source: KillSource,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum KillSource {
    #[default]
    ClientUi,
    Teardown,
}

pub(super) async fn kill(
    node: Arc<EmbeddedNode>,
    executions: &BackgroundExecutionRegistry,
    principal: &str,
    sessions: &[String],
    request: KillTaskRequest,
) -> Result<Value> {
    let parent_addressed = sessions.contains(&request.session_id);
    let roots = if parent_addressed {
        std::slice::from_ref(&request.session_id)
    } else {
        sessions
    };
    let mut candidates = Vec::new();
    if parent_addressed {
        candidates.push((
            request.session_id.clone(),
            principal.to_owned(),
            Some(principal.to_owned()),
        ));
    }
    // Stock child-pane actions use the top-level session ID. Resolve the
    // tool's actual owner through the sessions it caused.
    let scopes = roots
        .iter()
        .map(|session| SessionScope {
            agent_did: principal.to_owned(),
            session_id: session.clone(),
            requester_did: Some(principal.to_owned()),
        })
        .collect::<Vec<_>>();
    for caused in load_caused_sessions(&node, &scopes).await? {
        if !parent_addressed && caused.scope.session_id != request.session_id {
            continue;
        }
        candidates.push((
            caused.scope.session_id,
            caused.scope.agent_did,
            caused.scope.requester_did,
        ));
    }
    // Resolve before mutating. A task ID shared by two visible owners must
    // not cancel whichever one happens to appear first.
    let mut matches = Vec::new();
    for candidate in candidates {
        let response = gents::graphql::graphql_with_transaction_retry(&node, &format!(r#"{{ AgentToolCall(filter: {{session_id: {{_eq: "{}"}}, tool_call_id: {{_eq: "{}"}}}}, limit: 2) {{_docID}} }}"#,
            escape_graphql_string(&candidate.0), escape_graphql_string(&request.task_id)), "Grok task owner lookup").await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|v| v.get("AgentToolCall"))
            .and_then(Value::as_array)
            .context("missing task owner rows")?;
        if rows.len() > 1 {
            return Ok(json!({"taskId":request.task_id, "outcome":"not_found"}));
        }
        if rows.len() == 1 {
            matches.push(candidate);
        }
    }
    let scope = if matches.len() == 1 {
        matches.pop()
    } else {
        None
    };
    let outcome = if let Some((owner_session, agent, requester)) = scope {
        use gents::CancelBackgroundToolCallOutcome as Outcome;
        match gents::tool_control::cancel_session_background_process(
            node,
            executions,
            &agent,
            requester.as_deref(),
            &owner_session,
            &request.task_id,
        )
        .await?
        {
            Outcome::Cancelled { .. } => "killed",
            Outcome::Lost => "lost",
            Outcome::Unverified => "kill_unverified",
            Outcome::AlreadyTerminal { .. } => "already_exited",
            Outcome::NotBackground | Outcome::NotFound => "not_found",
        }
    } else {
        "not_found"
    };
    // Both stock kill sources are operator cancellation. Neither creates a
    // new request or a synthetic completion; runtime persistence drives UI.
    let _source = request.source;
    Ok(json!({"taskId":request.task_id, "outcome":outcome}))
}
