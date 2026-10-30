use anyhow::{Context, Result};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use defra_node::EmbeddedNode;
use serde::Deserialize;

use std::sync::Arc;

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::lifecycle::{SessionMessageCause, SessionMessageTarget};
use crate::session_origin::SessionScope;
use crate::tool_surface::AgentToolConfig;

#[cfg(test)]
mod goal_input_tests;

mod discovery;
pub(crate) use discovery::{agent_session_list, AgentSessionListArgs};
mod inspection;
pub(crate) use inspection::{agent_session_read, AgentSessionReadArgs};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskBody {
    pub task_id: String,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentNewArgs {
    pub agent: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub task: Option<TaskBody>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentMessageArgs {
    pub session_id: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub task: Option<TaskBody>,
    #[serde(default)]
    pub interrupt: Option<bool>,
    #[serde(default)]
    pub delivery: Option<AgentMessageDelivery>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentMessageDelivery {
    Queue,
    Steer,
    Interrupt,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentInterruptArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentListArgs {}

pub(crate) enum MessageBody<'a> {
    Prompt(&'a str),
    Task(&'a TaskBody),
}

pub(crate) fn message_body<'a>(
    field: &str,
    prompt: Option<&'a String>,
    task: Option<&'a TaskBody>,
) -> Result<MessageBody<'a>, String> {
    match (prompt.map(|prompt| prompt.trim()), task) {
        (Some(prompt), None) if !prompt.is_empty() => Ok(MessageBody::Prompt(prompt)),
        (None, Some(task)) if !task.task_id.trim().is_empty() => Ok(MessageBody::Task(task)),
        (Some(_), None) => Err(format!("{field} must be non-empty")),
        (None, Some(_)) => Err("task.task_id must be non-empty".to_owned()),
        _ => Err(format!("provide exactly one of {field} or task")),
    }
}

impl AgentNewArgs {
    pub(crate) fn body(&self) -> Result<MessageBody<'_>, String> {
        message_body("prompt", self.prompt.as_ref(), self.task.as_ref())
    }
}

impl AgentMessageArgs {
    pub(crate) fn body(&self) -> Result<MessageBody<'_>, String> {
        message_body("message", self.message.as_ref(), self.task.as_ref())
    }

    pub(crate) fn delivery(&self) -> Result<AgentMessageDelivery, String> {
        match (self.delivery, self.interrupt) {
            (None, None | Some(false)) => Ok(AgentMessageDelivery::Queue),
            (None, Some(true)) | (Some(AgentMessageDelivery::Interrupt), Some(true)) => {
                Ok(AgentMessageDelivery::Interrupt)
            }
            (Some(delivery), None) => Ok(delivery),
            (
                Some(delivery @ (AgentMessageDelivery::Queue | AgentMessageDelivery::Steer)),
                Some(false),
            ) => Ok(delivery),
            (Some(_), Some(_)) => Err("interrupt conflicts with delivery".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Request,
    Queued,
    Steering,
}

impl Delivery {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Queued => "queue",
            Self::Steering => "steer",
        }
    }
}

pub(crate) struct RenderedBody {
    pub content: String,
    pub goal: Option<(String, Option<i64>)>,
}

pub(crate) async fn load_caller_session_tools(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) -> Result<AgentToolConfig> {
    let (_, tools, targets) =
        crate::request_admission::load_request_context(node, node_did, agent_id).await?;
    tools
        .map(|tools| AgentToolConfig::from_document_with_targets(&tools, &targets))
        .transpose()
        .map(Option::unwrap_or_default)
}

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
) -> Result<Option<crate::document_config::Task>> {
    let loaded = crate::config_client::ConfigAccess::transact_local_readonly(
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
                    crate::config_client::read_desired_state_document_in_txn(
                        txn,
                        crate::collection::Collection::Task,
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
    task_document: &crate::document_config::Task,
    caller_node_did: &str,
    target_agent_id: &str,
    target_session_id: &str,
    input: Option<&serde_json::Value>,
) -> Result<RenderedBody, String> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (node_scope, ctx_scope) =
        crate::template::task_node_ctx(caller_node_did, target_agent_id, &now);
    let scope = crate::template::TemplateScope {
        session: Some(serde_json::json!({"session_id": target_session_id})),
        request: None,
        event: serde_json::Value::Null,
        doc: None,
        args: Some(input.cloned().unwrap_or(serde_json::json!({}))),
        group: None,
        node: node_scope,
        ctx: ctx_scope,
    };
    let (content, objective) = match crate::template::render_task(
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

pub(crate) async fn resolve_send_target(
    node: &Arc<EmbeddedNode>,
    caller_node_did: &str,
    tools: &AgentToolConfig,
    session_id: &str,
) -> Result<Option<SessionMessageTarget>> {
    let Some(session) = crate::session_origin::load_session(
        &ConfigAccess::local(node.clone()),
        session_id,
        Some(caller_node_did),
    )
    .await?
    else {
        return Ok(None);
    };
    Ok(
        session_target_allowed(&session, caller_node_did, tools).then_some(SessionMessageTarget {
            node_did: session.node_did,
            agent_id: session.agent_id,
            session_id: session.session_id,
        }),
    )
}

fn session_target_allowed(
    session: &gents_protocol::session::AgentSession,
    caller_node_did: &str,
    tools: &AgentToolConfig,
) -> bool {
    session.requester_did.as_deref() == Some(caller_node_did)
        && (session.node_did == caller_node_did
            || (session
                .provenance
                .as_ref()
                .is_some_and(|origin| origin.parent_request_doc_id.is_some())
                && tools.allows(&session.node_did, &session.agent_id)))
}

pub(crate) fn target_scope(caller_node_did: &str, target: &SessionMessageTarget) -> SessionScope {
    SessionScope {
        node_did: target.node_did.clone(),
        session_id: target.session_id.clone(),
        requester_did: Some(caller_node_did.to_owned()),
    }
}

enum PlannedWrite {
    Request {
        create: gents_protocol::request_admission::AgentRequestCreate,
        delivery: Delivery,
    },
    Goal {
        create: gents_protocol::request_admission::AgentRequestCreate,
        objective: String,
        token_budget: Option<i64>,
    },
}

pub(crate) struct Plan {
    session_id: String,
    hop: u32,
    steer_head: Option<Option<String>>,
    interrupt_before_publish: bool,
    write: PlannedWrite,
}

impl Plan {
    pub(crate) async fn sign_agent_creation(&mut self, node_did: &str) -> Result<()> {
        let create = match &mut self.write {
            PlannedWrite::Request { create, .. } | PlannedWrite::Goal { create, .. } => create,
        };
        create.input.session_creation =
            Some(gents_protocol::request_input::SessionCreationIntent::AgentNew);
        let signer = crate::identity::RegisteredIdentity::from_registered_did(node_did, None)?;
        crate::sign_agent_request_create(&signer, create).await
    }
    pub(crate) fn hop(&self) -> u32 {
        self.hop
    }

    pub(crate) fn request(
        session_id: String,
        hop: u32,
        create: gents_protocol::request_admission::AgentRequestCreate,
    ) -> Self {
        Self {
            session_id,
            hop,
            steer_head: None,
            interrupt_before_publish: false,
            write: PlannedWrite::Request {
                create,
                delivery: Delivery::Request,
            },
        }
    }

    pub(crate) fn delivery(&self) -> Delivery {
        match self.write {
            PlannedWrite::Request { delivery, .. } => delivery,
            PlannedWrite::Goal { .. } => Delivery::Request,
        }
    }
}

async fn target_goal_assignment(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
) -> Result<Option<gents_protocol::request_input::GoalAssignmentInput>> {
    use gents_protocol::request_input::GoalAssignmentInput;
    if target.node_did == cause.caller_node_did {
        return ConfigAccess::transact_local_readonly(
            node,
            None,
            "session_message.target_goal",
            |txn| {
                Box::pin(async move {
                    let Some(session) = crate::session::load_agent_session_row_in_txn(
                        txn,
                        &target.node_did,
                        &target.session_id,
                        Some(&cause.caller_node_did),
                    )
                    .await?
                    else {
                        return Ok(None);
                    };
                    anyhow::ensure!(
                        session.session.agent_id == target.agent_id,
                        "session-message target Agent differs from its canonical session"
                    );
                    let goal = crate::goal::load_canonical_goal_in_txn(
                        txn,
                        &target.node_did,
                        &target.session_id,
                    )
                    .await?;
                    Ok(goal.and_then(|goal| {
                        goal.assignment_root_request_doc_id
                            .map(|root| GoalAssignmentInput {
                                goal_id: goal.goal_id,
                                root_request_doc_id: Some(root),
                            })
                    }))
                })
            },
        )
        .await;
    }
    let verifier = crate::identity::RegisteredIdentity::from_registered_did(
        cause.caller_node_did.clone(),
        None,
    )?;
    let state = crate::goal::load_session_goal_observation_state(
        &ConfigAccess::local(node.clone()),
        &verifier,
        &target.node_did,
        &target.session_id,
        Some(&cause.caller_node_did),
    )
    .await?;
    match state {
        crate::goal::SessionGoalObservationState::NoReadableSession => Ok(None),
        crate::goal::SessionGoalObservationState::Unknown => {
            anyhow::bail!("The target chat's Goal state is still syncing. Read the chat and retry the message.")
        }
        crate::goal::SessionGoalObservationState::Observed(observation) => {
            Ok(observation.goal.and_then(|goal| {
                goal.assignment_root_request_doc_id
                    .map(|root| GoalAssignmentInput {
                        goal_id: goal.snapshot.goal_id,
                        root_request_doc_id: Some(root),
                    })
            }))
        }
    }
}

#[cfg(test)]
pub(crate) async fn plan(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    rendered: RenderedBody,
    title: Option<&str>,
    interrupt: bool,
) -> Result<Result<Plan, String>> {
    plan_with_delivery(
        node,
        cause,
        target,
        rendered,
        title,
        if interrupt {
            AgentMessageDelivery::Interrupt
        } else {
            AgentMessageDelivery::Queue
        },
    )
    .await
}

pub(crate) async fn plan_with_delivery(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    rendered: RenderedBody,
    title: Option<&str>,
    delivery: AgentMessageDelivery,
) -> Result<Result<Plan, String>> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let local = target.node_did == cause.caller_node_did;
    if !local && delivery == AgentMessageDelivery::Steer {
        return Ok(Err(
            "steering requires a local target session; use delivery=queue".to_owned(),
        ));
    }
    if crate::session::session_is_stopped(
        node,
        &target.node_did,
        &target.session_id,
        Some(&cause.caller_node_did),
    )
    .await?
    {
        return Ok(Err(
            "session is stopped; use agent_resume before sending another message".to_owned(),
        ));
    }
    let active = if local {
        match crate::interrupt::active_session_request(
            node,
            &target.session_id,
            &target.node_did,
            Some(&cause.caller_node_did),
        )
        .await?
        {
            Some(active) => {
                let active_doc_id = active
                    .doc_id
                    .as_deref()
                    .context("active session request lacks physical identity")?;
                Some(
                    crate::request_binding::load_agent_request_by_doc_id(node, active_doc_id)
                        .await?
                        .context("active session request disappeared")?,
                )
            }
            None => None,
        }
    } else {
        None
    };
    let steer_head = (delivery == AgentMessageDelivery::Steer)
        .then(|| active.as_ref().map(|request| request.doc_id.clone()));
    let own_hop = match &active {
        Some(active) => active.request_hop.max(
            crate::session::load_session_current_hop(node, &target.node_did, &target.session_id)
                .await?,
        ),
        None => {
            crate::session::load_session_current_hop(node, &target.node_did, &target.session_id)
                .await?
        }
    };
    let hop = crate::lifecycle::next_request_hop(
        crate::lifecycle::RequestHopCause::CrossSession {
            cause_hop: cause.caller_hop,
        },
        own_hop,
    );
    let write = if let Some((objective, token_budget)) = rendered.goal {
        if !local {
            return Ok(Err(
                "a Task that declares a Goal can only address this principal's own sessions"
                    .to_owned(),
            ));
        }
        if active.is_some() {
            return Ok(Err(
                "a Task that declares a Goal requires an idle session".to_owned()
            ));
        }
        let create = crate::lifecycle::build_session_message_request(
            cause,
            target,
            &rendered.content,
            title,
            &request_id,
            Some(format!("session-message:{}", cause.tool_call_doc_id)),
            hop,
            None,
            None,
        )
        .await?;
        PlannedWrite::Goal {
            create,
            objective,
            token_budget,
        }
    } else {
        let steering = active.filter(|_| delivery != AgentMessageDelivery::Interrupt);
        let queue = steering
            .as_ref()
            .map(|active| gents_protocol::request_input::RequestQueue {
                delivery: if delivery == AgentMessageDelivery::Steer {
                    gents_protocol::request_input::QueueDelivery::Steer
                } else {
                    gents_protocol::request_input::QueueDelivery::Queue
                },
                position: None,
                source: gents_protocol::request_input::QueueSource::Steering,
                policy: gents_protocol::request_input::QueuePolicy::Append,
                key: None,
                queued_after_request_id: Some(active.request_id.clone()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
        PlannedWrite::Request {
            create: crate::lifecycle::materialize::build_session_message_request_with_input(
                cause,
                target,
                &rendered.content,
                title,
                &request_id,
                None,
                hop,
                gents_protocol::request_input::RequestInput {
                    queue,
                    goal_assignment: target_goal_assignment(node, cause, target).await?,
                    ..Default::default()
                },
            )
            .await?,
            delivery: match (&steering, delivery) {
                (None, _) => Delivery::Request,
                (Some(_), AgentMessageDelivery::Steer) => Delivery::Steering,
                (Some(_), _) => Delivery::Queued,
            },
        }
    };
    Ok(Ok(Plan {
        session_id: target.session_id.clone(),
        hop,
        steer_head,
        interrupt_before_publish: delivery == AgentMessageDelivery::Interrupt,
        write,
    }))
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub(crate) struct SessionMessageReceipt {
    pub ok: bool,
    pub session_id: String,
    pub request_id: String,
    pub request_doc_id: String,
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    pub await_mode: String,
    pub status: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CommitError {
    #[error(transparent)]
    Steer(#[from] crate::lifecycle::queue::SteerAdmissionRefusal),
    #[error(transparent)]
    Access(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl crate::config_client::TransactionFailure for CommitError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Other(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            Self::Steer(_) => None,
        }
    }
}

pub(crate) async fn commit(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
    plan: Plan,
    report_delivery: bool,
) -> std::result::Result<SessionMessageReceipt, CommitError> {
    let error = match commit_once(node, cause, lifecycle, plan, report_delivery).await {
        Ok(receipt) => return Ok(receipt),
        Err(error) => error,
    };
    let doc_id = lifecycle
        .doc_id()
        .context("session-message row lacks physical identity")?
        .to_owned();
    let durable = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &doc_id,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    .context("session-message row disappeared after its commit failed")?;
    if durable.state() == crate::tool_call_lifecycle::ToolCallState::Pending {
        return Err(error);
    }
    let receipt = load_receipt(
        node,
        &doc_id,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    .filter(|_| durable.is_running())
    .with_context(|| format!("session-message commit outcome is unknown after: {error:#}"))?;
    *lifecycle = durable;
    Ok(receipt)
}

async fn commit_once(
    node: &std::sync::Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
    plan: Plan,
    report_delivery: bool,
) -> std::result::Result<SessionMessageReceipt, CommitError> {
    let delivery = report_delivery.then(|| plan.delivery().as_str().to_owned());
    let tool_call_id = lifecycle.tool_call_id().to_owned();
    let receipt_for =
        move |enqueued: &crate::lifecycle::EnqueuedAgentRequest| SessionMessageReceipt {
            ok: true,
            session_id: enqueued.session_id.clone(),
            request_id: enqueued.request_id.clone(),
            request_doc_id: enqueued.doc_id.clone(),
            tool_call_id: tool_call_id.clone(),
            delivery: delivery.clone(),
            await_mode: "background".to_owned(),
            status: "running".to_owned(),
        };
    let binding = lifecycle.background_receipt_binding()?;
    let start = lifecycle.dispatch_start()?;
    let (start, binding, plan, receipt_for) = (&start, &binding, &plan, &receipt_for);
    let (actor, label) = match &plan.write {
        PlannedWrite::Request { .. } => (None, "session_message.commit_request"),
        PlannedWrite::Goal { .. } => (
            Some(
                ::identity::Did::new(cause.caller_node_did.clone())
                    .context("caller DID is not ACP-addressable")?,
            ),
            "session_message.commit_goal",
        ),
    };
    let (receipt, started_at) =
        crate::config_client::ConfigAccess::transact_local_write(node, actor, label, move |txn| {
            Box::pin(async move {
                let started_at = start_dispatch(txn, start).await?;
                let enqueued = stage_planned_write_in_txn(txn, cause, plan).await?;
                let receipt = receipt_for(&enqueued);
                crate::tool_call_lifecycle::publish_background_receipt_in_txn(
                    txn,
                    binding,
                    &serde_json::to_string(&receipt).map_err(anyhow::Error::from)?,
                )
                .await?;
                Ok::<_, CommitError>((receipt, started_at))
            })
        })
        .await?;
    lifecycle.mark_started(started_at);
    Ok(receipt)
}

/// Create the planned request, or stage the Goal with it, in the caller's
/// transaction. It writes no tool row.
pub(crate) async fn stage_planned_write_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    cause: &SessionMessageCause,
    plan: &Plan,
) -> std::result::Result<crate::lifecycle::EnqueuedAgentRequest, CommitError> {
    if plan.interrupt_before_publish {
        let create = match &plan.write {
            PlannedWrite::Request { create, .. } | PlannedWrite::Goal { create, .. } => create,
        };
        let active = crate::interrupt::interrupt_active_session_in_txn(
            txn,
            &create.session_id,
            &create.node_did,
            Some(create.requester_did.as_str()),
        )
        .await?;
        if active.is_none() && create.node_did == cause.caller_node_did {
            crate::goal::pause_goal_for_interrupt_in_txn(txn, &create.node_did, &create.session_id)
                .await?;
        }
    }
    match &plan.write {
        PlannedWrite::Request { create, .. } => {
            ensure_planned_hop_current(txn, create).await?;
            let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
            let response = txn.execute(&mutation).await?;
            let doc_id = crate::graphql::created_doc_id(&response, "AgentRequest")
                .map_err(anyhow::Error::from)?;
            if let Some(expected_head) = &plan.steer_head {
                validate_steer_in_txn(txn, create, &doc_id, expected_head.as_deref()).await??;
            }
            Ok(crate::lifecycle::EnqueuedAgentRequest {
                doc_id,
                request_id: create.request_id.clone(),
                session_id: create.session_id.clone(),
            })
        }
        PlannedWrite::Goal {
            create,
            objective,
            token_budget,
        } => {
            ensure_planned_hop_current(txn, create).await?;
            let enqueued = crate::goal::stage_goal_backed_request_in_txn(
                txn,
                &create.node_did,
                &plan.session_id,
                objective,
                *token_budget,
                create,
            )
            .await?;
            if let Some(expected_head) = &plan.steer_head {
                validate_steer_in_txn(txn, create, &enqueued.doc_id, expected_head.as_deref())
                    .await??;
            }
            Ok(enqueued)
        }
    }
}

async fn validate_steer_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    create: &gents_protocol::request_admission::AgentRequestCreate,
    candidate_doc_id: &str,
    expected_head: Option<&str>,
) -> Result<std::result::Result<(), crate::lifecycle::queue::SteerAdmissionRefusal>> {
    let response = txn.execute(&format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }},
            purpose: {{ _eq: "normal" }}, lifecycle_state: {{ _in: {claimed_or_processing} }} }}, limit: 2) {{
            {} execution_lease_secs interrupt_requested_at
        }} }}"#,
        escape_graphql_string(&create.node_did),
        escape_graphql_string(&create.session_id),
        crate::request_admission::SIGNED_REQUEST_FIELDS, claimed_or_processing = RequestLifecycleState::graphql_list([RequestLifecycleState::Claimed, RequestLifecycleState::Processing])
    )).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple active requests in target session"
    );
    anyhow::ensure!(
        rows.first().and_then(|row| row.doc_id.as_deref()) == expected_head,
        "the target session changed while the steer was planned"
    );
    let Some(row) = rows.into_iter().next() else {
        return Ok(Ok(()));
    };
    let head = crate::watcher::AgentRequest::try_from(row)?;
    let pending = crate::lifecycle::queue::pending_with_new_candidate_in_txn(
        txn,
        &create.node_did,
        &create.session_id,
        candidate_doc_id,
    )
    .await?;
    Ok(crate::lifecycle::queue::steer_reachable(
        &head,
        pending,
        candidate_doc_id,
        chrono::Utc::now(),
    ))
}

