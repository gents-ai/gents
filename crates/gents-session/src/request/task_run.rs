//! Firing one Task on demand: render its prompt and goal, sign the request as
//! the actor `access` acts for, admit it through the Task delivery owner. The
//! CLI's `gents automation task run` and the Engineer's `config task run` both come here.

use anyhow::{anyhow, Context, Result};
use gents_protocol::request_input::RequestInput;
use gents_protocol::row::AgentRequestRow;
use serde::Serialize;
use serde_json::Value;

use crate::goal::{load_canonical_goal_in_txn, prepare_client_goal_input, task_assignment_goal_id};
use gents_documents::task_goal_declaration::validate_task_goal_declaration;
use gents_documents::template::{
    render_manual_template, render_template, task_node_ctx, TemplateScope,
};
use gents_documents::{read_desired_state_record_in_txn, Agent, Task};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

/// What a caller supplies for one fire.
#[derive(Debug, Clone)]
pub struct TaskRunOptions {
    /// A JSON object bound as the `args.*` template scope.
    pub args: Value,
    /// A goal-backed Task's stable invocation key; otherwise a session label.
    pub session_id: Option<String>,
    /// An existing session to continue with this Task.
    pub continue_session: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRunOutput {
    pub task_id: String,
    pub agent_did: String,
    pub node_did: String,
    pub request_id: String,
    pub session_id: String,
    pub request_doc_id: String,
    pub input: RequestInput,
    pub status: &'static str,
    pub duplicate: bool,
}

/// A Task loaded for firing, with its enabled agent verified.
pub struct RunnableTask {
    pub task: Task,
    doc_id: String,
}

/// Loads an enabled Task whose agent is enabled, or fails naming which is not.
pub async fn read_runnable_task(
    access: &ConfigAccess,
    node_did: &str,
    task_id: &str,
) -> Result<RunnableTask> {
    let (doc_id, task) = access
        .transact_write("task_run.read", |txn| {
            Box::pin(async move {
                let (task_doc_id, value) = read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Task,
                    node_did,
                    task_id,
                )
                .await?
                .context("task not found under the selected node")?;
                let task: Task = serde_json::from_value(value)?;
                anyhow::ensure!(task.enabled, "Task {} is disabled", task.task_id);
                let (_, value) = read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    node_did,
                    &task.agent_did,
                )
                .await?
                .context("task agent not found under the selected node")?;
                let agent: Agent = serde_json::from_value(value)?;
                anyhow::ensure!(agent.enabled, "Agent {} is disabled", agent.agent_did);
                Ok((task_doc_id, task))
            })
        })
        .await?;
    Ok(RunnableTask { task, doc_id })
}

