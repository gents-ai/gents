use defra_node::EmbeddedNode;
use gents_protocol::error_chain::ErrorChain;
use serde::Deserialize;

use std::sync::Arc;

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::lifecycle::{SessionMessageCause, SessionMessageTarget};
use crate::session_origin::SessionScope;
use crate::tool_surface::AgentToolConfig;

#[derive(Debug, thiserror::Error)]
pub enum SessionMessageError {
    #[error("time filters require RFC 3339 timestamps")]
    InvalidTimeFilter(#[source] chrono::ParseError),
    #[error("limit must be between 1 and 100")]
    InvalidListLimit,
    #[error("status must be busy or idle")]
    InvalidStatusFilter,
    #[error("invalid relationship")]
    InvalidRelationshipFilter,
    #[error("trigger query omitted request rows")]
    TriggerRowsOmitted,
    #[error("cursor query omitted session rows")]
    CursorRowsOmitted,
    #[error("session list omitted canonical rows")]
    ListRowsOmitted,
    #[error("session omitted physical identity")]
    SessionIdentityMissing,
    #[error("listed session is no longer messageable")]
    ListedSessionNotMessageable,
    #[error("session_id is required; discover it with agent_session_list")]
    SessionIdRequired,
    #[error("limit must be between 1 and 6")]
    InvalidReadLimit,
    #[error("cursor is supported only for recent messages")]
    CursorRequiresRecentView,
    #[error("invalid recent cursor; restart agent_session_read with view=recent")]
    InvalidRecentCursor(#[source] serde_json::Error),
    #[error("cursor belongs to another session; restart agent_session_read with view=recent")]
    CursorOtherSession,
    #[error("invalid cursor; next call: agent_session_read with session_id and view=recent")]
    InvalidCursor(#[source] serde_json::Error),
    #[error(transparent)]
    CursorJson(serde_json::Error),
    #[error("session inspection omitted {collection}")]
    InspectionOmitted { collection: String },
    #[error("decoding queued session input for request {request_id}")]
    DecodeQueuedInput { request_id: serde_json::Value },
    #[error("decoding inspected request")]
    DecodeInspectedRequest(#[source] serde_json::Error),
    #[error("canonical tool summary references an unavailable call")]
    ToolSummaryUnavailable,
    #[error("session is not messageable; discover available sessions with agent_session_list")]
    SessionNotMessageable,
    #[error("agent_session_read cannot address the calling session; use sessions to inspect this conversation")]
    CallingSessionRead,
    #[error("inspected session is unavailable or ambiguous")]
    InspectedSessionAmbiguous,
    #[error("inspected session is no longer messageable")]
    InspectedSessionNotMessageable,
    #[error("The target chat's Goal state is still syncing. Read the chat and retry the message.")]
    GoalStateSyncing,
    #[error("session-message target Agent differs from its canonical session")]
    TargetAgentMismatch,
    #[error("the target session moved to hop {current} while this message was planned")]
    TargetSessionMoved { current: u32 },
    #[error("decode configured Task")]
    TaskDecode(#[source] serde_json::Error),
    #[error("session-message row lacks physical identity")]
    RowIdentityMissing,
    #[error("session-message row disappeared after its commit failed")]
    RowDisappeared,
    #[error("session-message row was altered or is no longer pending")]
    RowNotPending,
    #[error("active session request lacks physical identity")]
    ActiveRequestIdentityMissing,
    #[error("active session request disappeared")]
    ActiveRequestDisappeared,
    #[error("active request lacks physical identity")]
    ActiveRequestLacksIdentity,
    #[error("caused request lacks physical identity")]
    CausedRequestIdentityMissing,
    #[error("caused request lacks node_did")]
    CausedRequestNodeDidMissing,
    #[error("session-message row lacks its calling request document")]
    RowCallingRequestMissing,
    #[error("session-message calling request disappeared")]
    CallingRequestDisappeared,
    #[error("tool-call summary row omitted _docID")]
    ToolSummaryDocIdMissing,
    #[error(
        "cursor belongs to another session scope; restart agent_session_read with view=recent"
    )]
    CursorOtherSessionScope,
    #[error("session inspection requires a valid caller DID")]
    InvalidCallerDid(#[source] ::identity::Error),
    #[error("session-message commit outcome is unknown after: {error}")]
    CommitOutcomeUnknown { error: String },
    #[error("{0}")]
    MutationRender(&'static str),
    #[error("caller DID is not ACP-addressable")]
    CallerDIDNotACPAddressable(#[source] identity::Error),
    #[error(transparent)]
    AgentRequestAdmission(Box<crate::request_admission::AgentRequestAdmissionError>),
    #[error(transparent)]
    CanonicalRows(#[from] crate::session::canonical_rows::CanonicalRowsError),
    #[error(transparent)]
    ConfigClient(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Delivery(#[from] crate::tool_call_lifecycle::delivery::DeliveryError),
    #[error(transparent)]
    DesiredState(#[from] crate::config_client::DesiredStateError),
    #[error(transparent)]
    GraphqlResponse(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),
    #[error(transparent)]
    Interrupt(Box<crate::interrupt::InterruptError>),
    #[error(transparent)]
    RequestBinding(#[from] crate::request_binding::RequestBindingError),
    #[error(transparent)]
    SessionOrigin(#[from] crate::session_origin::SessionOriginError),
    #[error(transparent)]
    SessionOutput(#[from] crate::session::SessionOutputError),
    #[error(transparent)]
    ToolCallQuery(#[from] crate::tool_call_lifecycle::query::ToolCallQueryError),
    #[error(transparent)]
    ToolSurface(#[from] crate::tool_surface::ToolSurfaceError),
    #[error(transparent)]
    BackgroundCompletion(#[from] crate::background_completion::BackgroundCompletionError),
    #[error(transparent)]
    Parse(#[from] chrono::ParseError),
    #[error(transparent)]
    Goal(Box<crate::goal::GoalError>),
    #[error(transparent)]
    Materialize(Box<crate::lifecycle::materialize::MaterializeError>),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Session(Box<crate::session::SessionError>),
    #[error(transparent)]
    Transition(#[from] crate::tool_call_lifecycle::TransitionError),
}

impl From<crate::lifecycle::materialize::MaterializeError> for SessionMessageError {
    fn from(error: crate::lifecycle::materialize::MaterializeError) -> Self {
        Self::Materialize(Box::new(error))
    }
}

impl From<crate::session::SessionError> for SessionMessageError {
    fn from(error: crate::session::SessionError) -> Self {
        Self::Session(Box::new(error))
    }
}

impl From<crate::goal::GoalError> for SessionMessageError {
    fn from(error: crate::goal::GoalError) -> Self {
        Self::Goal(Box::new(error))
    }
}

impl From<crate::interrupt::InterruptError> for SessionMessageError {
    fn from(error: crate::interrupt::InterruptError) -> Self {
        Self::Interrupt(Box::new(error))
    }
}

impl From<crate::request_admission::AgentRequestAdmissionError> for SessionMessageError {
    fn from(error: crate::request_admission::AgentRequestAdmissionError) -> Self {
        Self::AgentRequestAdmission(Box::new(error))
    }
}

impl crate::config_client::TransactionFailure for SessionMessageError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::ConfigClient(error) => Some(error),
            Self::Delivery(error) => error.config_client_error(),
            Self::DesiredState(error) => error.config_client_error(),
            Self::GraphqlResponse(error) => error.config_client_error(),
            Self::Interrupt(error) => error.config_client_error(),
            Self::SessionOrigin(error) => error.config_client_error(),
            Self::SessionOutput(error) => error.config_client_error(),
            Self::ToolCallQuery(error) => error.config_client_error(),
            Self::ToolSurface(error) => error.config_client_error(),
            Self::Goal(error) => error.config_client_error(),
            Self::Materialize(error) => error.config_client_error(),
            Self::Session(error) => error.config_client_error(),
            Self::Transition(error) => error.config_client_error(),
            _ => None,
        }
    }
}

type Result<T, E = SessionMessageError> = std::result::Result<T, E>;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Request,
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
    Ok(Option::unwrap_or_default(
        tools
            .map(|tools| AgentToolConfig::from_document_with_targets(&tools, &targets))
            .transpose()?,
    ))
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
        .map(|value| serde_json::from_value(value).map_err(SessionMessageError::TaskDecode))
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
    write: PlannedWrite,
}

impl Plan {
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
                    if session.session.agent_id != target.agent_id {
                        return Err(SessionMessageError::TargetAgentMismatch);
                    }
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
            Err(SessionMessageError::GoalStateSyncing)
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

pub(crate) async fn plan(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    rendered: RenderedBody,
    title: Option<&str>,
    interrupt: bool,
) -> Result<Result<Plan, String>> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let local = target.node_did == cause.caller_node_did;
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
                    .ok_or(SessionMessageError::ActiveRequestIdentityMissing)?;
                Some(
                    crate::request_binding::load_agent_request_by_doc_id(node, active_doc_id)
                        .await?
                        .ok_or(SessionMessageError::ActiveRequestDisappeared)?,
                )
            }
            None => None,
        }
    } else {
        None
    };
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
        .ok_or(SessionMessageError::RowIdentityMissing)?
        .to_owned();
    let durable = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &doc_id,
        lifecycle.node_did(),
        lifecycle.session_id(),
        lifecycle.requester_did(),
    )
    .await?
    .ok_or(SessionMessageError::RowDisappeared)?;
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
    .ok_or_else(|| SessionMessageError::CommitOutcomeUnknown {
        error: ErrorChain(&error).to_string(),
    })?;
    *lifecycle = durable;
    Ok(receipt)
}

async fn commit_once(
    node: &std::sync::Arc<EmbeddedNode>,
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
    let (start, binding, plan, receipt_for) = (&start, &binding, &plan, &receipt_for);
    let (actor, label) = match &plan.write {
        PlannedWrite::Request { .. } => (None, "session_message.commit_request"),
        PlannedWrite::Goal { .. } => (
            Some(
                ::identity::Did::new(cause.caller_node_did.clone())
                    .map_err(SessionMessageError::CallerDIDNotACPAddressable)?,
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
                    &serde_json::to_string(&receipt)?,
                )
                .await?;
                Ok::<_, SessionMessageError>((receipt, started_at))
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
    _cause: &SessionMessageCause,
    plan: &Plan,
) -> Result<crate::lifecycle::EnqueuedAgentRequest> {
    match &plan.write {
        PlannedWrite::Request { create, .. } => {
            ensure_planned_hop_current(txn, create).await?;
            let mutation = create
                .graphql_mutation()
                .map_err(SessionMessageError::MutationRender)?;
            let response = txn.execute(&mutation).await?;
            let doc_id = crate::graphql::created_doc_id(&response, "AgentRequest")?;
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
            crate::goal::stage_goal_backed_request_in_txn(
                txn,
                &create.node_did,
                &plan.session_id,
                objective,
                *token_budget,
                create,
            )
            .await
            .map_err(Into::into)
        }
    }
}

async fn ensure_planned_hop_current(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    let current =
        crate::session::load_session_current_hop_in_txn(txn, &create.node_did, &create.session_id)
            .await?;
    if !(create.request_hop >= current) {
        return Err(SessionMessageError::TargetSessionMoved { current });
    }
    Ok(())
}

async fn start_dispatch(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    start: &crate::tool_call_lifecycle::delivery::DispatchStart,
) -> Result<chrono::DateTime<chrono::Utc>> {
    crate::tool_call_lifecycle::delivery::start_running_in_txn(txn, start, None)
            .await?
            .ok_or(SessionMessageError::RowNotPending)
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
        .ok_or(SessionMessageError::RowIdentityMissing)?;
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
) -> bool {
    target_session != caller_session && target_origin_cause == Some(caller_session)
}

pub async fn interrupt_refusal(
    node: &Arc<EmbeddedNode>,
    caller: &SessionScope,
    target: &SessionScope,
) -> Result<Option<String>> {
    let started_by =
        crate::session_origin::started_by(&ConfigAccess::local(node.clone()), target).await?;
    let cause = started_by
        .filter(|link| link.scope == *caller)
        .map(|link| link.scope.session_id);
    Ok(
        (!agent_interrupt_allowed(&caller.session_id, &target.session_id, cause.as_deref()))
            .then(|| "only the session that started this session may interrupt it".to_owned()),
    )
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
            .ok_or(SessionMessageError::ActiveRequestLacksIdentity)?,
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
        .ok_or(SessionMessageError::CausedRequestIdentityMissing)?;
    let node_did = caused
        .node_did
        .as_deref()
        .ok_or(SessionMessageError::CausedRequestNodeDidMissing)?;
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
            Err(error) if local => return Err(error.into()),
            Err(error) => tracing::warn!(
                caused_request_doc_id = doc_id,
                error = %ErrorChain(&error),
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
        let calling_request_id = calling_request_id(node, lifecycle).await?;
        crate::background_completion::append_background_tool_completion(
            node,
            lifecycle.session_id(),
            &calling_request_id,
            lifecycle
                .doc_id()
                .ok_or(SessionMessageError::RowIdentityMissing)?,
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

pub(crate) async fn calling_request_id(
    node: &EmbeddedNode,
    lifecycle: &crate::tool_call_lifecycle::ToolCallLifecycle,
) -> Result<String> {
    if !lifecycle.request_id().trim().is_empty() {
        return Ok(lifecycle.request_id().to_owned());
    }
    let doc_id = lifecycle
        .request_doc_id()
        .ok_or(SessionMessageError::RowCallingRequestMissing)?;
    Ok(
        crate::request_binding::load_agent_request_by_doc_id(node, doc_id)
            .await?
            .ok_or(SessionMessageError::CallingRequestDisappeared)?
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
            caller_agent_id: "caller-agent".to_owned(),
            caller_session_id: "caller-session".to_owned(),
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
            "unexpected error: {}",
            ErrorChain(&error)
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
        assert_eq!(message_plan.delivery(), Delivery::Steering);
        let delivered = commit(&node, &cause, &mut send_call, message_plan, true)
            .await
            .unwrap();
        assert_eq!(delivered.session_id, "correlated-worker");
        assert_eq!(delivered.delivery.as_deref(), Some("steering"));
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
        let (message, _caller, caused) = started("opening-provenance").await;
        let node = message.admission.node.clone();
        let access = ConfigAccess::local(node.clone());
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
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }
}