async fn ensure_planned_hop_current(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    let current =
        crate::session::load_session_current_hop_in_txn(txn, &create.node_did, &create.session_id)
            .await?;
    anyhow::ensure!(
        create.request_hop >= current,
        "the target session moved to hop {current} while this message was planned"
    );
    Ok(())
}

async fn start_dispatch(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    start: &crate::tool_call_lifecycle::delivery::DispatchStart,
) -> Result<chrono::DateTime<chrono::Utc>> {
    crate::tool_call_lifecycle::delivery::start_running_in_txn(txn, start, None)
        .await?
        .context("session-message row was altered or is no longer pending")
}

pub(crate) async fn load_receipt(
    node: &std::sync::Arc<EmbeddedNode>,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<SessionMessageReceipt>> {
    let read = crate::tool_call_lifecycle::query::load_tool_call_read(
        &crate::config_client::ConfigAccess::local(node.clone()),
        tool_call_doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    let Some(message) = read.result else {
        return Ok(None);
    };
    let text = crate::tool_call_lifecycle::query::render_tool_result(&message)?;
    Ok(serde_json::from_str::<SessionMessageReceipt>(&text)
        .ok()
        .filter(|receipt| receipt.ok))
}

pub(crate) enum CausedObservation {
    Bound(Box<gents_protocol::row::AgentRequestRow>),
    NotVisible,
    Unbound(&'static str),
}

pub(crate) async fn observe_caused_request(
    node: &std::sync::Arc<EmbeddedNode>,
    lifecycle: &crate::tool_call_lifecycle::ToolCallLifecycle,
) -> Result<CausedObservation> {
    let tool_call_doc_id = lifecycle
        .doc_id()
        .context("session-message row lacks physical identity")?;
    let Some(receipt) = load_receipt(
        node,
        tool_call_doc_id,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    else {
        return Ok(CausedObservation::Unbound(
            "the row has no receipt naming the request it caused",
        ));
    };
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
            _docID request_id node_did requester_did session_id lifecycle_state input
            request_hop caused_by_parent_request_doc_id caused_by_parent_tool_call_id
            caused_by_parent_tool_call_doc_id
        }} }}"#,
        escape_graphql_string(&receipt.request_doc_id),
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "load the request a session-message row caused",
    )
    .await?;
    let Some(caused) = crate::graphql::first_row::<gents_protocol::row::AgentRequestRow>(
        &response,
        "AgentRequest",
    )?
    else {
        return Ok(CausedObservation::NotVisible);
    };
    let caller = lifecycle.node_did();
    let session_message = caused.requester_did.as_deref() == Some(caller)
        && caused.caused_by_parent_tool_call_doc_id.as_deref() == Some(tool_call_doc_id)
        && caused.caused_by_parent_tool_call_id.as_deref() == Some(lifecycle.tool_call_id())
        && caused.caused_by_parent_request_doc_id.as_deref() == lifecycle.request_doc_id();
    if caused.request_id != receipt.request_id
        || caused.session_id.as_deref() != Some(receipt.session_id.as_str())
        || !session_message
    {
        tracing::warn!(
            tool_call_doc_id,
            caused_request_id = %caused.request_id,
            "caused request does not match its session-message receipt"
        );
        return Ok(CausedObservation::Unbound(
            "the receipt names a request that does not carry this call's lineage",
        ));
    }
    Ok(CausedObservation::Bound(Box::new(caused)))
}

