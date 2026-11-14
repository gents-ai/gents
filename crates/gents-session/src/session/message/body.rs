use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;

use super::{MessageBody, RenderedBody};

pub async fn render_body(
    node: &std::sync::Arc<EmbeddedNode>,
    caller_agent_did: &str,
    caller_node_did: &str,
    target_agent_did: &str,
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
    let Some(task_document) = load_configured_task(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(caller_agent_did)?,
        ),
        caller_node_did,
        task_id,
    )
    .await?
    else {
        return Ok(Err(format!("task '{task_id}' is not configured")));
    };
    if task_document.emit_outcome {
        return Ok(Err("emit_outcome Tasks require document-triggered delivery or explicit CLI/desktop Task admission; agent_new/agent_message Task bodies do not emit outcomes".into()));
    }
    Ok(render_task_body(
        &task_document,
        caller_node_did,
        target_agent_did,
        target_session_id,
        task.input.as_ref(),
    ))
}

pub async fn load_configured_task(
    access: &gents_store::config_client::ConfigAccess,
    caller_node_did: &str,
    task_id: &str,
) -> Result<Option<gents_documents::Task>> {
    let loaded = access
        .transact_readonly("session_message.load_task", {
            let owner = caller_node_did.to_owned();
            let task_id = task_id.to_owned();
            move |txn| {
                let owner = owner.clone();
                let task_id = task_id.clone();
                Box::pin(async move {
                    gents_documents::read_desired_state_document_in_txn(
                        txn,
                        gents_store::collection::Collection::Task,
                        &owner,
                        &task_id,
                    )
                    .await
                })
            }
        })
        .await?;
    loaded
        .map(|value| serde_json::from_value(value).context("decode configured Task"))
        .transpose()
}

pub fn render_task_body(
    task_document: &gents_documents::Task,
    caller_node_did: &str,
    target_agent_did: &str,
    target_session_id: &str,
    input: Option<&serde_json::Value>,
) -> Result<RenderedBody, String> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (node_scope, ctx_scope) =
        gents_documents::template::task_node_ctx(caller_node_did, target_agent_did, &now);
    let scope = gents_documents::template::TemplateScope {
        session: Some(serde_json::json!({"session_id": target_session_id})),
        request: None,
        event: serde_json::Value::Null,
        doc: None,
        args: Some(input.cloned().unwrap_or(serde_json::json!({}))),
        group: None,
        node: node_scope,
        ctx: ctx_scope,
    };
    let (content, objective) = match gents_documents::template::render_task(
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
