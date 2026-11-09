//! Stock task-button control, delegated to the runtime's process owner.
use anyhow::{Context, Result};
use gents::{
    store::graphql::escape_graphql_string, tools::call::hook::BackgroundExecutionRegistry,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::caused_sessions::{load_caused_sessions_as, SessionScope};

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

/// `access` is the bound Agent's managed access: it reads and settles the
/// Agent's own tool calls.
pub(super) async fn kill(
    access: &gents::ConfigAccess,
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
            node_did: principal.to_owned(),
            session_id: session.clone(),
            requester_did: Some(principal.to_owned()),
        })
        .collect::<Vec<_>>();
    for caused in load_caused_sessions_as(access, &scopes).await? {
        if !parent_addressed && caused.scope.session_id != request.session_id {
            continue;
        }
        candidates.push((
            caused.scope.session_id,
            caused.scope.node_did,
            caused.scope.requester_did,
        ));
    }
    // Resolve before mutating: a task ID shared by two visible owners must
    // not cancel whichever appears first.
    let mut matches = Vec::new();
    for candidate in candidates {
        let response = access.execute(&format!(r#"{{ AgentToolCall(filter: {{session_id: {{_eq: "{}"}}, tool_call_id: {{_eq: "{}"}}}}, limit: 2) {{_docID}} }}"#,
            escape_graphql_string(&candidate.0), escape_graphql_string(&request.task_id))).await?;
        let rows = response
            .pointer("/data/AgentToolCall")
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
        match gents::tools::call::control::cancel_session_background_process(
            access,
            executions,
            &agent,
            requester.as_deref(),
            &owner_session,
            &request.task_id,
        )
        .await?
        {
            Outcome::Queued { .. } => "kill_pending",
            Outcome::Cancelled { .. } => "killed",
            Outcome::Lost => "lost",
            Outcome::Unverified => "kill_unverified",
            Outcome::AlreadyTerminal { .. } => "already_exited",
            Outcome::NotBackground | Outcome::NotFound => "not_found",
        }
    } else {
        "not_found"
    };
    let _source = request.source;
    Ok(json!({"taskId":request.task_id, "outcome":outcome}))
}
