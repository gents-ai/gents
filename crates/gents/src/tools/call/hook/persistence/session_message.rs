use super::*;
use anyhow::Context;

use crate::session::message::{AgentMessageArgs, AgentNewArgs};
use crate::tools::native::AGENT_SESSION_LIST_TOOL_NAME;
use crate::store::config_client::TransactionFailure;

fn unavailable_agent_payload(tool_name: &str) -> String {
    service_unavailable_payload(tool_name, "/agent", "selected agent is unavailable", false)
}

/// An agent_message refusal for a session_id this caller cannot reach. Models
/// often pass an agent name or Agent DID there; the refusal then names the exact
/// agent_new call that starts that agent with the same body.
fn unreachable_session_payload(
    tool_name: &str,
    requested: &str,
    args: &AgentMessageArgs,
    tools: &crate::tools::surface::AgentToolConfig,
) -> String {
    let named = tools.targets.iter().find(|target| {
        target.name.eq_ignore_ascii_case(requested) || target.agent_did == requested
    });
    let only = match tools.targets.as_slice() {
        [target] => Some(target),
        _ => None,
    };
    let start = |target: &crate::documents::AgentTargetDocument| {
        let mut arguments = json!({"agent": target.name});
        match (&args.message, &args.task) {
            (_, Some(task)) => {
                arguments["task"] = json!({"task_id": task.task_id, "input": task.input})
            }
            (Some(message), None) => arguments["prompt"] = json!(message),
            (None, None) => arguments["prompt"] = json!("<PROMPT>"),
        }
        json!({"tool": AGENT_NEW_TOOL_NAME, "arguments": arguments})
    };
    let (message, next_call) = match (named, tools.agent_new_enabled()) {
        (Some(target), true) => (
            format!(
                "'{requested}' names the agent '{}', not a session. Start it with agent_new as next_call shows; that call delivers this message, so do not send it again. Use agent_message with the session_id agent_new returns only for a later follow-up. If agent_new is not among your tools, that agent is unavailable: read why with config agent get {}.",
                target.name, target.agent_did
            ),
            start(target),
        ),
        (None, true) => (
            format!(
                "no session '{requested}' is reachable. To start a new conversation, call agent_new with one of allowed_agents; to continue one, copy its session_id from agent_session_list."
            ),
            only.map(start).unwrap_or_else(
                || json!({"tool": AGENT_SESSION_LIST_TOOL_NAME, "arguments": {}}),
            ),
        ),
        (_, false) => (
            format!(
                "no session '{requested}' is reachable, and this agent has no agent targets, so agent_new is unavailable. Copy a session_id from agent_session_list, or add an AgentTarget and select it in Tools.agents.target_ids (config help agent-target)."
            ),
            json!({"tool": AGENT_SESSION_LIST_TOOL_NAME, "arguments": {}}),
        ),
    };
    json_string(json!({
        "ok": false,
        "failure_class": "tool_not_allowed",
        "path": "/session_id",
        "message": message,
        "next_call": next_call,
        "retryable": false,
        "service_id": "session",
        "tool_name": tool_name,
        "requested_tool_name": requested,
        "allowed_agents": tools.target_names()
    }))
}

fn native_permission_denied<E>(error: &E) -> bool
where
    E: TransactionFailure,
{
    error
        .config_client_error()
        .is_some_and(|error| error.is_permission_denied())
}

