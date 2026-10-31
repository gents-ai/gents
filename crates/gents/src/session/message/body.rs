use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use std::sync::Arc;

use super::{MessageBody, RenderedBody};

pub(crate) async fn render_body(
    node: &std::sync::Arc<EmbeddedNode>,
    caller_node_did: &str,
    target_agent_id: &str,
    target_session_id: &str,
    body: MessageBody<'_>,
) -> Result<Result<RenderedBody, String>> {
    let task = match body {
        MessageBody::Prompt(prompt) => {
            return Ok(Ok(RenderedBody {
                content: prompt.to_owned(),
                goal: None,
            }))
        }
        MessageBody::Task(task) => task,
    };
    let task_id = task.task_id.trim();
    let Some(task_document) = load_configured_task(node, caller_node_did, task_id).await? else {
        return Ok(Err(format!("task '{task_id}' is not configured")));
    };
    if task_document.emit_outcome {
        return Ok(Err("emit_outcome Tasks require document-triggered delivery or explicit CLI/desktop Task admission; agent_new/agent_message Task bodies do not emit outcomes".into()));
    }
    Ok(render_task_body(
        &task_document,
        caller_node_did,
        target_agent_id,
        target_session_id,
        task.input.as_ref(),
    ))
}

pub(crate) async fn load_configured_task(
    node: &Arc<EmbeddedNode>,
    caller_node_did: &str,
    task_id: &str,
) -> Result<Option<crate::documents::Task>> {
    let loaded = crate::store::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "session_message.load_task",
        {
            let owner = caller_node_did.to_owned();
            let task_id = task_id.to_owned();
            move |txn| {
                let owner = owner.clone();
                let task_id = task_id.clone();
                Box::pin(async move {
                    crate::store::config_client::read_desired_state_document_in_txn(
                        txn,
                        crate::store::collection::Collection::Task,
                        &owner,
                        &task_id,
                    )
                    .await
                })
            }
        },
    )
    .await?;
    loaded
        .map(|value| serde_json::from_value(value).context("decode configured Task"))
        .transpose()
}

pub(crate) fn render_task_body(
    task_document: &crate::documents::Task,
    caller_node_did: &str,
    target_agent_id: &str,
    target_session_id: &str,
    input: Option<&serde_json::Value>,
) -> Result<RenderedBody, String> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (node_scope, ctx_scope) =
        crate::documents::template::task_node_ctx(caller_node_did, target_agent_id, &now);
    let scope = crate::documents::template::TemplateScope {
        session: Some(serde_json::json!({"session_id": target_session_id})),
        request: None,
        event: serde_json::Value::Null,
        doc: None,
        args: Some(input.cloned().unwrap_or(serde_json::json!({}))),
        group: None,
        node: node_scope,
        ctx: ctx_scope,
    };
    let (content, objective) = match crate::documents::template::render_task(
        &task_document.prompt_template,
        task_document.goal_objective_template.as_deref(),
        task_document.goal_token_budget,
        &scope,
    ) {
        Ok(rendered) => rendered,
        Err(error) => return Err(format!("task {error}")),
    };
    let goal = objective.map(|objective| (objective, task_document.goal_token_budget));
    Ok(RenderedBody { content, goal })
}