/// Fires `task` once, now: renders its prompt and goal with `options.args`,
/// signs the request as `signer`, which must be the actor `access` acts for,
/// and admits it through the Task delivery owner.
pub async fn enqueue_task_run(
    access: &ConfigAccess,
    node_did: &str,
    signer: &dyn NodeIdentity,
    runnable: RunnableTask,
    options: &TaskRunOptions,
) -> Result<TaskRunOutput> {
    anyhow::ensure!(
        options.args.is_object(),
        "task args must be a JSON object (got: {})",
        options.args
    );
    let actor_did = access
        .actor_did()
        .context("Task invocation requires an authenticated client identity")?;
    let RunnableTask {
        task,
        doc_id: task_doc_id,
    } = runnable;
    let task_id = task.task_id.clone();
    let emit_outcome = task.emit_outcome;
    let agent_did = task.agent_did;
    let prompt_template = task.prompt_template;
    let goal_objective_template = task.goal_objective_template;
    let goal_token_budget = task.goal_token_budget;
    validate_task_goal_declaration(goal_objective_template.as_deref(), goal_token_budget)?;

    let invocation_key = resolve_invocation_key(
        &task_id,
        goal_objective_template.is_some(),
        goal_objective_template
            .as_ref()
            .and(options.session_id.as_deref()),
    )?;
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: node_did.to_owned(),
        trigger_id: format!("manual:{task_id}:{invocation_key}"),
        source_collection: "Task".into(),
        source_doc_id: task_doc_id,
    };
    let fire_key = crate::triggers::task_fire_key(&identity);
    let request_id = format!("trigger-request:{fire_key}");
    anyhow::ensure!(
        goal_objective_template.is_some()
            || options.session_id.is_none()
            || options.continue_session.is_none(),
        "ordinary Tasks accept either a session label or an existing session to continue, not both"
    );
    let session_id = match options.continue_session.as_deref() {
        Some(id) => {
            anyhow::ensure!(
                !id.trim().is_empty(),
                "--continue-session must not be empty"
            );
            id.to_owned()
        }
        None => options
            .session_id
            .as_ref()
            .filter(|id| goal_objective_template.is_none() && !id.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("trigger-session:{fire_key}")),
    };
    let admitted = lookup_request_by_retry_key(access, node_did, &fire_key).await?;
    let prior_created_at = admitted
        .as_ref()
        .map(|request| {
            anyhow::ensure!(
                request.request_id == request_id,
                "Task fire identity conflicts with request ID"
            );
            request
                .created_at
                .clone()
                .context("Task fire request is missing created_at")
        })
        .transpose()?;
    let now = prior_created_at
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let (node_scope, ctx_scope) = task_node_ctx(node_did, &agent_did, &now);
    let scope = TemplateScope {
        session: Some(serde_json::json!({"session_id": session_id})),
        request: Some(serde_json::json!({"request_id": request_id})),
        event: serde_json::json!({
            "fired_at": now,
            "trigger_id": serde_json::Value::Null,
            "trigger_kind": "manual",
        }),
        doc: None,
        args: Some(options.args.clone()),
        group: None,
        node: node_scope,
        ctx: ctx_scope,
    };
    let content = render_manual_template(&task_id, &prompt_template, &scope)?;
    let rendered_goal_objective = goal_objective_template
        .as_deref()
        .map(|template| {
            render_template(template, &scope)
                .map_err(|e| anyhow!("render goal template for task {}: {e}", task_id))
        })
        .transpose()?;
    if rendered_goal_objective
        .as_deref()
        .is_some_and(|objective| objective.trim().is_empty())
    {
        anyhow::bail!("Task {} rendered an empty goal objective", task_id);
    }

    let (content, input) =
        gents_documents::skills::content_and_input_with_prompt_selected_skill_ids(None, &content);
    let input = prepare_task_run_input(
        access,
        signer,
        node_did,
        &session_id,
        options.continue_session.is_some(),
        rendered_goal_objective.is_some(),
        admitted.as_ref(),
        input,
    )
    .await?;
    let goal_id = prepare_task_run_goal_id(
        access,
        node_did,
        &session_id,
        rendered_goal_objective.is_some(),
        admitted.as_ref(),
    )
    .await?;
    let requester_did =
        crate::session::client_requester_scope(access, node_did, &session_id, &actor_did).await?;
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(&actor_did);
    let create = crate::request::lifecycle::build_signed_client_request(
        access,
        crate::request::lifecycle::RequestSpec {
            input: input.clone(),
            trigger_lineage: crate::request::lifecycle::TriggerLineage {
                trigger_kind: Some("manual".to_string()),
                ..Default::default()
            },
            retry_key: Some(fire_key.clone()),
            ..crate::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                crate::request::lifecycle::RequestIdentity {
                    request_id: request_id.clone(),
                    node_did: node_did.to_owned(),
                    requester_did: Some(requester_did),
                    agent_did: agent_did.clone(),
                    session_id: session_id.clone(),
                    content: content.clone(),
                    execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                    created_at: now.clone(),
                },
                admission,
            )
        },
        crate::request::lifecycle::RequestSigner::Identity(signer),
    )
    .await?;
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key,
        identity,
        task_id: task_id.clone(),
        request_id: request_id.clone(),
        session_id: session_id.clone(),
        goal_id,
        goal_objective: rendered_goal_objective,
        goal_token_budget,
        goal_assignment_applied: false,
        emit_outcome,
        queued_serial: false,
        source_handoff_id: Some(invocation_key),
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: now,
    };
    let admitted = crate::triggers::write_task_delivery(
        access,
        &fire,
        options.continue_session.is_some(),
        &create,
    )
    .await?;

    let (agent_did, input) = if admitted.duplicate {
        let persisted = lookup_request_by_retry_key(access, node_did, &fire.fire_key)
            .await?
            .context("duplicate Task fire is missing its admitted request")?;
        (
            persisted
                .agent_did
                .context("admitted Task request lacks agent_did")?,
            persisted.input.unwrap_or_default(),
        )
    } else {
        (agent_did, input)
    };
    Ok(TaskRunOutput {
        task_id,
        agent_did,
        node_did: node_did.to_owned(),
        request_id: admitted.request.request_id,
        session_id: admitted.request.session_id,
        request_doc_id: admitted.request.doc_id,
        input,
        status: if admitted.duplicate {
            "duplicate"
        } else {
            "pending"
        },
        duplicate: admitted.duplicate,
    })
}