impl DefraSessionHook {
    pub(super) async fn persist_session_message_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                tool_name,
                args,
                deadline_at,
                AwaitMode::Background,
            )
            .await?;
        macro_rules! refuse {
            ($class:expr, $payload:expr) => {{
                let payload = $payload;
                Box::pin(lifecycle.spawn_failed($class, &payload)).await?;
                return Ok(self.skip_tool_result(tool_name, payload));
            }};
        }

        let caller_doc_id = lifecycle
            .request_doc_id()
            .context("session-message dispatch lacks its calling request document")?
            .to_owned();
        let access = self.actor_access()?;
        let caller = crate::request::binding::load_agent_request_by_doc_id_with_access(
            &access,
            &caller_doc_id,
        )
        .await?
        .context("session-message calling request disappeared")?;
        let tools = crate::session::message::load_caller_session_tools(
            &self.node,
            &caller.node_did,
            &caller.agent_did,
        )
        .await?;
        if !tools.enabled {
            refuse!(
                FailureClass::ServiceUnavailable,
                tool_not_allowed_payload(
                    tool_name,
                    "/",
                    tool_name,
                    "the agents tools are not enabled for this agent",
                    tools.target_names(),
                )
            );
        }

        let create = tool_name == AGENT_NEW_TOOL_NAME;
        let (target, body, title, delivery) = if create {
            let parsed = match serde_json::from_str::<AgentNewArgs>(args) {
                Ok(parsed) => parsed,
                Err(error) => refuse!(
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(
                        tool_name,
                        "/",
                        format!("invalid agent_new arguments: {error}"),
                    )
                ),
            };
            let agent = parsed.agent.trim().to_owned();
            let Some(target) = tools.target(&agent).cloned() else {
                refuse!(
                    FailureClass::ArgumentInvalid,
                    tool_not_allowed_payload(
                        tool_name,
                        "/agent",
                        &agent,
                        format!("'{agent}' is not an allowed agent for this agent"),
                        tools.target_names(),
                    )
                );
            };
            if target.target_node_did == caller.node_did {
                match crate::session::message::target_agent_exists(
                    &access,
                    &target.target_node_did,
                    &target.agent_did,
                )
                .await
                {
                    Ok(true) => {}
                    Ok(false) => refuse!(
                        FailureClass::ServiceUnavailable,
                        unavailable_agent_payload(tool_name)
                    ),
                    Err(error) if native_permission_denied(&error) => {
                        tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(error.as_ref()), "agent_new target observation denied");
                        refuse!(
                            FailureClass::ServiceUnavailable,
                            unavailable_agent_payload(tool_name)
                        );
                    }
                    Err(error) => return Err(error),
                }
            }
            (
                crate::request::lifecycle::SessionMessageTarget {
                    node_did: target.target_node_did,
                    agent_did: target.agent_did,
                    session_id: uuid::Uuid::new_v4().to_string(),
                },
                (parsed.prompt, parsed.task),
                parsed.title,
                crate::session::message::AgentMessageDelivery::Queue,
            )
        } else {
            let parsed = match serde_json::from_str::<AgentMessageArgs>(args) {
                Ok(parsed) => parsed,
                Err(error) => refuse!(
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(
                        tool_name,
                        "/",
                        format!("invalid agent_message arguments: {error}"),
                    )
                ),
            };
            let target_session = parsed.session_id.trim().to_owned();
            if target_session == caller.session_id {
                refuse!(
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(
                        tool_name,
                        "/session_id",
                        "agent_message cannot address the calling session itself",
                    )
                );
            }
            let Some(target) = crate::session::message::resolve_send_target(
                &self.node,
                &caller.node_did,
                &caller.agent_did,
                &tools,
                &target_session,
            )
            .await?
            else {
                refuse!(
                    FailureClass::ArgumentInvalid,
                    unreachable_session_payload(tool_name, &target_session, &parsed, &tools)
                );
            };
            let delivery = match parsed.delivery() {
                Ok(delivery) => delivery,
                Err(message) => refuse!(
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(tool_name, "/delivery", message)
                ),
            };
            if delivery == crate::session::message::AgentMessageDelivery::Interrupt {
                if let Some(reason) = crate::session::message::interrupt_refusal(
                    &self.node,
                    &caller.agent_did,
                    &crate::session::origin::SessionScope::of_request(&caller),
                    &crate::session::message::target_scope(&caller.agent_did, &target),
                )
                .await?
                {
                    refuse!(
                        FailureClass::ArgumentInvalid,
                        interrupt_refused_payload(tool_name, &target_session, &reason)
                    );
                }
            }
            (target, (parsed.message, parsed.task), None, delivery)
        };
        let field = if create { "prompt" } else { "message" };
        let body =
            match crate::session::message::message_body(field, body.0.as_ref(), body.1.as_ref()) {
                Ok(body) => body,
                Err(message) => refuse!(
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(tool_name, "/", message)
                ),
            };

        let caller_hop = caller.request_hop;
        let rendered = match crate::session::message::render_body(
            &self.node,
            &caller.agent_did,
            &caller.node_did,
            &target.agent_did,
            &target.session_id,
            body,
        )
        .await?
        {
            Ok(rendered) => rendered,
            Err(message) => refuse!(
                FailureClass::ArgumentInvalid,
                invalid_tool_arguments_payload(tool_name, "/task", message)
            ),
        };

        let tool_call_doc_id = lifecycle
            .doc_id()
            .context("session-message dispatch lacks its physical row")?
            .to_owned();
        let cause = crate::request::lifecycle::SessionMessageCause {
            caller_node_did: caller.node_did.clone(),
            caller_agent_did: caller.agent_did.clone(),
            caller_session_id: caller.session_id.clone(),
            caller_requester_did: caller.requester_did.clone(),
            caller_request_id: caller.request_id.clone(),
            caller_request_doc_id: caller.doc_id.clone(),
            caller_hop,
            tool_call_id: lifecycle.tool_call_id().to_owned(),
            tool_call_doc_id,
            correlation: caller.caused_by_correlation.clone(),
        };
        let signer = lifecycle
            .access()?
            .managed_agent_signer()
            .context("session message requires a managed Agent signer")?;
        let mut plan = match crate::session::message::plan_with_delivery(
            &self.node,
            &cause,
            &target,
            rendered,
            title.as_deref(),
            delivery,
            &signer,
        )
        .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(message)) => refuse!(
                FailureClass::ArgumentInvalid,
                invalid_tool_arguments_payload(
                    tool_name,
                    if delivery == crate::session::message::AgentMessageDelivery::Steer
                        && target.node_did != caller.node_did
                    {
                        "/delivery"
                    } else {
                        "/task"
                    },
                    message
                )
            ),
            Err(error) if create => {
                tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(error.as_ref()), "agent_new target planning failed");
                if native_permission_denied(&error) {
                    refuse!(
                        FailureClass::ServiceUnavailable,
                        unavailable_agent_payload(tool_name)
                    );
                }
                refuse!(
                    FailureClass::ServiceUnavailable,
                    service_unavailable_payload(
                        tool_name,
                        "/agent",
                        "selected agent is temporarily unavailable",
                        true
                    )
                );
            }
            Err(error) => refuse!(
                FailureClass::ServiceUnavailable,
                service_unavailable_payload(
                    tool_name,
                    "/session_id",
                    format!("the target chat could not be read: {error:#}"),
                    true,
                )
            ),
        };
        if create {
            plan.sign_agent_creation(&caller.node_did, &caller.input, &signer)
                .await?;
        }
        if target.node_did == caller.node_did {
            let observation =
                crate::request::admission::max_request_hop(&self.node, &caller.node_did).await?;
            match (observation.admit(plan.hop()), observation.bound()) {
                (None, _) => refuse!(
                    FailureClass::ServiceUnavailable,
                    service_unavailable_payload(
                        tool_name,
                        "/session_id",
                        "this node's max_request_hop could not be read".to_owned(),
                        true,
                    )
                ),
                (Some(false), Some(max_request_hop)) => refuse!(
                    FailureClass::ArgumentInvalid,
                    hop_exceeded_payload(tool_name, plan.hop(), max_request_hop)
                ),
                _ => {}
            }
        }
        let receipt = match crate::session::message::commit(
            &self.node,
            &cause,
            &mut lifecycle,
            plan,
            !create,
        )
        .await
        {
            Ok(receipt) => crate::tools::output::render(
                &receipt,
                &["status", "session_id", "request_id", "tool_call_id"],
            )?,
            Err(crate::session::message::CommitError::Steer(refusal)) => {
                refuse!(
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(tool_name, "/delivery", refusal.to_string())
                );
            }
            Err(error @ crate::session::message::CommitError::ObservedTerminal { .. }) => {
                return Ok(ToolCallHookAction::Terminate {
                    reason: error.to_string(),
                });
            }
            Err(error) => {
                if let crate::session::message::CommitError::Other(ref error) = error {
                    if let Some(capacity) = error.downcast_ref::<BackgroundCapacityExceeded>() {
                        refuse!(
                            FailureClass::ServiceUnavailable,
                            background_budget_exceeded_payload(tool_name, capacity.0)
                        );
                    }
                }
                if create {
                    tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "agent_new publication failed");
                    if native_permission_denied(&error) {
                        refuse!(
                            FailureClass::ServiceUnavailable,
                            unavailable_agent_payload(tool_name)
                        );
                    }
                    refuse!(
                        FailureClass::ServiceUnavailable,
                        service_unavailable_payload(
                            tool_name,
                            "/agent",
                            "selected agent is temporarily unavailable",
                            true
                        )
                    );
                }
                refuse!(
                    FailureClass::ServiceUnavailable,
                    service_unavailable_payload(
                        tool_name,
                        "/",
                        format!("the message could not be delivered: {error:#}"),
                        true,
                    )
                );
            }
        };
        Ok(self.skip_tool_result(tool_name, receipt))
    }
}