pub fn agent_interrupt_allowed(
    caller_session: &str,
    target_session: &str,
    target_origin_cause: Option<&str>,
    same_node: bool,
    task_triggered: bool,
) -> bool {
    target_session != caller_session
        && (target_origin_cause == Some(caller_session) || (same_node && task_triggered))
}

pub async fn interrupt_refusal(
    node: &Arc<EmbeddedNode>,
    caller: &SessionScope,
    target: &SessionScope,
) -> Result<Option<String>> {
    let started = crate::session_origin::verified_agent_new_creation(
        &ConfigAccess::local(node.clone()),
        target,
        caller,
    )
    .await?;
    let cause = started.then_some(caller.session_id.clone());
    let same_node = caller.node_did == target.node_did;
    let task_triggered = if same_node && cause.is_none() {
        crate::session_origin::task_triggered_opening(&ConfigAccess::local(node.clone()), target)
            .await?
    } else {
        false
    };
    Ok((!agent_interrupt_allowed(&caller.session_id, &target.session_id, cause.as_deref(), same_node, task_triggered))
        .then(|| "only the session that started this session or a same-node agent for a Task-triggered session may interrupt it".to_owned()))
}

pub async fn interrupt_session(
    node: &Arc<EmbeddedNode>,
    target: &SessionScope,
) -> Result<Option<String>> {
    if node.node_identity_did() == Some(target.node_did.as_str()) {
        let session_id = target.session_id.clone();
        let node_did = target.node_did.clone();
        let requester_did = target.requester_did.clone();
        return crate::config_client::ConfigAccess::transact_local_write(
            node,
            None,
            "session.interrupt",
            move |txn| {
                let session_id = session_id.clone();
                let node_did = node_did.clone();
                let requester_did = requester_did.clone();
                Box::pin(async move {
                    let active = crate::interrupt::interrupt_active_session_in_txn(
                        txn,
                        &session_id,
                        &node_did,
                        requester_did.as_deref(),
                    )
                    .await?;
                    if active.is_none() {
                        crate::goal::pause_goal_for_interrupt_in_txn(txn, &node_did, &session_id)
                            .await?;
                    }
                    Ok(active)
                })
            },
        )
        .await;
    }

    let Some(active) = crate::interrupt::active_session_request(
        node,
        &target.session_id,
        &target.node_did,
        target.requester_did.as_deref(),
    )
    .await?
    else {
        return Ok(None);
    };
    crate::interrupt::interrupt_request_by_doc_id(
        node,
        active
            .doc_id
            .as_deref()
            .context("active request lacks physical identity")?,
        &target.node_did,
        active.requester_did.as_deref(),
    )
    .await?;
    Ok(Some(active.request_id))
}

