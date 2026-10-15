//! The agents tools. `agent_new` and `agent_message` start or continue
//! another agent's session; both materialize one request through the
//! session-message writer in `lifecycle::materialize`, which records the
//! calling edge and the causal hop. Every agent is an ordinary agent addressed
//! directly: the started session runs under its own behavior, and its result
//! reaches the caller only as a background completion message.

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use std::sync::Arc;

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::lifecycle::{SessionMessageCause, SessionMessageTarget};
use crate::session_origin::SessionScope;
use crate::tool_surface::AgentToolConfig;

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
    /// Request interruption of the current turn before queueing the message.
    #[serde(default)]
    pub interrupt: bool,
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
}

/// How an `agent_message` reached its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// An idle session received a new request.
    Request,
    /// A busy session received an agent-authored steering continuation
    /// queued after its active request.
    Steering,
}

impl Delivery {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Steering => "steering",
        }
    }
}

/// A message body rendered for its target: the content, and the Goal a Task
/// declares, if any.
pub(crate) struct RenderedBody {
    pub content: String,
    pub goal: Option<(String, Option<i64>)>,
}

/// The caller's `Tools.agents` group and its resolved targets, read through
/// the request admission configuration walk. Absent tools leave it disabled.
pub(crate) async fn load_caller_session_tools(
    node: &EmbeddedNode,
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

/// Render a message body. A Task is the caller's own configuration: its
/// prompt template is rendered with `task.input` as `args`, and its Goal
/// declaration, if any, is rendered for the target session.
pub(crate) async fn render_body(
    node: &EmbeddedNode,
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
    let loaded = crate::config_client::ConfigAccess::transact_local(
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
    let Some(loaded) = loaded else {
        return Ok(Err(format!("task '{task_id}' is not configured")));
    };
    let task_document: crate::document_config::Task =
        serde_json::from_value(loaded).context("decode configured Task")?;
    if task_document.emit_outcome {
        return Ok(Err("emit_outcome Tasks require document-triggered delivery or explicit CLI/desktop Task admission; agent_new/agent_message Task bodies do not emit outcomes".into()));
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (node_scope, ctx_scope) =
        crate::template::task_node_ctx(caller_node_did, target_agent_id, &now);
    let scope = crate::template::TemplateScope {
        session: Some(serde_json::json!({"session_id": target_session_id})),
        request: None,
        event: serde_json::Value::Null,
        doc: None,
        args: Some(task.input.clone().unwrap_or(serde_json::json!({}))),
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
        Err(error) => return Ok(Err(format!("task {error}"))),
    };
    let goal = objective.map(|objective| (objective, task_document.goal_token_budget));
    Ok(Ok(RenderedBody { content, goal }))
}

/// Resolve the session `agent_message` addresses. The caller may address a
/// session in its own requester scope: one of its own agent's sessions, or a
/// session an `agent_new` started on a target that is still in its allowlist.
/// ACP decides whether the resulting write is accepted.
pub(crate) async fn resolve_send_target(
    node: &Arc<EmbeddedNode>,
    caller_node_did: &str,
    tools: &AgentToolConfig,
    session_id: &str,
) -> Result<Option<SessionMessageTarget>> {
    let Some(session) = crate::session_origin::load_session(
        &ConfigAccess::Local(node.clone()),
        session_id,
        Some(caller_node_did),
    )
    .await?
    else {
        return Ok(None);
    };
    let own = session.node_did == caller_node_did;
    let started = session
        .provenance
        .as_ref()
        .is_some_and(|provenance| provenance.parent_request_doc_id.is_some())
        && tools.allows(&session.node_did, &session.agent_id);
    Ok((own || started).then(|| SessionMessageTarget {
        node_did: session.node_did,
        agent_id: session.agent_id,
        session_id: session.session_id,
    }))
}

/// The scope of a session [`resolve_send_target`] resolved: its requester is
/// always the calling principal.
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

/// A session-message request and how it will be delivered, decided before
/// the tool row starts running.
pub(crate) struct Plan {
    session_id: String,
    hop: u32,
    write: PlannedWrite,
}

impl Plan {
    /// The causal hop the planned request or continuation is written with.
    pub(crate) fn hop(&self) -> u32 {
        self.hop
    }

    pub(crate) fn delivery(&self) -> Delivery {
        match self.write {
            PlannedWrite::Request { delivery, .. } => delivery,
            PlannedWrite::Goal { .. } => Delivery::Request,
        }
    }
}

/// Plan one session-message request. An idle or remote session gets a new
/// request; a busy local session gets a steering append queued after its
/// active request, unless the caller interrupts that request first, in which
/// case the message is a new request. A Task Goal is set on a local idle target session with its
/// request in one transaction.
pub(crate) async fn plan(
    node: &EmbeddedNode,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    rendered: RenderedBody,
    title: Option<&str>,
    interrupt: bool,
) -> Result<Result<Plan, String>> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let local = target.node_did == cause.caller_node_did;
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
    // Lean `CausalHop.nextHop` of a cross-session cause: past the caller, and
    // never below the addressed session's current hop.
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
        )
        .await?;
        PlannedWrite::Goal {
            create,
            objective,
            token_budget,
        }
    } else {
        // A busy session is steered: the message is queued after its active
        // request, which orders the queue and is not the message's origin.
        let steering = active.filter(|_| !interrupt);
        let queue = steering
            .as_ref()
            .map(|active| gents_protocol::request_input::RequestQueue {
                delivery: Default::default(),
                position: None,
                source: gents_protocol::request_input::QueueSource::Steering,
                policy: gents_protocol::request_input::QueuePolicy::Append,
                key: None,
                queued_after_request_id: Some(active.request_id.clone()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
        PlannedWrite::Request {
            create: crate::lifecycle::build_session_message_request(
                cause,
                target,
                &rendered.content,
                title,
                &request_id,
                None,
                hop,
                queue,
            )
            .await?,
            delivery: if steering.is_some() {
                Delivery::Steering
            } else {
                Delivery::Request
            },
        }
    };
    Ok(Ok(Plan {
        session_id: target.session_id.clone(),
        hop,
        write,
    }))
}

/// The receipt a session-message row answers its invocation with. It names
/// the exact request the call caused; settlement and cancellation read that
/// request's document from here.
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

/// Start the pending row, persist its planned request and publish the
/// receipt naming that request, all in one transaction: a running
/// session-message row always has a receipt and a caused request.
///
/// An embedded commit can be durable although its result was lost, so a
/// failed commit is decided by the durable row: a running row with its
/// receipt was delivered, and a pending row was not. Any other observation is
/// an error, and the row is left to recovery.
pub(crate) async fn commit(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
    plan: Plan,
    report_delivery: bool,
) -> Result<SessionMessageReceipt> {
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
    node: &EmbeddedNode,
    cause: &SessionMessageCause,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
    plan: Plan,
    report_delivery: bool,
) -> Result<SessionMessageReceipt> {
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
    let start = &start;
    let session_id = plan.session_id;
    let (receipt, started_at) = match plan.write {
        PlannedWrite::Request { create, .. } => {
            let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
            let (mutation, binding, create, receipt_for) =
                (&mutation, &binding, &create, &receipt_for);
            crate::config_client::ConfigAccess::transact_local(
                node,
                None,
                "session_message.commit_request",
                move |txn| {
                    Box::pin(async move {
                        ensure_planned_hop_current(txn, create).await?;
                        let started_at = start_dispatch(txn, start).await?;
                        let response = txn.execute(mutation).await?;
                        let doc_id = crate::graphql::created_doc_id(&response, "AgentRequest")?;
                        let receipt = receipt_for(&crate::lifecycle::EnqueuedAgentRequest {
                            doc_id,
                            request_id: create.request_id.clone(),
                            session_id: create.session_id.clone(),
                        });
                        crate::tool_call_lifecycle::publish_background_receipt_in_txn(
                            txn,
                            binding,
                            &serde_json::to_string(&receipt)?,
                        )
                        .await?;
                        Ok((receipt, started_at))
                    })
                },
            )
            .await
        }
        PlannedWrite::Goal {
            create,
            objective,
            token_budget,
        } => {
            let actor = ::identity::Did::new(cause.caller_node_did.clone())
                .context("caller DID is not ACP-addressable")?;
            let (create, objective, session_id, binding, receipt_for) =
                (&create, &objective, &session_id, &binding, &receipt_for);
            crate::config_client::ConfigAccess::transact_local(
                node,
                Some(actor),
                "session_message.commit_goal",
                move |txn| {
                    Box::pin(async move {
                        ensure_planned_hop_current(txn, create).await?;
                        let started_at = start_dispatch(txn, start).await?;
                        let enqueued = crate::goal::stage_goal_backed_request_in_txn(
                            txn,
                            &create.node_did,
                            session_id,
                            objective,
                            token_budget,
                            create,
                        )
                        .await?;
                        let receipt = receipt_for(&enqueued);
                        crate::tool_call_lifecycle::publish_background_receipt_in_txn(
                            txn,
                            binding,
                            &serde_json::to_string(&receipt)?,
                        )
                        .await?;
                        Ok((receipt, started_at))
                    })
                },
            )
            .await
        }
    }?;
    lifecycle.mark_started(started_at);
    Ok(receipt)
}

/// The hop was planned from the target session's current hop before the
/// transaction; a higher request committed since then would leave the message
/// below that session's hop, so the commit refuses it instead.
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

/// The receipt a session-message row published: `Ok(None)` when the row has
/// no invocation reply, or its reply is not a session-message receipt.
pub(crate) async fn load_receipt(
    node: &std::sync::Arc<EmbeddedNode>,
    tool_call_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<SessionMessageReceipt>> {
    let read = crate::tool_call_lifecycle::query::load_tool_call_read(
        &crate::config_client::ConfigAccess::Local(node.clone()),
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

/// What a session-message row can observe of the one request it caused.
pub(crate) enum CausedObservation {
    /// The receipt names a request carrying the row's lineage.
    Bound(gents_protocol::row::AgentRequestRow),
    /// The named request is not visible here yet: a peer has not replicated
    /// it back (Lean premise on `SessionMessageRecoveryCause`).
    NotVisible,
    /// Lean `SessionMessageRecoveryCause.causedRequestUnbound`: the receipt
    /// is missing, or names a request that fails the row's lineage.
    Unbound(&'static str),
}

/// The one request a session-message row caused, read through the row's
/// receipt and checked against its lineage: a session-message request, new or
/// steering, carrying the row's full calling edge under this requester.
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
    Ok(CausedObservation::Bound(caused))
}

/// Lean `DurableLineage.interruptAllowed`: in 0.20 an agent may interrupt
/// another session only when the caller's session started it.
pub fn agent_interrupt_allowed(
    caller_session: &str,
    target_session: &str,
    target_origin_cause: Option<&str>,
) -> bool {
    target_session != caller_session && target_origin_cause == Some(caller_session)
}

/// Refusal reason when the calling session may not interrupt `target`
/// (Lean `DurableLineage.interruptAllowed`, through the stored provenance).
pub async fn interrupt_refusal(
    node: &Arc<EmbeddedNode>,
    caller: &SessionScope,
    target: &SessionScope,
) -> Result<Option<String>> {
    let started_by =
        crate::session_origin::started_by(&ConfigAccess::Local(node.clone()), target).await?;
    let cause = started_by
        .filter(|link| link.scope == *caller)
        .map(|link| link.scope.session_id);
    Ok(
        (!agent_interrupt_allowed(&caller.session_id, &target.session_id, cause.as_deref()))
            .then(|| "only the session that started this session may interrupt it".to_owned()),
    )
}

/// Stop `target`'s current turn through the single-session interrupt owner.
/// Returns the interrupted request id, or `None` when the session is idle.
pub async fn interrupt_session(
    node: &EmbeddedNode,
    target: &SessionScope,
) -> Result<Option<String>> {
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

/// A session `agent_list` reports and how the calling session relates to it.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub(crate) struct ReachableSession {
    pub session_id: String,
    pub node_did: String,
    pub relationship: &'static str,
    pub status: String,
    pub can_message: bool,
    pub can_interrupt: bool,
}

/// `agent_list`: the agents the caller may start and the sessions linked to
/// the calling session (`gents::session_origin::lineage`).
pub(crate) async fn agent_list(
    node: &Arc<EmbeddedNode>,
    caller: &crate::AgentRequest,
    tools: &AgentToolConfig,
) -> Result<serde_json::Value> {
    let own = SessionScope::of_request(caller);
    let lineage = crate::session_origin::lineage(&ConfigAccess::Local(node.clone()), &own).await?;
    let linked = lineage
        .started_by
        .into_iter()
        .map(|link| (link, "started_you"))
        .chain(
            lineage
                .started
                .into_iter()
                .map(|link| (link, "started_by_you")),
        )
        .chain(lineage.sent.into_iter().map(|link| (link, "messaged")))
        .chain(
            lineage
                .received
                .into_iter()
                .map(|link| (link, "messaged_you")),
        );
    let mut sessions = Vec::new();
    for (link, relationship) in linked {
        let scope = link.scope;
        if sessions
            .iter()
            .any(|entry: &ReachableSession| entry.session_id == scope.session_id)
        {
            continue;
        }
        let can_message = scope.requester_did.as_deref() == Some(caller.node_did.as_str())
            && resolve_send_target(node, &caller.node_did, tools, &scope.session_id)
                .await?
                .is_some();
        let active = crate::interrupt::active_session_request(
            node,
            &scope.session_id,
            &scope.node_did,
            scope.requester_did.as_deref(),
        )
        .await?;
        sessions.push(ReachableSession {
            session_id: scope.session_id,
            node_did: scope.node_did,
            relationship,
            status: if active.is_some() { "busy" } else { "idle" }.to_owned(),
            can_message,
            can_interrupt: relationship == "started_by_you",
        });
    }
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
    Ok(serde_json::json!({ "ok": true, "agents": agents, "sessions": sessions }))
}

/// What a kill did (Lean `Recovery.KillAction`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KillOutcome {
    /// The caused request had already ended; the row settled from it.
    Settled,
    /// The local caused request was interrupted; its terminal settles the row.
    Interrupting { request_id: String },
    /// The row was cancelled now, with a cancelled completion notification.
    Cancelled,
}

/// `cancel_process` and the operator kill on a running session-message row
/// (Lean `Recovery.killAction`). A kill never waits on a peer or on a request
/// it cannot name: those rows are cancelled directly.
pub(crate) async fn kill(
    node: &std::sync::Arc<EmbeddedNode>,
    lifecycle: &mut crate::tool_call_lifecycle::ToolCallLifecycle,
) -> Result<KillOutcome> {
    let caused = match observe_caused_request(node, lifecycle).await? {
        CausedObservation::Bound(caused) => Some(caused),
        CausedObservation::NotVisible | CausedObservation::Unbound(_) => None,
    };
    if let Some(caused) = &caused {
        let terminal = caused
            .lifecycle_state
            .is_some_and(gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal);
        if terminal
            && crate::background_completion::settle_session_message_row(node, lifecycle).await?
        {
            return Ok(KillOutcome::Settled);
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
                    return Ok(KillOutcome::Interrupting {
                        request_id: caused.request_id.clone(),
                    })
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
    }
    if lifecycle
        .cancel_during_run_owned(
            crate::tool_call_lifecycle::CancelCause::UserCancelled,
            "explicit_cancel",
        )
        .await?
    {
        let calling_request_id = calling_request_id(node, lifecycle).await?;
        crate::background_completion::append_background_tool_completion(
            node.as_ref(),
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

/// The logical id of the request that made a session-message call, read from
/// its physical binding when the row was not loaded with it.
pub(crate) async fn calling_request_id(
    node: &EmbeddedNode,
    lifecycle: &crate::tool_call_lifecycle::ToolCallLifecycle,
) -> Result<String> {
    if !lifecycle.request_id().trim().is_empty() {
        return Ok(lifecycle.request_id().to_owned());
    }
    let doc_id = lifecycle
        .request_doc_id()
        .context("session-message row lacks its calling request document")?;
    Ok(
        crate::request_binding::load_agent_request_by_doc_id(node, doc_id)
            .await?
            .context("session-message calling request disappeared")?
            .request_id,
    )
}

#[cfg(test)]
mod hop_tests {
    use super::*;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_session_message, PublishedAdmissionOptions,
    };
    use crate::tool_call_lifecycle::AwaitMode;

    fn cause(
        message: &crate::tool_call_lifecycle::admission_fixture::PublishedSessionMessage,
        caller_hop: u32,
    ) -> SessionMessageCause {
        SessionMessageCause {
            caller_node_did: message.admission.node_did.clone(),
            caller_request_id: "caller-request".to_owned(),
            caller_request_doc_id: message.admission.tool.request_doc_id().unwrap().to_owned(),
            caller_hop,
            tool_call_id: "hop-tool".to_owned(),
            tool_call_doc_id: "hop-tool-doc".to_owned(),
            correlation: None,
        }
    }

    /// Lean `DurableLineage.sessionMessageWrite`: a message into an idle
    /// session is a new request and one into a busy session steers it; both
    /// climb past their caller without lowering the session's hop, and name
    /// the calling request and tool call. Steering is queued after the active
    /// request, which is never its origin.
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
        assert!(cases.iter().any(|case| case.delivery == "steering"));
        for case in cases {
            let name = case.name.as_str();
            assert_eq!(case.own_hop, caused.request_hop, "{name}");
            let state = if case.delivery == "steering" {
                "processing"
            } else {
                "pending"
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
            let planned = plan(&node, &cause, &target, body, None, false)
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
}

#[cfg(test)]
mod agent_tools_tests {
    use super::*;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_session_message, PublishedAdmissionOptions, PublishedSessionMessage,
    };
    use crate::tool_call_lifecycle::AwaitMode;

    async fn started(
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

    /// The session a claim of `caused` materializes, with its provenance.
    async fn materialize_session(node: &EmbeddedNode, caused: &crate::AgentRequest) {
        let caused = caused.clone();
        ConfigAccess::transact_local(node, None, "test.materialize_session", move |txn| {
            let caused = caused.clone();
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    &caused.session_id,
                    &caused.node_did,
                    &caused.agent_id,
                    caused.requester_did.as_deref(),
                    None,
                    caused
                        .caused_by_parent_request_doc_id
                        .clone()
                        .map(|parent| gents_protocol::session::SessionProvenance {
                            parent_request_doc_id: Some(parent),
                            ..Default::default()
                        }),
                    &chrono::Utc::now().to_rfc3339(),
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

    /// Lean `DurableLineage.interruptAllowed` through `gents::session_origin`:
    /// only the session that started a session may interrupt it.
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
        // The started session may not interrupt the session that started it.
        assert!(interrupt_refusal(&node, &target, &caller)
            .await
            .unwrap()
            .is_some());
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }

    /// `agent_interrupt` stops the busy session's turn; `agent_message` with
    /// `interrupt` plans a new request rather than steering.
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
        assert_eq!(interrupt_session(&node, &scope).await.unwrap(), None);
        set_state(&node, &message.caused_request_doc_id, "processing").await;

        let cause = SessionMessageCause {
            caller_node_did: caller.node_did.clone(),
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
        assert_eq!(steering.delivery(), Delivery::Steering);
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

    /// `agent_list` reports the allowlist and each reachable session with its
    /// relationship to the calling session.
    #[tokio::test]
    async fn agent_list_reports_agents_and_reachable_sessions() {
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
        let sessions = listed["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1, "{listed}");
        assert_eq!(sessions[0]["session_id"], caused.session_id);
        assert_eq!(sessions[0]["relationship"], "started_by_you");
        assert_eq!(sessions[0]["can_message"], true);
        assert_eq!(sessions[0]["can_interrupt"], true);
        assert_eq!(sessions[0]["status"], "idle");

        let from_child = agent_list(&node, &caused, &tools).await.unwrap();
        let sessions = from_child["sessions"].as_array().unwrap();
        assert!(
            sessions
                .iter()
                .any(|session| session["session_id"] == caller.session_id
                    && session["relationship"] == "started_you"
                    && session["can_interrupt"] == false),
            "{from_child}"
        );
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }
}