impl DefraSessionHook {
    pub(super) async fn persist_agent_control_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                tool_name,
                args,
                deadline_at,
                AwaitMode::Foreground,
            )
            .await?;
        lifecycle.start_running().await?;
        let result = self
            .agent_control_result(&lifecycle, tool_name, args)
            .await?;
        self.complete_control_tool_call(&mut lifecycle, tool_name, result)
            .await
    }

    async fn agent_control_result(
        &self,
        lifecycle: &crate::tools::call::lifecycle::ToolCallLifecycle,
        tool_name: &str,
        args: &str,
    ) -> anyhow::Result<String> {
        let caller_doc_id = lifecycle
            .request_doc_id()
            .context("agents tool call lacks its calling request document")?
            .to_owned();
        let access = self.actor_access()?;
        let caller = crate::request::binding::load_agent_request_by_doc_id_with_access(
            &access,
            &caller_doc_id,
        )
        .await?
        .context("agents tool calling request disappeared")?;
        let tools = crate::session::message::load_caller_session_tools(
            &self.node,
            &caller.node_did,
            &caller.agent_did,
        )
        .await?;
        if !tools.enabled {
            return Ok(tool_not_allowed_payload(
                tool_name,
                "/",
                tool_name,
                "the agents tools are not enabled for this agent",
                tools.target_names(),
            ));
        }
        if tool_name == crate::tools::native::AGENT_LIST_TOOL_NAME {
            if let Err(error) = serde_json::from_str::<crate::session::message::AgentListArgs>(args)
            {
                return Ok(invalid_tool_arguments_payload(
                    tool_name,
                    "/",
                    format!("invalid agent_list arguments: {error}"),
                ));
            }
            return Ok(json_string(
                crate::session::message::agent_list(&self.node, &caller, &tools).await?,
            ));
        }
        if tool_name == crate::tools::native::AGENT_SESSION_LIST_TOOL_NAME {
            let parsed =
                match serde_json::from_str::<crate::session::message::AgentSessionListArgs>(args) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        return Ok(invalid_tool_arguments_payload(
                            tool_name,
                            "/",
                            format!("invalid agent_session_list arguments: {error}"),
                        ))
                    }
                };
            if let Err(error) = parsed.validate() {
                return Ok(invalid_tool_arguments_payload(
                    tool_name,
                    "/",
                    error.to_string(),
                ));
            }
            return Ok(json_string(
                crate::session::message::agent_session_list(&self.node, &caller, &tools, &parsed)
                    .await?,
            ));
        }
        if tool_name == crate::tools::native::AGENT_SESSION_READ_TOOL_NAME {
            let parsed =
                match serde_json::from_str::<crate::session::message::AgentSessionReadArgs>(args) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        return Ok(invalid_tool_arguments_payload(
                            tool_name,
                            "/",
                            format!("invalid agent_session_read arguments: {error}"),
                        ))
                    }
                };
            if let Err(error) = parsed.validate() {
                return Ok(invalid_tool_arguments_payload(
                    tool_name,
                    "/",
                    error.to_string(),
                ));
            }
            if parsed.session_id.trim() == caller.session_id {
                return Ok(invalid_tool_arguments_payload(tool_name, "/session_id", "agent_session_read cannot address the calling session; use sessions to inspect this conversation"));
            }
            let Some(target) = crate::session::message::resolve_send_target(
                &self.node,
                &caller.node_did,
                &caller.agent_did,
                &tools,
                parsed.session_id.trim(),
            )
            .await?
            else {
                return Ok(tool_not_allowed_payload(tool_name, "/session_id", &parsed.session_id, "session is not messageable; discover available sessions with agent_session_list", tools.target_names()));
            };
            if let Err(error) = parsed.validate_target(&crate::session::message::target_scope(
                &caller.agent_did,
                &target,
            )) {
                return Ok(invalid_tool_arguments_payload(
                    tool_name,
                    "/cursor",
                    error.to_string(),
                ));
            }
            return Ok(json_string(
                crate::session::message::agent_session_read(&self.node, &caller, &tools, &parsed)
                    .await?,
            ));
        }
        let control_tool = matches!(
            tool_name,
            crate::tools::native::AGENT_INTERRUPT_TOOL_NAME
                | crate::tools::native::AGENT_STOP_TOOL_NAME
                | crate::tools::native::AGENT_RESUME_TOOL_NAME
        );
        if !control_tool {
            return Ok(invalid_tool_arguments_payload(
                tool_name,
                "/",
                "unknown session tool",
            ));
        }
        let parsed = match serde_json::from_str::<crate::session::message::AgentInterruptArgs>(args)
        {
            Ok(parsed) => parsed,
            Err(error) => {
                return Ok(invalid_tool_arguments_payload(
                    tool_name,
                    "/",
                    format!("invalid {tool_name} arguments: {error}"),
                ))
            }
        };
        let session = parsed.session_id.trim();
        let Some(target) = crate::session::message::resolve_send_target(
            &self.node,
            &caller.node_did,
            &caller.agent_did,
            &tools,
            session,
        )
        .await?
        else {
            return Ok(interrupt_refused_payload(
                tool_name,
                session,
                "only the session that started this session may control it",
            ));
        };
        let target = crate::session::message::target_scope(&caller.agent_did, &target);
        if let Some(reason) = crate::session::message::interrupt_refusal(
            &self.node,
            &caller.agent_did,
            &crate::session::origin::SessionScope::of_request(&caller),
            &target,
        )
        .await?
        {
            return Ok(interrupt_refused_payload(tool_name, session, &reason));
        }
        if matches!(
            tool_name,
            crate::tools::native::AGENT_STOP_TOOL_NAME
                | crate::tools::native::AGENT_RESUME_TOOL_NAME
        ) && target.node_did != caller.node_did
        {
            return Ok(interrupt_refused_payload(
                tool_name,
                session,
                "stop and resume currently require a session on this node; use agent_message with interrupt=true for a remote session",
            ));
        }
        match tool_name {
            crate::tools::native::AGENT_INTERRUPT_TOOL_NAME => {
                let interrupted = crate::session::message::interrupt_session_with_access(
                    &self.node,
                    &self.actor_access()?,
                    &target,
                )
                .await?;
                Ok(json_string(json!({
                    "ok": true,
                    "session_id": session,
                    "status": if interrupted.is_some() { "interrupting" } else { "idle" },
                    "request_id": interrupted
                })))
            }
            crate::tools::native::AGENT_STOP_TOOL_NAME => {
                let receipt = crate::session::stop_session_with_access(
                    &self.node,
                    &self.actor_access()?,
                    &target.node_did,
                    &target.session_id,
                    target.requester_did.as_deref(),
                )
                .await?;
                Ok(json_string(json!({"ok": true, "session_id": session,
                    "status": receipt.status,
                    "interrupted_request_id": receipt.interrupted_request_id,
                    "drained_request_ids": receipt.drained_request_ids,
                    "next": "call agent_session_read with view=status to observe request and tool settlement"})))
            }
            crate::tools::native::AGENT_RESUME_TOOL_NAME => {
                let Some(job_driver) = self.job_driver.clone() else {
                    anyhow::bail!("agent_resume cannot verify plugin execution settlement because the runtime job owner is unavailable; retry after runtime recovery")
                };
                crate::session::resume_session_with_access(
                    &self.node,
                    &self.actor_access()?,
                    &target.node_did,
                    &target.session_id,
                    target.requester_did.as_deref(),
                    self.background_executions.clone(),
                    job_driver,
                )
                .await?;
                Ok(json_string(
                    json!({"ok": true, "session_id": session, "status": "resumed"}),
                ))
            }
            _ => unreachable!(),
        }
    }
}