pub fn resolve_invocation_key(
    task_id: &str,
    goal_backed: bool,
    stable_key: Option<&str>,
) -> Result<String> {
    match stable_key {
        Some(key) => {
            anyhow::ensure!(!key.trim().is_empty(), "--session-id must not be empty");
            Ok(key.to_owned())
        }
        None if goal_backed => anyhow::bail!(
            "Task {task_id} declares a durable goal; pass --session-id with a stable invocation identity so retries converge"
        ),
        None => Ok(uuid::Uuid::new_v4().to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn prepare_task_run_input(
    access: &ConfigAccess,
    verifier: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    continue_existing: bool,
    goal_backed: bool,
    admitted: Option<&AgentRequestRow>,
    input: RequestInput,
) -> Result<RequestInput> {
    if let Some(admitted) = admitted {
        let actor = access
            .actor_did()
            .context("Task invocation requires an authenticated client identity")?;
        anyhow::ensure!(
            admitted.node_did.as_deref() == Some(node_did)
                && admitted.admission_signer_did.as_deref() == Some(actor.as_str())
                && admitted.session_id.as_deref() == Some(session_id),
            "admitted Task input differs from its owner/session"
        );
        return Ok(admitted.input.clone().unwrap_or_default());
    }
    if goal_backed || !continue_existing {
        return Ok(input);
    }
    prepare_client_goal_input(access, verifier, node_did, session_id, input).await
}

/// Duplicate admission preserves the stored fire, so an untagged signed receipt
/// must not infer its original Task Goal identity from the current Goal row.
pub async fn prepare_task_run_goal_id(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    goal_backed: bool,
    admitted: Option<&AgentRequestRow>,
) -> Result<Option<String>> {
    if let Some(admitted) = admitted {
        return Ok(admitted
            .input
            .as_ref()
            .and_then(|input| input.goal_assignment.as_ref())
            .map(|reference| reference.goal_id.clone()));
    }
    if !goal_backed {
        return Ok(None);
    }
    access
        .transact_readonly("cli.task_run.goal_identity", |txn| {
            Box::pin(async move {
                let current = load_canonical_goal_in_txn(txn, node_did, session_id).await?;
                Ok(Some(task_assignment_goal_id(
                    current.as_ref().map(|goal| goal.goal_id.as_str()),
                    node_did,
                    session_id,
                )))
            })
        })
        .await
}

pub async fn lookup_request_by_retry_key(
    access: &ConfigAccess,
    node_did: &str,
    retry_key: &str,
) -> Result<Option<AgentRequestRow>> {
    let actor = access
        .actor_did()
        .context("Task invocation requires an authenticated client identity")?;
    let query = format!(
        r#"query {{
            AgentRequest(filter: {{ node_did: {{ _eq: "{owner}" }}, admission_signer_did: {{ _eq: "{actor}" }}, retry_key: {{ _eq: "{key}" }} }}, limit: 2) {{
                {signed_fields}
            }}
        }}"#,
        actor = escape_graphql_string(&actor),
        key = escape_graphql_string(retry_key),
        owner = escape_graphql_string(node_did),
        signed_fields = gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
    );
    let response = access.execute(&query).await?;
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            anyhow::bail!("lookup Task fire request failed: {errors:?}");
        }
    }
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if rows.len() > 1 {
        anyhow::bail!("Task fire retry_key resolved to multiple AgentRequest rows");
    }
    rows.first()
        .cloned()
        .map(|row| {
            let row: AgentRequestRow = serde_json::from_value(row)
                .context("decoding Task fire canonical AgentRequest row")?;
            gents_store::identity::request_receipt::verify_request_receipt_signature(&row)?;
            Ok(row)
        })
        .transpose()
}
