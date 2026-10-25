use super::*;
use anyhow::Context;
use gents_protocol::error_chain::ErrorChain;

use crate::session_message::{AgentMessageArgs, AgentNewArgs};

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
                lifecycle.spawn_failed($class, &payload).await?;
                return Ok(self.skip_tool_result(tool_name, payload));
            }};
        }

        let caller_doc_id = lifecycle
            .request_doc_id()
            .context("session-message dispatch lacks its calling request document")?
            .to_owned();
        let caller =
            crate::request_binding::load_agent_request_by_doc_id(&self.node, &caller_doc_id)
                .await?
                .context("session-message calling request disappeared")?;
        let tools = crate::session_message::load_caller_session_tools(
            &self.node,
            &caller.node_did,
            &caller.agent_id,
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
        let (target, body, title, interrupt) = if create {
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
            if target.target_node_did == caller.node_did
                && load_agent(&self.node, &target.agent_id).await?.is_none()
            {
                refuse!(
                    FailureClass::ServiceUnavailable,
                    service_unavailable_payload(
                        tool_name,
                        "/agent",
                        format!(
                            "agent '{agent}' refers to agent '{}' which no longer exists",
                            target.agent_id
                        ),
                        false,
                    )
                );
            }
            (
                crate::lifecycle::SessionMessageTarget {
                    node_did: target.target_node_did,
                    agent_id: target.agent_id,
                    session_id: uuid::Uuid::new_v4().to_string(),
                },
                (parsed.prompt, parsed.task),
                parsed.title,
                false,
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
            let Some(target) = crate::session_message::resolve_send_target(
                &self.node,
                &caller.node_did,
                &tools,
                &target_session,
            )
            .await?
            else {
                refuse!(
                    FailureClass::ArgumentInvalid,
                    tool_not_allowed_payload(
                        tool_name,
                        "/session_id",
                        &target_session,
                        "session is neither this agent's own nor one it started on an allowed agent",
                        tools.target_names(),
                    )
                );
            };
            if parsed.interrupt {
                if let Some(reason) = crate::session_message::interrupt_refusal(
                    &self.node,
                    &crate::session_origin::SessionScope::of_request(&caller),
                    &crate::session_message::target_scope(&caller.node_did, &target),
                )
                .await?
                {
                    refuse!(
                        FailureClass::ArgumentInvalid,
                        interrupt_refused_payload(tool_name, &target_session, &reason)
                    );
                }
            }
            (
                target,
                (parsed.message, parsed.task),
                None,
                parsed.interrupt,
            )
        };
        let field = if create { "prompt" } else { "message" };
        let body =
            match crate::session_message::message_body(field, body.0.as_ref(), body.1.as_ref()) {
                Ok(body) => body,
                Err(message) => refuse!(
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(tool_name, "/", message)
                ),
            };

        let caller_hop = caller.request_hop;
        let rendered = match crate::session_message::render_body(
            &self.node,
            &caller.node_did,
            &target.agent_id,
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
        let cause = crate::lifecycle::SessionMessageCause {
            caller_node_did: caller.node_did.clone(),
            caller_agent_id: caller.agent_id.clone(),
            caller_session_id: caller.session_id.clone(),
            caller_request_id: caller.request_id.clone(),
            caller_request_doc_id: caller.doc_id.clone(),
            caller_hop,
            tool_call_id: lifecycle.tool_call_id().to_owned(),
            tool_call_doc_id,
            correlation: caller.caused_by_correlation.clone(),
        };
        let plan = match crate::session_message::plan(
            &self.node,
            &cause,
            &target,
            rendered,
            title.as_deref(),
            interrupt,
        )
        .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(message)) => refuse!(
                FailureClass::ArgumentInvalid,
                invalid_tool_arguments_payload(tool_name, "/task", message)
            ),
            Err(error) => refuse!(
                FailureClass::ServiceUnavailable,
                service_unavailable_payload(
                    tool_name,
                    "/session_id",
                    format!(
                        "the target chat could not be read: {}",
                        ErrorChain(error.as_ref())
                    ),
                    true,
                )
            ),
        };
        if target.node_did == caller.node_did {
            let max_request_hop =
                crate::request_admission::max_request_hop(&self.node, &caller.node_did).await?;
            if let Some(max_request_hop) = max_request_hop.filter(|bound| {
                !crate::lifecycle::request_hop_within_bound(Some(*bound), plan.hop())
            }) {
                refuse!(
                    FailureClass::ArgumentInvalid,
                    hop_exceeded_payload(tool_name, plan.hop(), max_request_hop)
                );
            }
        }
        let receipt = match crate::session_message::commit(
            &self.node,
            &cause,
            &mut lifecycle,
            plan,
            !create,
        )
        .await
        {
            Ok(receipt) => crate::tool_output::render(
                &receipt,
                &["status", "session_id", "request_id", "tool_call_id"],
            )?,
            Err(error) => {
                let capacity = error
                    .downcast_ref::<crate::tool_call_lifecycle::delivery::DeliveryError>()
                    .and_then(
                        crate::tool_call_lifecycle::delivery::DeliveryError::background_capacity,
                    )
                    .map(|capacity| capacity.0);
                if let Some(capacity) = capacity {
                    refuse!(
                        FailureClass::ServiceUnavailable,
                        background_budget_exceeded_payload(tool_name, capacity)
                    );
                }
                refuse!(
                    FailureClass::ServiceUnavailable,
                    service_unavailable_payload(
                        tool_name,
                        "/",
                        format!(
                            "the message could not be delivered: {}",
                            ErrorChain(error.as_ref())
                        ),
                        true,
                    )
                );
            }
        };
        // Stop the target's turn only once the message is durably delivered,
        // so a failed commit never interrupts without delivering. The new
        // request waits behind the interrupted one.
        if interrupt {
            let scope = crate::session_message::target_scope(&caller.node_did, &target);
            if let Err(error) = crate::session_message::interrupt_session(&self.node, &scope).await
            {
                tracing::warn!(
                    target_session = %target.session_id,
                    error = %ErrorChain(error.as_ref()),
                    "agent_message delivered, but interrupting the target's turn failed"
                );
            }
        }
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
        lifecycle: &crate::tool_call_lifecycle::ToolCallLifecycle,
        tool_name: &str,
        args: &str,
    ) -> anyhow::Result<String> {
        let caller_doc_id = lifecycle
            .request_doc_id()
            .context("agents tool call lacks its calling request document")?
            .to_owned();
        let caller =
            crate::request_binding::load_agent_request_by_doc_id(&self.node, &caller_doc_id)
                .await?
                .context("agents tool calling request disappeared")?;
        let tools = crate::session_message::load_caller_session_tools(
            &self.node,
            &caller.node_did,
            &caller.agent_id,
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
        if tool_name == crate::toolset::AGENT_LIST_TOOL_NAME {
            if let Err(error) = serde_json::from_str::<crate::session_message::AgentListArgs>(args)
            {
                return Ok(invalid_tool_arguments_payload(
                    tool_name,
                    "/",
                    format!("invalid agent_list arguments: {error}"),
                ));
            }
            return Ok(json_string(
                crate::session_message::agent_list(&self.node, &caller, &tools).await?,
            ));
        }
        if tool_name == crate::toolset::AGENT_SESSION_LIST_TOOL_NAME {
            let parsed =
                match serde_json::from_str::<crate::session_message::AgentSessionListArgs>(args) {
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
                crate::session_message::agent_session_list(&self.node, &caller, &tools, &parsed)
                    .await?,
            ));
        }
        if tool_name == crate::toolset::AGENT_SESSION_READ_TOOL_NAME {
            let parsed =
                match serde_json::from_str::<crate::session_message::AgentSessionReadArgs>(args) {
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
            let Some(target) = crate::session_message::resolve_send_target(
                &self.node,
                &caller.node_did,
                &tools,
                parsed.session_id.trim(),
            )
            .await?
            else {
                return Ok(tool_not_allowed_payload(tool_name, "/session_id", &parsed.session_id, "session is not messageable; discover available sessions with agent_session_list", tools.target_names()));
            };
            if let Err(error) = parsed.validate_target(&crate::session_message::target_scope(
                &caller.node_did,
                &target,
            )) {
                return Ok(invalid_tool_arguments_payload(
                    tool_name,
                    "/cursor",
                    error.to_string(),
                ));
            }
            return Ok(json_string(
                crate::session_message::agent_session_read(&self.node, &caller, &tools, &parsed)
                    .await?,
            ));
        }
        let control_tool = matches!(
            tool_name,
            crate::toolset::AGENT_INTERRUPT_TOOL_NAME
                | crate::toolset::AGENT_STOP_TOOL_NAME
                | crate::toolset::AGENT_RESUME_TOOL_NAME
        );
        if !control_tool {
            return Ok(invalid_tool_arguments_payload(
                tool_name,
                "/",
                "unknown session tool",
            ));
        }
        let parsed = match serde_json::from_str::<crate::session_message::AgentInterruptArgs>(args)
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
        let Some(target) = crate::session_message::resolve_send_target(
            &self.node,
            &caller.node_did,
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
        let target = crate::session_message::target_scope(&caller.node_did, &target);
        if let Some(reason) = crate::session_message::interrupt_refusal(
            &self.node,
            &crate::session_origin::SessionScope::of_request(&caller),
            &target,
        )
        .await?
        {
            return Ok(interrupt_refused_payload(tool_name, session, &reason));
        }
        if matches!(
            tool_name,
            crate::toolset::AGENT_STOP_TOOL_NAME | crate::toolset::AGENT_RESUME_TOOL_NAME
        ) && target.node_did != caller.node_did
        {
            return Ok(interrupt_refused_payload(
                tool_name,
                session,
                "stop and resume currently require a session on this node; use agent_message with interrupt=true for a remote session",
            ));
        }
        match tool_name {
            crate::toolset::AGENT_INTERRUPT_TOOL_NAME => {
                let interrupted =
                    crate::session_message::interrupt_session(&self.node, &target).await?;
                Ok(json_string(json!({
                    "ok": true,
                    "session_id": session,
                    "status": if interrupted.is_some() { "interrupting" } else { "idle" },
                    "request_id": interrupted
                })))
            }
            crate::toolset::AGENT_STOP_TOOL_NAME => {
                let receipt = crate::session::stop_session(
                    &self.node,
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
            crate::toolset::AGENT_RESUME_TOOL_NAME => {
                let Some(job_driver) = self.job_driver.clone() else {
                    anyhow::bail!("agent_resume cannot verify plugin execution settlement because the runtime job owner is unavailable; retry after runtime recovery")
                };
                crate::session::resume_session(
                    &self.node,
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