fn interrupt_refused_payload(tool_name: &str, session_id: &str, reason: &str) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "tool_not_allowed",
        "code": "interrupt_not_permitted",
        "path": "/session_id",
        "message": reason,
        "retryable": false,
        "service_id": "session",
        "tool_name": tool_name,
        "session_id": session_id
    }))
}

fn hop_exceeded_payload(tool_name: &str, hop: u32, max_request_hop: u32) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "invalid_tool_arguments",
        "code": "request_hop_exceeded",
        "path": "/",
        "message": format!(
            "this message would be hop {hop}, beyond the node's max_request_hop {max_request_hop}"
        ),
        "retryable": false,
        "service_id": "session",
        "tool_name": tool_name,
        "hop": hop,
        "max_request_hop": max_request_hop
    }))
}

#[cfg(test)]
mod unreachable_session_tests {
    use super::*;

    fn tools(names: &[&str]) -> crate::tools::surface::AgentToolConfig {
        crate::tools::surface::AgentToolConfig {
            enabled: true,
            targets: names
                .iter()
                .map(|name| crate::documents::AgentTargetDocument {
                    target_id: format!("{name}-target"),
                    node_did: "did:key:node".to_owned(),
                    target_node_did: "did:key:node".to_owned(),
                    agent_did: format!("did:key:{name}"),
                    name: (*name).to_owned(),
                    description: None,
                    tags: Vec::new(),
                })
                .collect(),
        }
    }