pub(crate) async fn agent_list(
    _node: &Arc<EmbeddedNode>,
    _caller: &crate::AgentRequest,
    tools: &AgentToolConfig,
) -> Result<serde_json::Value> {
    let agents = tools
        .targets
        .iter()
        .map(|target| {
            serde_json::json!({
                "agent": target.name,
                "node_did": target.target_node_did,
                "agent_id": target.agent_id,
                "description": target.description,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "ok": true, "agents": agents }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KillOutcome {
    Settled,
    Interrupting { request_id: String },
    Cancelled,
}

/// The caused-request step of a kill: `Some` when this step decides the kill
/// (the terminal request settled the row, or the live local request was
/// interrupted). `None` when the caller must cancel the row directly: a
/// terminal request this step did not settle, or a live remote request
/// (interrupted; a failed peer interrupt is only logged).
pub(crate) async fn kill_caused_request(
    node: &std::sync::Arc<EmbeddedNode>,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
    caused: &gents_protocol::row::AgentRequestRow,
) -> Result<Option<KillOutcome>> {
    let terminal = caused
        .lifecycle_state
        .is_some_and(gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal);
    if terminal && crate::background_completion::settle_session_message_row(node, lifecycle).await?
    {
        return Ok(Some(KillOutcome::Settled));
    }
    let doc_id = caused
        .doc_id
        .as_deref()
        .context("caused request lacks physical identity")?;
    let node_did = caused
        .node_did
        .as_deref()
        .context("caused request lacks node_did")?;
    let local = node_did == lifecycle.node_did();
    if !terminal {
        let interrupted = crate::interrupt::interrupt_request_by_doc_id(
            node,
            doc_id,
            node_did,
            caused.requester_did.as_deref(),
        )
        .await;
        match interrupted {
            Ok(_) if local => {
                return Ok(Some(KillOutcome::Interrupting {
                    request_id: caused.request_id.clone(),
                }))
            }
            Ok(_) => {}
            Err(error) if local => return Err(error),
            Err(error) => tracing::warn!(
                caused_request_doc_id = doc_id,
                error = %format!("{error:#}"),
                "peer interrupt of a killed session message was not written"
            ),
        }
    }
    Ok(None)
}

pub(crate) async fn kill(
    node: &std::sync::Arc<EmbeddedNode>,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
) -> Result<KillOutcome> {
    let caused = match observe_caused_request(node, lifecycle).await? {
        CausedObservation::Bound(caused) => Some(caused),
        CausedObservation::NotVisible | CausedObservation::Unbound(_) => None,
    };
    if let Some(caused) = &caused {
        if let Some(outcome) = kill_caused_request(node, lifecycle, caused).await? {
            return Ok(outcome);
        }
    }
    if lifecycle
        .cancel_during_run_owned(
            crate::tool_call_lifecycle::CancelCause::UserCancelled,
            "explicit_cancel",
        )
        .await?
    {
        let calling_request_id = lifecycle.calling_request_id().await?;
        crate::background_completion::append_background_tool_completion(
            node,
            lifecycle.session_id(),
            &calling_request_id,
            lifecycle
                .doc_id()
                .context("session-message row lacks physical identity")?,
            lifecycle.tool_name(),
            "cancelled",
            "",
            Some("explicit_cancel"),
            crate::lifecycle::RequestHopCause::Continuation,
        )
        .await?;
    }
    Ok(KillOutcome::Cancelled)
}

#[cfg(test)]
mod delivery_conformance {
    use super::*;

    #[test]
    fn agent_delivery_matches_lean() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().agent_delivery_cases;
        assert_eq!(cases.len(), 12);
        for case in cases {
            let mut input = serde_json::json!({
                "session_id": "target", "message": "hello",
                "delivery": case.delivery, "interrupt": case.interrupt
            });
            if case.interrupt.is_none() {
                input.as_object_mut().unwrap().remove("interrupt");
            }
            let args: AgentMessageArgs = serde_json::from_value(input).unwrap();
            let actual = args.delivery().ok().map(|mode| match mode {
                AgentMessageDelivery::Queue => "queue",
                AgentMessageDelivery::Steer => "steer",
                AgentMessageDelivery::Interrupt => "interrupt",
            });
            assert_eq!(actual, case.expected.as_deref(), "{case:?}");
        }
    }
}

#[cfg(test)]
mod hop_tests {
    use super::*;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_session_message, PublishedAdmissionOptions,
    };
    use crate::tool_call_lifecycle::AwaitMode;

    pub(super) fn cause(
        message: &crate::tool_call_lifecycle::admission_fixture::PublishedSessionMessage,
        caller_hop: u32,
    ) -> SessionMessageCause {
        SessionMessageCause {
            caller_node_did: message.admission.node_did.clone(),
            caller_agent_id: "caller-agent".to_owned(),
            caller_session_id: "caller-session".to_owned(),
            caller_requester_did: None,
            caller_request_id: "caller-request".to_owned(),
            caller_request_doc_id: message.admission.tool.request_doc_id().unwrap().to_owned(),
            caller_hop,
            tool_call_id: "hop-tool".to_owned(),
            tool_call_doc_id: "hop-tool-doc".to_owned(),
            correlation: None,
        }
    }

    #[tokio::test]
    async fn generated_session_message_writes_carry_the_calling_edge() {
        let message = published_session_message(PublishedAdmissionOptions {
            name: "session-message-writes".to_owned(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = message.admission.node.clone();
        let caused = crate::request_binding::load_agent_request_by_doc_id(
            &node,
            &message.caused_request_doc_id,
        )
        .await
        .unwrap()
        .unwrap();
        let target = SessionMessageTarget {
            node_did: caused.node_did.clone(),
            agent_id: caused.agent_id.clone(),
            session_id: caused.session_id.clone(),
        };
        let cases = &crate::lean_vocab_test::lean_contract_snapshot()
            .causal_hop_contract
            .write_cases;
        assert!(cases.iter().any(|case| case.delivery == "queue"));
        assert!(cases.iter().any(|case| case.delivery == "steer"));
        for case in cases {
            let name = case.name.as_str();
            assert_eq!(case.own_hop, caused.request_hop, "{name}");
            let state = if case.delivery == "request" {
                "pending"
            } else {
                "processing"
            };
            let response = node
                .execute(&format!(
                    r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&message.caused_request_doc_id)
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            let cause = cause(&message, case.caller_hop);
            let body = RenderedBody {
                content: "again".to_owned(),
                goal: None,
            };
            let planned = plan_with_delivery(
                &node,
                &cause,
                &target,
                body,
                None,
                if case.delivery == "steer" {
                    AgentMessageDelivery::Steer
                } else {
                    AgentMessageDelivery::Queue
                },
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(planned.delivery().as_str(), case.delivery, "{name}");
            assert_eq!(planned.hop(), case.expected_hop, "{name}");
            let PlannedWrite::Request { create, .. } = &planned.write else {
                panic!("{name}: a message without a Goal is a request");
            };
            assert_eq!(create.request_hop, case.expected_hop, "{name}");
            assert_eq!(
                create.caused_by_parent_request_doc_id.as_deref(),
                Some(cause.caller_request_doc_id.as_str()),
                "{name}"
            );
            assert_eq!(
                create.caused_by_parent_tool_call_doc_id.as_deref() == Some("hop-tool-doc"),
                case.names_caller_tool_call,
                "{name}"
            );
            let queued_after = create
                .input
                .queue
                .as_ref()
                .and_then(|queue| queue.queued_after_request_id.as_deref());
            assert_eq!(
                queued_after == Some(caused.request_id.as_str()),
                case.queued_after_active,
                "{name}"
            );
        }
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn stage_planned_write_in_txn_creates_the_request_and_leaves_the_tool_row() {
        let message = published_session_message(PublishedAdmissionOptions {
            name: "session-message-staging".to_owned(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = message.admission.node.clone();
        let tool = &message.admission.tool;
        let tool_doc_id = tool.doc_id().unwrap().to_owned();
        let caused = crate::request_binding::load_agent_request_by_doc_id(
            &node,
            &message.caused_request_doc_id,
        )
        .await
        .unwrap()
        .unwrap();
        let target = SessionMessageTarget {
            node_did: caused.node_did.clone(),
            agent_id: caused.agent_id.clone(),
            session_id: caused.session_id.clone(),
        };
        let cause = cause(&message, 0);
        let body = RenderedBody {
            content: "staged".to_owned(),
            goal: None,
        };
        let planned = plan(&node, &cause, &target, body, None, false)
            .await
            .unwrap()
            .unwrap();
        let load_tool = || {
            crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
                node.clone(),
                &tool_doc_id,
                tool.node_did(),
                tool.session_id(),
                tool.requester_did(),
            )
        };
        let before = load_tool().await.unwrap().unwrap();
        let receipt_before = load_receipt(
            &node,
            &tool_doc_id,
            tool.node_did(),
            tool.session_id(),
            tool.requester_did(),
        )
        .await
        .unwrap();
        let (cause, planned) = (&cause, &planned);
        let enqueued = crate::config_client::ConfigAccess::transact_local_write(
            &node,
            None,
            "session_message.test_stage",
            move |txn| {
                Box::pin(async move { stage_planned_write_in_txn(txn, cause, planned).await })
            },
        )
        .await
        .unwrap();
        let staged = crate::request_binding::load_agent_request_by_doc_id(&node, &enqueued.doc_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(staged.request_id, enqueued.request_id);
        assert_eq!(staged.session_id, target.session_id);
        let after = load_tool().await.unwrap().unwrap();
        assert_eq!(after.state(), before.state());
        assert_eq!(after.is_running(), before.is_running());
        let receipt_after = load_receipt(
            &node,
            &tool_doc_id,
            tool.node_did(),
            tool.session_id(),
            tool.requester_did(),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(&receipt_after).unwrap(),
            serde_json::to_value(&receipt_before).unwrap()
        );
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn stage_planned_write_in_txn_refuses_a_session_that_moved_past_the_planned_hop() {
        let message = published_session_message(PublishedAdmissionOptions {
            name: "session-message-stale-hop".to_owned(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = message.admission.node.clone();
        let caused = crate::request_binding::load_agent_request_by_doc_id(
            &node,
            &message.caused_request_doc_id,
        )
        .await
        .unwrap()
        .unwrap();
        let target = SessionMessageTarget {
            node_did: caused.node_did.clone(),
            agent_id: caused.agent_id.clone(),
            session_id: caused.session_id.clone(),
        };
        let stale_cause = cause(&message, 0);
        let body = RenderedBody {
            content: "stale".to_owned(),
            goal: None,
        };
        let planned = plan(&node, &stale_cause, &target, body, None, false)
            .await
            .unwrap()
            .unwrap();
        let ahead_cause = cause(&message, 10);
        let ahead = plan(
            &node,
            &ahead_cause,
            &target,
            RenderedBody {
                content: "ahead".to_owned(),
                goal: None,
            },
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(ahead.hop() > planned.hop());
        let (ahead_cause, ahead) = (&ahead_cause, &ahead);
        crate::config_client::ConfigAccess::transact_local_write(
            &node,
            None,
            "session_message.test_ahead_stage",
            move |txn| {
                Box::pin(async move { stage_planned_write_in_txn(txn, ahead_cause, ahead).await })
            },
        )
        .await
        .unwrap();
        let (stale_cause, planned) = (&stale_cause, &planned);
        let error = crate::config_client::ConfigAccess::transact_local_write(
            &node,
            None,
            "session_message.test_stale_stage",
            move |txn| {
                Box::pin(async move { stage_planned_write_in_txn(txn, stale_cause, planned).await })
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("moved to hop"),
            "unexpected error: {error:#}"
        );
        let PlannedWrite::Request { create, .. } = &planned.write else {
            panic!("a message without a Goal is a request");
        };
        let response = node
            .execute(&format!(
                r#"query {{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(&create.request_id)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let data = response.data.unwrap();
        assert!(data["AgentRequest"].as_array().unwrap().is_empty());
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }
}

#[cfg(test)]
mod agent_tools_tests {
    use super::*;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_session_message, PublishedAdmissionOptions, PublishedSessionMessage,
    };
    use crate::tool_call_lifecycle::AwaitMode;

    pub(super) async fn started(
        name: &str,
    ) -> (
        PublishedSessionMessage,
        crate::AgentRequest,
        crate::AgentRequest,
    ) {
        let message = published_session_message(PublishedAdmissionOptions {
            name: name.to_owned(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = message.admission.node.clone();
        let caller = crate::request_binding::load_agent_request_by_doc_id(
            &node,
            message.admission.tool.request_doc_id().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        let caused = crate::request_binding::load_agent_request_by_doc_id(
            &node,
            &message.caused_request_doc_id,
        )
        .await
        .unwrap()
        .unwrap();
        materialize_session(&node, &caused).await;
        (message, caller, caused)
    }

    async fn materialize_session(
        node: &std::sync::Arc<EmbeddedNode>,
        caused: &crate::AgentRequest,
    ) {
        materialize_session_at(node, caused, &chrono::Utc::now().to_rfc3339()).await;
    }

    async fn materialize_session_at(
        node: &std::sync::Arc<EmbeddedNode>,
        caused: &crate::AgentRequest,
        created_at: &str,
    ) {
        let caused = caused.clone();
        let created_at = created_at.to_owned();
        ConfigAccess::transact_local_write(node, None, "test.materialize_session", move |txn| {
            let caused = caused.clone();
            let created_at = created_at.clone();
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    &caused.session_id,
                    &caused.node_did,
                    &caused.agent_id,
                    caused.requester_did.as_deref(),
                    None,
                    crate::lifecycle::materialize::created_session_provenance(&caused),
                    &created_at,
                )
                .await
            })
        })
        .await
        .unwrap();
    }

    async fn set_state(node: &EmbeddedNode, doc_id: &str, state: &str) {
        let response = node
            .execute(&format!(
                r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#,
                escape_graphql_string(doc_id)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }

    #[tokio::test]
    async fn steer_reachability_matches_lean() {
        use gents_protocol::request_input::{
            QueueDelivery, QueuePolicy, QueueSource, RequestQueue,
        };
        let (message, _, head) = started("steer-reachability-model").await;
        let cases =
            &crate::lean_vocab_test::lean_contract_snapshot().agent_steer_reachability_cases;
        for case in cases {
            let rows = case
                .inputs
                .iter()
                .map(|item| {
                    let (delivery, source, agent) = match item.kind.as_str() {
                        "queued" => (QueueDelivery::Queue, QueueSource::User, false),
                        "user_steer" => (QueueDelivery::Steer, QueueSource::User, false),
                        "agent_steer" => (QueueDelivery::Steer, QueueSource::Steering, true),
                        "blocked" => (
                            QueueDelivery::Queue,
                            QueueSource::BackgroundCompletion,
                            false,
                        ),
                        other => panic!("unexpected modeled boundary kind: {other}"),
                    };
                    let mut input = head.input.clone();
                    input.cwd = if item.compatible {
                        head.input.cwd.clone()
                    } else {
                        Some("/incompatible".into())
                    };
                    input.queue = Some(RequestQueue {
                        delivery,
                        source,
                        policy: QueuePolicy::Append,
                        position: None,
                        key: None,
                        queued_after_request_id: agent.then(|| head.request_id.clone()),
                        interrupted_request_id: None,
                        background_completion_wake_version: None,
                    });
                    gents_protocol::row::AgentRequestRow {
                        doc_id: Some(item.id.to_string()),
                        request_id: item.id.to_string(),
                        node_did: Some(head.node_did.clone()),
                        session_id: Some(head.session_id.clone()),
                        requester_did: head.requester_did.clone(),
                        agent_id: Some(head.agent_id.clone()),
                        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                        execution_origin: Some("interactive".into()),
                        caused_by_parent_request_doc_id: agent.then(|| "parent".into()),
                        caused_by_parent_tool_call_doc_id: agent.then(|| "tool".into()),
                        input: Some(input),
                        ..Default::default()
                    }
                })
                .collect();
            let actual = crate::lifecycle::queue::steer_reachable(
                &head,
                rows,
                &case.target.to_string(),
                chrono::Utc::now(),
            )
            .is_ok();
            assert_eq!(actual, case.expected, "{case:?}");
        }
        message.admission.node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn interrupt_delivery_latches_only_the_prepublication_head() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().interrupt_delivery_cases;
        for case in cases {
            let (message, caller, caused) = started("interrupt-delivery-atomic").await;
            let node = message.admission.node.clone();
            set_state(
                &node,
                &caused.doc_id,
                if case.planned.is_some() {
                    "processing"
                } else {
                    "completed"
                },
            )
            .await;
            crate::goal::set_goal(
                &node,
                &caused.node_did,
                &caused.session_id,
                Some("interrupt goal"),
                Some(crate::goal::GoalStatus::Active),
                None,
            )
            .await
            .unwrap();
            let cause = super::hop_tests::cause(&message, caller.request_hop);
            let target = SessionMessageTarget {
                node_did: caused.node_did.clone(),
                agent_id: caused.agent_id.clone(),
                session_id: caused.session_id.clone(),
            };
            let plan = plan_with_delivery(
                &node,
                &cause,
                &target,
                RenderedBody {
                    content: "deliver after interruption".to_owned(),
                    goal: None,
                },
                None,
                AgentMessageDelivery::Interrupt,
            )
            .await
            .unwrap()
            .unwrap();
            let replacement = if case.active == Some(2) {
                let replacement_plan = plan_with_delivery(
                    &node,
                    &cause,
                    &target,
                    RenderedBody {
                        content: "replacement active request".to_owned(),
                        goal: None,
                    },
                    None,
                    AgentMessageDelivery::Queue,
                )
                .await
                .unwrap()
                .unwrap();
                let receipt =
                    ConfigAccess::transact_local_write(&node, None, "test.replacement", |txn| {
                        let (cause, plan) = (&cause, &replacement_plan);
                        Box::pin(async move { stage_planned_write_in_txn(txn, cause, plan).await })
                    })
                    .await
                    .unwrap();
                Some(receipt.doc_id)
            } else {
                None
            };
            set_state(
                &node,
                &caused.doc_id,
                if case.active == Some(1) {
                    "processing"
                } else {
                    "completed"
                },
            )
            .await;
            if let Some(doc_id) = &replacement {
                set_state(&node, doc_id, "processing").await;
            }
            let receipt =
                ConfigAccess::transact_local_write(&node, None, "test.interrupt_delivery", |txn| {
                    let (cause, plan) = (&cause, &plan);
                    Box::pin(async move { stage_planned_write_in_txn(txn, cause, plan).await })
                })
                .await
                .unwrap();
            let response = ConfigAccess::local(node.clone()).execute(&format!(
                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID interrupt_requested_at }} }}"#,
                escape_graphql_string(&target.session_id),
            )).await.unwrap();
            let rows = response["data"]["AgentRequest"].as_array().unwrap();
            let old = rows
                .iter()
                .find(|row| row["_docID"] == caused.doc_id)
                .unwrap();
            let new = rows
                .iter()
                .find(|row| row["_docID"] == receipt.doc_id)
                .unwrap();
            let second = replacement
                .as_ref()
                .map(|doc| rows.iter().find(|row| row["_docID"] == *doc).unwrap());
            let actual = if old["interrupt_requested_at"].as_str().is_some() {
                Some(1)
            } else if second.is_some_and(|row| row["interrupt_requested_at"].as_str().is_some()) {
                Some(2)
            } else {
                None
            };
            assert_eq!(actual, case.expected, "{case:?}, rows={rows:?}");
            let goal =
                crate::goal::load_canonical_goal(&node, &caused.node_did, &caused.session_id)
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(
                goal.parsed_status() == Some(crate::goal::GoalStatus::Paused),
                case.goal_paused,
                "{case:?}"
            );
            assert!(
                new["interrupt_requested_at"].is_null(),
                "{case:?}, rows={rows:?}"
            );
            node.shutdown().await;
            std::fs::remove_dir_all(&message.admission.path).unwrap();
        }
    }

    #[tokio::test]
    async fn explicit_steer_checks_the_physical_head_at_publication() {
        let (message, caller, caused) = started("agent-steer-head-change").await;
        let node = message.admission.node.clone();
        set_state(&node, &caused.doc_id, "processing").await;
        let cause = SessionMessageCause {
            caller_node_did: caller.node_did.clone(),
            caller_agent_id: caller.agent_id.clone(),
            caller_session_id: caller.session_id.clone(),
            caller_requester_did: caller.requester_did.clone(),
            caller_request_id: caller.request_id.clone(),
            caller_request_doc_id: caller.doc_id.clone(),
            caller_hop: caller.request_hop,
            tool_call_id: "steer-tool".to_owned(),
            tool_call_doc_id: "steer-tool-doc".to_owned(),
            correlation: None,
        };
        let target = SessionMessageTarget {
            node_did: caused.node_did.clone(),
            agent_id: caused.agent_id.clone(),
            session_id: caused.session_id.clone(),
        };
        let make_plan = || async {
            plan_with_delivery(
                &node,
                &cause,
                &target,
                RenderedBody {
                    content: "change course".to_owned(),
                    goal: None,
                },
                None,
                AgentMessageDelivery::Steer,
            )
            .await
            .unwrap()
            .unwrap()
        };
        let stale = make_plan().await;
        set_state(&node, &caused.doc_id, "completed").await;
        let (cause_ref, stale_ref) = (&cause, &stale);
        let result = ConfigAccess::transact_local_write(
            &node,
            None,
            "test.steer.changed_head",
            move |txn| {
                Box::pin(async move { stage_planned_write_in_txn(txn, cause_ref, stale_ref).await })
            },
        )
        .await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("changed while the steer was planned"));
        set_state(&node, &caused.doc_id, "processing").await;
        let queued = plan_with_delivery(
            &node,
            &cause,
            &target,
            RenderedBody {
                content: "queued turn".to_owned(),
                goal: None,
            },
            None,
            AgentMessageDelivery::Queue,
        )
        .await
        .unwrap()
        .unwrap();
        let (cause_ref, queued_ref) = (&cause, &queued);
        let queued_receipt = ConfigAccess::transact_local_write(
            &node,
            None,
            "test.steer.queued_predecessor",
            move |txn| {
                Box::pin(
                    async move { stage_planned_write_in_txn(txn, cause_ref, queued_ref).await },
                )
            },
        )
        .await
        .unwrap();
        let current = make_plan().await;
        let (cause_ref, current_ref) = (&cause, &current);
        let receipt =
            ConfigAccess::transact_local_write(&node, None, "test.steer.compatible", move |txn| {
                Box::pin(
                    async move { stage_planned_write_in_txn(txn, cause_ref, current_ref).await },
                )
            })
            .await
            .expect("compatible steer is published");
        let row = crate::request_binding::load_agent_request_by_doc_id(&node, &receipt.doc_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.input.queue.unwrap().delivery,
            gents_protocol::request_input::QueueDelivery::Steer
        );
        let (node_did, session_id) = (&caused.node_did, &caused.session_id);
        let snapshot = crate::config_client::ConfigAccess::transact_local_readonly(
            &node,
            None,
            "test.steer.predecessor_order",
            move |txn| {
                Box::pin(async move {
                    crate::lifecycle::queue::pending_rows_in_txn(txn, node_did, session_id).await
                })
            },
        )
        .await
        .unwrap();
        assert!(snapshot
            .iter()
            .any(|row| row.doc_id.as_deref() == Some(queued_receipt.doc_id.as_str())));
        let candidate = snapshot
            .iter()
            .find(|row| row.doc_id.as_deref() == Some(receipt.doc_id.as_str()))
            .unwrap();
        let mut contexts = Vec::new();
        let mut human = caused.clone();
        human.requester_did = Some("did:test:human".to_owned());
        contexts.push(("human requester", human));
        let mut cwd = caused.clone();
        cwd.input.cwd = Some("/different-cwd".to_owned());
        contexts.push(("cwd", cwd));
        let mut skills = caused.clone();
        skills
            .input
            .selected_skill_ids
            .push("different-skill".to_owned());
        contexts.push(("skills", skills));
        let mut workspace = caused.clone();
        workspace.workspace_id = Some("different-workspace".to_owned());
        contexts.push(("workspace", workspace));
        let mut goal = caused.clone();
        goal.input.goal_assignment = Some(gents_protocol::request_input::GoalAssignmentInput {
            goal_id: "different-goal".to_owned(),
            root_request_doc_id: None,
        });
        contexts.push(("goal", goal));
        let modeled = crate::lean_vocab_test::lean_contract_snapshot()
            .agent_steer_disposition_cases
            .iter()
            .find(|case| case.active && !case.compatible)
            .unwrap();
        for case in &crate::lean_vocab_test::lean_contract_snapshot().agent_steer_target_cases {
            let mut head = caused.clone();
            if case["planned_head"] != case["active_head"] {
                head.request_id = "a-later-turn".to_owned();
            }
            assert_eq!(
                crate::lifecycle::queue::steering_compatible(&head, candidate, chrono::Utc::now()),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
        for (name, head) in contexts {
            let disposition = if crate::lifecycle::queue::steering_compatible(
                &head,
                candidate,
                chrono::Utc::now(),
            ) {
                "steer"
            } else {
                "refuse"
            };
            assert_eq!(disposition, modeled.expected, "{name}");
        }
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn only_the_starting_session_may_interrupt() {
        let (message, caller, caused) = started("agent-interrupt-permission").await;
        let node = message.admission.node.clone();
        let caller = SessionScope::of_request(&caller);
        let target = SessionScope::of_request(&caused);
        assert!(interrupt_refusal(&node, &caller, &target)
            .await
            .unwrap()
            .is_none());
        let another = SessionScope {
            session_id: "another-session".to_owned(),
            ..caller.clone()
        };
        assert!(interrupt_refusal(&node, &another, &target)
            .await
            .unwrap()
            .is_some());
        assert!(interrupt_refusal(&node, &target, &caller)
            .await
            .unwrap()
            .is_some());
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn signed_creation_waits_for_its_canonical_parent_evidence() {
        let message = published_session_message(PublishedAdmissionOptions {
            name: "delayed-agent-creation-evidence".to_owned(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = message.admission.node.clone();
        let caller = crate::request_binding::load_agent_request_by_doc_id(
            &node,
            message.admission.tool.request_doc_id().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        let caused = crate::request_binding::load_agent_request_by_doc_id(
            &node,
            &message.caused_request_doc_id,
        )
        .await
        .unwrap()
        .unwrap();
        let access = ConfigAccess::local(node.clone());
        let call_doc = message.admission.tool.doc_id().unwrap();
        let set_name = |name: &str| {
            format!(
            "mutation {{update_AgentToolCall(docID: \"{}\", input: {{tool_name: \"{}\"}}) {{_docID}}}}",
            escape_graphql_string(call_doc), escape_graphql_string(name),
        )
        };
        access
            .write("test.hide_creation_cause", &set_name("agent_message"))
            .await
            .unwrap();
        materialize_session(&node, &caused).await;
        let target = SessionScope::of_request(&caused);
        let source = SessionScope::of_request(&caller);
        let session = crate::session_origin::load_session(
            &access,
            &target.session_id,
            target.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(session.provenance.unwrap().creation_receipt.is_some());
        assert!(interrupt_refusal(&node, &source, &target)
            .await
            .unwrap()
            .is_some());
        access
            .write(
                "test.restore_creation_cause",
                &set_name(crate::toolset::AGENT_NEW_TOOL_NAME),
            )
            .await
            .unwrap();
        assert!(interrupt_refusal(&node, &source, &target)
            .await
            .unwrap()
            .is_none());
        let mut forged = caused.input.clone();
        forged.session_creation =
            Some(gents_protocol::request_input::SessionCreationIntent::PluginJob);
        let mutation = format!("mutation($input: JSON) {{update_AgentRequest(docID: \"{}\", input: {{input: $input}}) {{_docID}}}}", escape_graphql_string(&caused.doc_id));
        let rejected = access
            .transact_write("test.forge_creation_intent", |txn| {
                let mutation = &mutation;
                let forged = &forged;
                Box::pin(async move {
                    txn.execute_with_variables(mutation, &serde_json::json!({"input": forged}))
                        .await
                })
            })
            .await
            .unwrap_err();
        assert!(
            rejected.to_string().contains("immutable field"),
            "{rejected:#}"
        );
        assert!(interrupt_refusal(&node, &source, &target)
            .await
            .unwrap()
            .is_none());
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn remote_signed_creation_survives_parent_evidence_arriving_after_claim() {
        use crate::identity::NodeIdentity;
        use crate::tool_call_lifecycle::admission_fixture::{
            materialize_session_message, published_admission_with_owner, test_p2p_config,
        };
        use defra_p2p_adapter::P2pDocumentRequest;
        use std::time::Duration;

        let (mut source, owner) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "remote-delayed-creation".into(),
            real_identity: true,
            p2p: true,
            await_mode: AwaitMode::Background,
            tool_name: Some(crate::toolset::AGENT_NEW_TOOL_NAME.into()),
            start_running: false,
            ..Default::default()
        })
        .await
        .unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target_identity =
            crate::KeyIdentity::load_or_create(target_dir.path().join("target.key"), None).unwrap();
        let target = Arc::new(
            EmbeddedNode::builder()
                .data_path(target_dir.path())
                .with_node_identity_did(target_identity.did())
                .with_p2p(test_p2p_config())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&target).await.unwrap();
        let receipt = materialize_session_message(
            &source.node,
            &owner,
            &mut source.tool,
            target_identity.did(),
            "remote work",
        )
        .await
        .unwrap();

        async fn address(node: &EmbeddedNode) -> String {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(address) = node
                        .p2p()
                        .unwrap()
                        .listen_addresses()
                        .await
                        .unwrap()
                        .into_iter()
                        .find(|a| a.starts_with("127.0.0.1:") || a.starts_with("[::1]:"))
                    {
                        break address;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .expect("P2P loopback address")
        }
        let source_address = address(&source.node).await;
        let target_address = address(&target).await;
        let mut source_connections = source.node.subscribe(&[events::EventName::PeerConnection]);
        let mut target_connections = target.subscribe(&[events::EventName::PeerConnection]);
        source
            .node
            .p2p()
            .unwrap()
            .connect_peer(&target_address)
            .await
            .unwrap();
        for (node, connections) in [
            (&source.node, &mut source_connections),
            (&target, &mut target_connections),
        ] {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if !node
                        .p2p()
                        .unwrap()
                        .connected_peers()
                        .await
                        .unwrap()
                        .is_empty()
                    {
                        break;
                    }
                    connections
                        .recv()
                        .await
                        .expect("peer connection event stream");
                }
            })
            .await
            .expect("P2P peer connection");
        }
        for (node, peer_address) in [(&source.node, &target_address), (&target, &source_address)] {
            node.p2p()
                .unwrap()
                .add_replicator(
                    vec!["Network".into()],
                    Some(peer_address),
                    Default::default(),
                    Vec::new(),
                    None,
                )
                .await
                .unwrap();
        }
        let peer = target.p2p().unwrap().local_peer_id().await.unwrap();
        async fn push(
            source: &EmbeddedNode,
            target: &Arc<EmbeddedNode>,
            peer: &str,
            collection: &str,
            docs: &[String],
        ) {
            if docs.is_empty() {
                return;
            }
            source
                .p2p()
                .unwrap()
                .push_documents_to_peer(
                    peer,
                    docs.iter()
                        .map(|doc_id| P2pDocumentRequest {
                            collection: collection.into(),
                            doc_id: doc_id.clone(),
                        })
                        .collect(),
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let mut present = true;
                    for doc in docs {
                        let query = format!(
                            "{{{collection}(filter: {{_docID: {{_eq: \"{}\"}}}}) {{_docID}}}}",
                            escape_graphql_string(doc)
                        );
                        let response = ConfigAccess::local(target.clone())
                            .execute(&query)
                            .await
                            .unwrap();
                        present &= response["data"][collection]
                            .as_array()
                            .is_some_and(|rows| rows.len() == 1);
                    }
                    if present {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .expect("exact P2P documents arrive");
        }
        push(
            &source.node,
            &target,
            &peer,
            "AgentRequest",
            std::slice::from_ref(&receipt.request_doc_id),
        )
        .await;
        let query = format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
            escape_graphql_string(&receipt.request_doc_id),
            crate::watcher::AGENT_REQUEST_FIELDS
        );
        let response = ConfigAccess::local(target.clone())
            .execute(&query)
            .await
            .unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        let mut child = crate::lifecycle::RequestLifecycle::new_with_node_did(
            target.clone(),
            "general",
            target_identity.did(),
            row.try_into().unwrap(),
            60,
        );
        assert_eq!(
            child.claim().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        let caller = SessionScope::of_request(owner.request());
        let child_scope = SessionScope::of_request(child.request());
        assert!(ConfigAccess::local(target.clone())
            .execute("{AgentToolCall {_docID}}")
            .await
            .unwrap()["data"]["AgentToolCall"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(interrupt_refusal(&target, &caller, &child_scope)
            .await
            .unwrap()
            .is_some());

        for collection in [
            "AgentRequest",
            "AgentToolCall",
            "AgentMessage",
            "AgentOutputSegment",
        ] {
            let response = ConfigAccess::local(source.node.clone())
                .execute(&format!("{{{collection} {{_docID}}}}"))
                .await
                .unwrap();
            let docs: Vec<String> = response["data"][collection]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|row| row["_docID"].as_str().map(str::to_owned))
                .filter(|doc| doc != &receipt.request_doc_id)
                .collect();
            push(&source.node, &target, &peer, collection, &docs).await;
        }
        assert!(interrupt_refusal(&target, &caller, &child_scope)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            interrupt_session(&target, &child_scope).await.unwrap(),
            Some(child.request().request_id.clone())
        );
        let observed = ConfigAccess::local(target.clone())
            .execute(&format!(
                "{{AgentRequest(docID: \"{}\") {{interrupt_requested_at}}}}",
                escape_graphql_string(&receipt.request_doc_id),
            ))
            .await
            .unwrap();
        assert!(
            observed["data"]["AgentRequest"][0]["interrupt_requested_at"]
                .as_str()
                .is_some()
        );
        target.shutdown().await;
        source.node.shutdown().await;
        std::fs::remove_dir_all(&source.path).unwrap();
    }

    #[tokio::test]
    async fn forged_descriptive_provenance_and_altered_creation_receipt_cannot_grant_control() {
        let (message, caller, caused) = started("agent-interrupt-receipt").await;
        let node = message.admission.node.clone();
        let caller_scope = SessionScope::of_request(&caller);
        let target_scope = SessionScope::of_request(&caused);
        let access = ConfigAccess::local(node.clone());
        let session = crate::session_origin::load_session(
            &access,
            &target_scope.session_id,
            target_scope.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .unwrap();
        let mut forged = session.provenance.unwrap();
        assert!(
            forged.creation_receipt.is_some(),
            "newly materialized session has a receipt"
        );
        forged.creation_receipt.as_mut().unwrap().signature[0] ^= 1;
        let mutation = format!(
            r#"mutation($provenance: JSON) {{update_AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}, input: {{provenance: $provenance}}) {{_docID}}}}"#,
            escape_graphql_string(&target_scope.node_did),
            escape_graphql_string(&target_scope.session_id),
        );
        access
            .transact_write("test.corrupt_session_receipt", |txn| {
                let mutation = &mutation;
                let forged = &forged;
                Box::pin(async move {
                    txn.execute_with_variables(mutation, &serde_json::json!({"provenance": forged}))
                        .await
                })
            })
            .await
            .unwrap();
        assert!(interrupt_refusal(&node, &caller_scope, &target_scope)
            .await
            .unwrap()
            .is_some());

        let human = "unrelated-human-session";
        let provenance = gents_protocol::session::SessionProvenance {
            parent_request_doc_id: Some(caller.doc_id.clone()),
            opening_request_doc_id: Some(caused.doc_id.clone()),
            ..Default::default()
        };
        let created_at = chrono::Utc::now().to_rfc3339();
        access.transact_write("test.forged_session_provenance", |txn| {
            let provenance = &provenance;
            let caller = &caller;
            let created_at = &created_at;
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: AgentSessionMutationInputArg!) {create_AgentSession(input: $input) {_docID}}",
                    &serde_json::json!({"input": {"node_did": caller.node_did, "session_id": human,
                        "requester_did": caller.node_did, "agent_id": caller.agent_id,
                        "created_at": created_at, "provenance": provenance}}),
                ).await
            })
        }).await.unwrap();
        let human_scope = SessionScope {
            session_id: human.into(),
            ..caller_scope.clone()
        };
        assert!(interrupt_refusal(&node, &caller_scope, &human_scope)
            .await
            .unwrap()
            .is_some());
        let lineage = crate::session_origin::lineage(&access, &caller_scope)
            .await
            .unwrap();
        for scope in [&human_scope, &target_scope] {
            assert!(
                !lineage.started.iter().any(|link| &link.scope == scope),
                "{scope:?}: a parent pointer or an altered receipt is not a verified starter"
            );
        }
        assert_eq!(
            crate::session_origin::started_by(&access, &human_scope)
                .await
                .unwrap(),
            None
        );
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn interrupt_stops_the_turn_and_the_steer_is_a_new_request() {
        let (message, caller, caused) = started("agent-interrupt-steer").await;
        let node = message.admission.node.clone();
        let scope = SessionScope::of_request(&caused);
        let target = SessionMessageTarget {
            node_did: caused.node_did.clone(),
            agent_id: caused.agent_id.clone(),
            session_id: caused.session_id.clone(),
        };
        crate::goal::set_goal(
            &node,
            &caused.node_did,
            &caused.session_id,
            Some("pause after operator interrupt"),
            Some(crate::goal::GoalStatus::Active),
            None,
        )
        .await
        .unwrap();
        assert_eq!(interrupt_session(&node, &scope).await.unwrap(), None);
        assert_eq!(
            crate::goal::load_canonical_goal(&node, &caused.node_did, &caused.session_id)
                .await
                .unwrap()
                .unwrap()
                .parsed_status(),
            Some(crate::goal::GoalStatus::Paused),
            "an idle same-node interrupt pauses autonomous Goal work"
        );
        set_state(&node, &message.caused_request_doc_id, "processing").await;

        let cause = SessionMessageCause {
            caller_node_did: caller.node_did.clone(),
            caller_agent_id: "caller-agent".to_owned(),
            caller_session_id: "caller-session".to_owned(),
            caller_requester_did: None,
            caller_request_id: caller.request_id.clone(),
            caller_request_doc_id: caller.doc_id.clone(),
            caller_hop: caller.request_hop,
            tool_call_id: "steer-tool".to_owned(),
            tool_call_doc_id: "steer-tool-doc".to_owned(),
            correlation: None,
        };
        let body = || RenderedBody {
            content: "change course".to_owned(),
            goal: None,
        };
        let steering = plan(&node, &cause, &target, body(), None, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(steering.delivery(), Delivery::Queued);
        let steer = plan(&node, &cause, &target, body(), None, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(steer.delivery(), Delivery::Request);
        assert_eq!(steer.hop(), 1);

        assert_eq!(
            interrupt_session(&node, &scope).await.unwrap(),
            Some(caused.request_id.clone())
        );
        assert!(crate::interrupt::fetch_interrupt_requested_at_by_doc_id(
            &node,
            &message.caused_request_doc_id
        )
        .await
        .unwrap()
        .is_some());
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn explicit_stop_drains_pending_work_and_rejects_late_admission() {
        let (message, caller, caused) = started("agent-stop-drains").await;
        let node = message.admission.node.clone();
        crate::goal::set_goal(
            &node,
            &caused.node_did,
            &caused.session_id,
            Some("stop this session"),
            Some(crate::goal::GoalStatus::Active),
            None,
        )
        .await
        .unwrap();

        let receipt = crate::session::stop_session(
            &node,
            &caused.node_did,
            &caused.session_id,
            caused.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(receipt.status, "stop_requested");
        assert_eq!(receipt.drained_request_ids, vec![caused.request_id.clone()]);
        assert!(crate::session::session_is_stopped(
            &node,
            &caused.node_did,
            &caused.session_id,
            caused.requester_did.as_deref(),
        )
        .await
        .unwrap());
        assert_eq!(
            crate::goal::load_canonical_goal(&node, &caused.node_did, &caused.session_id)
                .await
                .unwrap()
                .unwrap()
                .parsed_status(),
            Some(crate::goal::GoalStatus::Paused)
        );

        let error = plan(
            &node,
            &SessionMessageCause {
                caller_node_did: caller.node_did.clone(),
                caller_agent_id: "caller-agent".to_owned(),
                caller_session_id: "caller-session".to_owned(),
                caller_requester_did: None,
                caller_request_id: caller.request_id.clone(),
                caller_request_doc_id: caller.doc_id.clone(),
                caller_hop: caller.request_hop,
                tool_call_id: "late-message".to_owned(),
                tool_call_doc_id: "late-message-doc".to_owned(),
                correlation: None,
            },
            &SessionMessageTarget {
                node_did: caused.node_did.clone(),
                agent_id: caused.agent_id.clone(),
                session_id: caused.session_id.clone(),
            },
            RenderedBody {
                content: "too late".to_owned(),
                goal: None,
            },
            None,
            false,
        )
        .await
        .unwrap();
        assert!(
            error.is_err(),
            "late message admission must honor durable stop"
        );

        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn agent_lists_separate_targets_from_messageable_sessions() {
        let (message, caller, caused) = started("agent-list").await;
        let node = message.admission.node.clone();
        let tools = AgentToolConfig {
            enabled: true,
            targets: vec![crate::document_config::AgentTargetDocument {
                target_id: "worker".to_owned(),
                node_did: caller.node_did.clone(),
                target_node_did: caused.node_did.clone(),
                agent_id: caused.agent_id.clone(),
                name: "worker".to_owned(),
                description: Some("does the work".to_owned()),
                tags: Vec::new(),
            }],
        };
        let listed = agent_list(&node, &caller, &tools).await.unwrap();
        assert_eq!(listed["agents"][0]["agent"], "worker");
        assert_eq!(listed["agents"][0]["agent_id"], caused.agent_id);
        assert!(listed.get("sessions").is_none());
        let listed = agent_session_list(&node, &caller, &tools, &AgentSessionListArgs::default())
            .await
            .unwrap();
        let sessions = listed["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1, "{listed}");
        assert_eq!(sessions[0]["session_id"], caused.session_id);
        assert_eq!(sessions[0]["relationship"], "started_by_you");
        assert_eq!(sessions[0]["can_message"], true);
        assert_eq!(sessions[0]["can_interrupt"], true);
        assert_eq!(sessions[0]["status"], "idle");

        let from_child =
            agent_session_list(&node, &caused, &tools, &AgentSessionListArgs::default())
                .await
                .unwrap();
        let sessions = from_child["sessions"].as_array().unwrap();
        for session in sessions {
            assert!(resolve_send_target(
                &node,
                &caused.node_did,
                &tools,
                session["session_id"].as_str().unwrap()
            )
            .await
            .unwrap()
            .is_some());
        }
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[test]
    fn session_target_authority_matches_lean_owner() {
        use gents_protocol::session::{AgentSession, SessionProvenance};
        let cases = &crate::lean_vocab_test::lean_causal_hop_contract().session_target_cases;
        assert_eq!(cases.len(), 16);
        for case in cases {
            let session = AgentSession {
                session_id: "worker-session".to_owned(),
                node_did: if case.same_node { "caller" } else { "peer" }.to_owned(),
                requester_did: Some(
                    if case.same_requester {
                        "caller"
                    } else {
                        "other"
                    }
                    .to_owned(),
                ),
                agent_id: "worker".to_owned(),
                created_at: "2026-10-14T00:00:00Z".to_owned(),
                closed_at: None,
                archived_at: None,
                archive_revision: 0,
                stopped_at: None,
                title: None,
                tags: Vec::new(),
                observation: None,
                provenance: case.caused.then(|| SessionProvenance {
                    parent_request_doc_id: Some("parent".to_owned()),
                    ..Default::default()
                }),
            };
            let tools = AgentToolConfig {
                enabled: true,
                targets: if case.allowed_target {
                    vec![crate::document_config::AgentTargetDocument {
                        target_id: "worker".to_owned(),
                        node_did: "caller".to_owned(),
                        target_node_did: session.node_did.clone(),
                        agent_id: "worker".to_owned(),
                        name: "worker".to_owned(),
                        description: None,
                        tags: Vec::new(),
                    }]
                } else {
                    Vec::new()
                },
            };
            assert_eq!(
                session_target_allowed(&session, "caller", &tools),
                case.expected_allowed,
                "{case:?}"
            );
        }
    }

    #[tokio::test]
    async fn session_listing_pages_by_logical_labels_and_compares_timestamp_instants() {
        let (message, caller, caused) = started("session-list-order-and-time").await;
        let node = message.admission.node.clone();
        for label in ["a-worker", "z-worker", "m-worker"] {
            let mut candidate = caused.clone();
            candidate.session_id = label.to_owned();
            candidate.caused_by_parent_request_doc_id = None;
            materialize_session_at(&node, &candidate, "2026-10-13T00:00:00Z").await;
        }
        let tools = AgentToolConfig {
            enabled: true,
            targets: Vec::new(),
        };
        let mut args: AgentSessionListArgs = serde_json::from_value(serde_json::json!({
            "limit": 1, "filter": {"relationship": "same_node"}
        }))
        .unwrap();
        let mut ordered = Vec::new();
        loop {
            let page = agent_session_list(&node, &caller, &tools, &args)
                .await
                .unwrap();
            ordered.extend(
                page["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["session_id"].as_str().unwrap().to_owned()),
            );
            args.cursor = page["next_cursor"].as_str().map(str::to_owned);
            if args.cursor.is_none() {
                break;
            }
        }
        assert_eq!(ordered, ["z-worker", "m-worker", "a-worker"]);
        for field in ["created_after", "updated_after"] {
            for (time, included) in [
                ("2026-10-12T23:59:59Z", true),
                ("2026-10-13T00:59:59+01:00", true),
                ("2026-10-13T00:00:00Z", false),
                ("2026-10-13T00:00:00+00:00", false),
                ("2026-10-13T00:00:00.001Z", false),
                ("2026-10-12T19:00:01-05:00", false),
            ] {
                let mut filter = serde_json::json!({"relationship": "same_node"});
                filter[field] = serde_json::json!(time);
                let args = serde_json::from_value(serde_json::json!({"filter": filter})).unwrap();
                let page = agent_session_list(&node, &caller, &tools, &args)
                    .await
                    .unwrap();
                let ids = page["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["session_id"].as_str().unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(
                    ids,
                    if included {
                        vec!["z-worker", "m-worker", "a-worker"]
                    } else {
                        Vec::new()
                    },
                    "{field}: {time}"
                );
            }
        }
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    #[tokio::test]
    async fn session_listing_discovers_unlinked_work_and_matches_send_authority() {
        let (message, mut caller_owner) =
            crate::tool_call_lifecycle::admission_fixture::published_session_message_with_owner(
                PublishedAdmissionOptions {
                    name: "agent-session-discovery".to_owned(),
                    real_identity: true,
                    await_mode: AwaitMode::Background,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let caller = caller_owner.request().clone();
        let node = message.admission.node.clone();
        let caused = crate::request_binding::load_agent_request_by_doc_id(
            &node,
            &message.caused_request_doc_id,
        )
        .await
        .unwrap()
        .unwrap();
        materialize_session(&node, &caused).await;
        let tools = AgentToolConfig {
            enabled: true,
            targets: Vec::new(),
        };
        let mut candidates = Vec::new();
        for (name, owner, requester, parent) in [
            (
                "trigger-worker",
                caller.node_did.as_str(),
                caller.node_did.as_str(),
                None,
            ),
            ("other-requester", caller.node_did.as_str(), "other", None),
            ("remote-root", "peer", caller.node_did.as_str(), None),
            (
                "remote-started",
                "peer",
                caller.node_did.as_str(),
                Some(caller.doc_id.clone()),
            ),
        ] {
            let mut candidate = caused.clone();
            candidate.session_id = name.to_owned();
            candidate.node_did = owner.to_owned();
            candidate.requester_did = Some(requester.to_owned());
            candidate.caused_by_parent_request_doc_id = parent;
            materialize_session(&node, &candidate).await;
            candidates.push(candidate.session_id);
        }
        candidates.push(caused.session_id.clone());
        let mut args = AgentSessionListArgs {
            limit: Some(1),
            ..Default::default()
        };
        let mut listed = std::collections::BTreeSet::new();
        loop {
            let page = agent_session_list(&node, &caller, &tools, &args)
                .await
                .unwrap();
            for row in page["sessions"].as_array().unwrap() {
                assert_eq!(row["can_message"], true);
                assert!(
                    listed.insert(row["session_id"].as_str().unwrap().to_owned()),
                    "duplicate paged session"
                );
            }
            args.cursor = page["next_cursor"].as_str().map(ToOwned::to_owned);
            if args.cursor.is_none() {
                break;
            }
        }
        assert!(listed.contains("trigger-worker"));
        for id in candidates {
            assert_eq!(
                listed.contains(&id),
                resolve_send_target(&node, &caller.node_did, &tools, &id)
                    .await
                    .unwrap()
                    .is_some(),
                "{id}"
            );
        }
        let all = agent_session_list(&node, &caller, &tools, &AgentSessionListArgs::default())
            .await
            .unwrap();
        let stale = agent_session_list(
            &node,
            &caller,
            &tools,
            &AgentSessionListArgs {
                cursor: Some("stale cursor".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(all, stale);
        let identity =
            crate::KeyIdentity::load_or_create(message.admission.path.join("test-agent.key"), None)
                .unwrap();
        let mut worker = crate::tool_call_lifecycle::admission_fixture::claimed_signed_request_with_trigger_correlation(
            &node, "trigger-worker-request", "correlated-worker", &identity, None,
            Some("shard-assignment"), Some("R3a"),
        ).await;
        let filtered = agent_session_list(&node, &caller, &tools, &serde_json::from_value(serde_json::json!({"filter": {"caused_by_trigger_id": "shard-assignment", "caused_by_correlation": "R3a", "status": "busy"}})).unwrap()).await.unwrap();
        assert_eq!(filtered["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["sessions"][0]["session_id"], "correlated-worker");
        assert_eq!(filtered["sessions"][0]["relationship"], "same_node");
        assert_eq!(
            filtered["sessions"][0]["request_id"],
            worker.request().request_id
        );
        assert_eq!(filtered["sessions"][0]["lifecycle_state"], "claimed");
        assert_eq!(filtered["sessions"][0]["can_interrupt"], false);
        let target = resolve_send_target(&node, &caller.node_did, &tools, "correlated-worker")
            .await
            .unwrap()
            .unwrap();
        let mut send_call = crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            node.clone(), &mut caller_owner, &caller.node_did, 1, crate::toolset::AGENT_MESSAGE_TOOL_NAME, "message-trigger-worker",
            serde_json::json!({"session_id": "correlated-worker", "message": "continue shard R3a"}), AwaitMode::Background, false,
        ).await.unwrap();
        let cause = crate::lifecycle::SessionMessageCause {
            caller_node_did: caller.node_did.clone(),
            caller_agent_id: "caller-agent".to_owned(),
            caller_session_id: "caller-session".to_owned(),
            caller_requester_did: None,
            caller_request_id: caller.request_id.clone(),
            caller_request_doc_id: caller.doc_id.clone(),
            caller_hop: caller.request_hop,
            tool_call_id: send_call.tool_call_id().to_owned(),
            tool_call_doc_id: send_call.doc_id().unwrap().to_owned(),
            correlation: None,
        };
        let message_plan = plan(
            &node,
            &cause,
            &target,
            RenderedBody {
                content: "continue shard R3a".to_owned(),
                goal: None,
            },
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(message_plan.delivery(), Delivery::Queued);
        let delivered = commit(&node, &cause, &mut send_call, message_plan, true)
            .await
            .unwrap();
        assert_eq!(delivered.session_id, "correlated-worker");
        assert_eq!(delivered.delivery.as_deref(), Some("queue"));
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            &caller.node_did,
            std::time::Duration::ZERO,
        );
        worker.begin_owned_execution(&writer).await.unwrap();
        worker
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Interrupted,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();
        drop(caller_owner);
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    /// A caused session records its opening request when it is created and
    /// keeps it when a later request reaches the session.
    #[tokio::test]
    async fn caused_session_records_its_opening_request() {
        let (message, caller, caused) = started("opening-provenance").await;
        let node = message.admission.node.clone();
        let access = ConfigAccess::local(node.clone());
        let signed = access
            .execute(&format!(
                "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
                escape_graphql_string(&caused.doc_id),
                crate::request_admission::SIGNED_REQUEST_FIELDS,
            ))
            .await
            .unwrap();
        let mut signed: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(signed["data"]["AgentRequest"][0].clone()).unwrap();
        crate::request_admission::verify_request_receipt_signature(&signed).unwrap();
        signed
            .input
            .as_mut()
            .unwrap()
            .started_by
            .as_mut()
            .unwrap()
            .session_id = "forged".into();
        assert!(crate::request_admission::verify_request_receipt_signature(&signed).is_err());
        let provenance = || async {
            crate::session_origin::load_session(
                &access,
                &caused.session_id,
                caused.requester_did.as_deref(),
            )
            .await
            .unwrap()
            .unwrap()
            .provenance
            .unwrap()
        };
        let recorded = provenance().await;
        assert_eq!(
            crate::session_origin::SessionScope::from(recorded.started_by.as_ref().unwrap()),
            crate::session_origin::SessionScope::of_request(&caller)
        );
        let caller_scope = crate::session_origin::SessionScope::of_request(&caller);
        let started = crate::session_origin::lineage(&access, &caller_scope)
            .await
            .unwrap()
            .started;
        assert!(started
            .iter()
            .any(|link| link.scope == crate::session_origin::SessionScope::of_request(&caused)));
        assert_eq!(
            recorded.parent_request_doc_id,
            caused.caused_by_parent_request_doc_id
        );
        assert_eq!(
            recorded.opening_request_doc_id.as_deref(),
            Some(caused.doc_id.as_str())
        );

        let mut later = caused.clone();
        later.doc_id = "bae-later-message".to_owned();
        materialize_session(&node, &later).await;
        assert_eq!(provenance().await, recorded);

        let mut without_call = caused.clone();
        without_call.caused_by_parent_tool_call_doc_id = None;
        assert_eq!(
            crate::lifecycle::materialize::created_session_provenance(&without_call)
                .unwrap()
                .opening_request_doc_id,
            None
        );
        without_call.caused_by_parent_request_doc_id = None;
        assert!(crate::lifecycle::materialize::created_session_provenance(&without_call).is_none());
        without_call.caused_by_trigger_id = Some("task-trigger".to_owned());
        without_call.caused_by_trigger_kind = Some("event".to_owned());
        assert_eq!(
            crate::lifecycle::materialize::created_session_provenance(&without_call)
                .unwrap()
                .opening_request_doc_id
                .as_deref(),
            Some(without_call.doc_id.as_str())
        );
        materialize_session(&node, &without_call).await;
        assert_eq!(provenance().await, recorded);
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }
}
