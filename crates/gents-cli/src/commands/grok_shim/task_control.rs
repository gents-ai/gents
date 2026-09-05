//! Stock task-button control, delegated to the runtime's process owner.
use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::{
    graphql::{ensure_no_errors, escape_graphql_string},
    hook::BackgroundExecutionRegistry,
};
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

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
    let scope = if sessions.contains(&request.session_id) {
        Some((principal.to_owned(), Some(principal.to_owned())))
    } else {
        let edges =
            super::projection::subagents::control::authorized_children(&node, principal, sessions)
                .await?;
        match edges
            .get(&request.session_id)
            .filter(|(_, edge)| edge.controllable())
        {
            None => None,
            Some((_, edge)) => {
                let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{ request_id session_id agent_did requester_did }} }}"#, escape_graphql_string(&edge.child_request_id))).await;
                ensure_no_errors(&response, "Grok child process scope")?;
                let rows: Vec<AgentRequestRow> = serde_json::from_value(
                    response
                        .data
                        .as_ref()
                        .and_then(|v| v.get("AgentRequest"))
                        .cloned()
                        .context("missing child process scope")?,
                )?;
                match rows.as_slice() {
                    [row]
                        if row.session_id.as_deref() == Some(&request.session_id)
                            && row.agent_did == edge.principal_did =>
                    {
                        row.agent_did
                            .clone()
                            .map(|agent| (agent, row.requester_did.clone()))
                    }
                    _ => None,
                }
            }
        }
    };
    let outcome = if let Some((agent, requester)) = scope {
        use gents::CancelBackgroundToolCallOutcome as Outcome;
        match gents::tool_control::cancel_session_background_process(
            node,
            executions,
            &agent,
            requester.as_deref(),
            &request.session_id,
            &request.task_id,
        )
        .await?
        {
            Outcome::Cancelled { .. } => "killed",
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