    fn refusal(session_id: &str, tools: &crate::tools::surface::AgentToolConfig) -> serde_json::Value {
        let args: AgentMessageArgs = serde_json::from_value(
            json!({"session_id": session_id, "message": "Reply with exactly TRAIN-111"}),
        )
        .unwrap();
        serde_json::from_str(&unreachable_session_payload(
            "agent_message",
            session_id,
            &args,
            tools,
        ))
        .unwrap()
    }

    #[test]
    fn an_agent_name_as_session_id_names_the_agent_new_call() {
        let tools = tools(&["research-helper", "reviewer"]);
        for requested in ["research-helper", "Research-Helper", "did:key:research-helper"] {
            let refusal = refusal(requested, &tools);
            assert_eq!(
                refusal["next_call"],
                json!({"tool": "agent_new", "arguments": {"agent": "research-helper", "prompt": "Reply with exactly TRAIN-111"}}),
                "{requested}"
            );
            assert!(refusal["message"].as_str().unwrap().contains("not a session"));
        }
    }

    #[test]
    fn an_unknown_session_points_to_the_only_target_or_the_session_list() {
        assert_eq!(
            refusal("missing", &tools(&["helper"]))["next_call"]["arguments"]["agent"],
            "helper"
        );
        assert_eq!(
            refusal("missing", &tools(&["a", "b"]))["next_call"],
            json!({"tool": "agent_session_list", "arguments": {}})
        );
        let none = refusal("missing", &tools(&[]));
        assert_eq!(none["next_call"]["tool"], "agent_session_list");
        assert!(none["message"].as_str().unwrap().contains("agent_new is unavailable"));
    }
}
