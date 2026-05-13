use std::time::Duration;

use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::message::{Message, Text, ToolResult, ToolResultContent, UserContent};
use rig::completion::{CompletionModel, CompletionResponse};
use rig::one_or_many::OneOrMany;
use serde_json::json;
use tracing::Instrument;

use crate::config::DEFAULT_DEADLINE_DURATION_SECS;
use crate::session;
use crate::subagent_tools::{
    child_request_completed, load_authorized_child_edge, load_child_final_response,
    load_child_session_id, load_child_terminal_row, load_parent_subagent_context,
    project_child_terminal, target_is_allowed, ParentSubagentContext, SpawnSubagentArgs,
};
use crate::tool_call_lifecycle::query::load_tool_call_result;
use crate::tool_call_lifecycle::runtime::{classify_managed_tool_result, ManagedToolTerminal};
use crate::tool_call_lifecycle::{
    create_subagent_request_with_request_id, AwaitMode, CancelPolicy, ChildTerminal, FailureClass,
    ToolCallLifecycle, MAX_SUBAGENT_DEPTH,
};
use crate::toolset::SPAWN_SUBAGENT_TOOL_NAME;
use crate::truncation::{truncate_text, DefraSpillTruncator, TruncationMode, Truncator};

use super::{non_empty, DefraSessionHook};

impl DefraSessionHook {
    pub async fn persist_message(&self, message: &Message) -> anyhow::Result<u32> {
        let content = serde_json::to_string(message)?;
        let (session_id, sequence, role, message_key) = {
            let mut state = self.state.lock().await;
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
            let message_key = tool_result_message_key(&session_id, message)?;
            if let Some(existing_sequence) = message_key
                .as_ref()
                .and_then(|key| state.persisted_tool_result_message_sequences.get(key))
            {
                return Ok(*existing_sequence);
            }

            match message {
                Message::User { .. } => {
                    state.sequence += 1;
                    let sequence = state.sequence;
                    state.reset_after_user_message();
                    if let Some(key) = message_key.as_ref() {
                        state
                            .persisted_tool_result_message_sequences
                            .insert(key.clone(), sequence);
                    }
                    (session_id, sequence, "user", message_key)
                }
                Message::Assistant { .. } => {
                    let sequence = state.persist_assistant_turn()?;
                    (session_id, sequence, "assistant", None)
                }
                Message::System { .. } => {
                    anyhow::bail!("system messages are not persisted in session history");
                }
            }
        };

        match message_key {
            Some(message_key) => {
                session::save_message_with_key(
                    &self.node,
                    &session_id,
                    sequence,
                    role,
                    &content,
                    &message_key,
                )
                .await?;
            }
            None => {
                session::save_message(&self.node, &session_id, sequence, role, &content).await?;
            }
        }
        Ok(sequence)
    }

    pub async fn persist_stream_tool_result_message(
        &self,
        tool_result: &ToolResult,
        internal_call_id: &str,
    ) -> anyhow::Result<()> {
        let session_id = self
            .state
            .lock()
            .await
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;

        let should_persist = {
            let mut state = self.state.lock().await;
            state.mark_stream_tool_result_seen(
                internal_call_id,
                &tool_result.id,
                tool_result.call_id.as_deref(),
            )?
        };
        if !should_persist {
            return Ok(());
        }

        let raw_stream_result = render_tool_result_text(tool_result);
        let prefer_stream_payload = is_subagent_tool_result_payload(&raw_stream_result);
        let stored_result =
            match session::load_tool_call_result(&self.node, &session_id, internal_call_id).await {
                Ok(stored) if !stored.is_empty() && !prefer_stream_payload => stored,
                Ok(_) => {
                    let (text, _, _) = truncate_text(
                        &raw_stream_result,
                        TruncationMode::Head,
                        &self.truncation_limits,
                    );
                    text
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        tool_call_id = %internal_call_id,
                        "failed to load stored tool result, falling back to stream payload"
                    );
                    let (text, _, _) = truncate_text(
                        &raw_stream_result,
                        TruncationMode::Head,
                        &self.truncation_limits,
                    );
                    text
                }
            };

        let persisted_result = ToolResult {
            id: tool_result.id.clone(),
            call_id: tool_result.call_id.clone(),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: stored_result,
            })),
        };

        let message = Message::User {
            content: OneOrMany::one(UserContent::ToolResult(persisted_result)),
        };
        self.persist_message(&message).await?;
        Ok(())
    }

    async fn ensure_assistant_turn_sequence(
        &self,
    ) -> anyhow::Result<(String, String, chrono::DateTime<chrono::Utc>, u32)> {
        let mut state = self.state.lock().await;
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
        let request_id = state.current_request_id.clone().unwrap_or_else(|| {
            tracing::warn!(
                "tool call has no active request id; persisting with empty request link"
            );
            String::new()
        });
        let deadline_at = state.request_deadline_at.unwrap_or_else(|| {
            tracing::warn!(
                "tool call has no active request deadline; using default lifecycle deadline"
            );
            chrono::Utc::now() + chrono::Duration::seconds(DEFAULT_DEADLINE_DURATION_SECS as i64)
        });

        let sequence = state.begin_or_continue_assistant_turn();

        Ok((session_id, request_id, deadline_at, sequence))
    }

    async fn persist_spawn_subagent_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, hook_deadline_at, seq) =
            self.ensure_assistant_turn_sequence().await?;
        self.state.lock().await.register_tool_result_identity(
            internal_call_id,
            None,
            tool_call_id.as_deref(),
        );

        let parsed = match serde_json::from_str::<SpawnSubagentArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return self
                    .fail_spawn_subagent_tool_call(
                        session_id,
                        request_id,
                        hook_deadline_at,
                        seq,
                        internal_call_id,
                        args,
                        FailureClass::ArgumentInvalid,
                        invalid_tool_arguments_payload(
                            SPAWN_SUBAGENT_TOOL_NAME,
                            "/",
                            format!("invalid spawn_subagent arguments: {error}"),
                        ),
                    )
                    .await;
            }
        };

        let parent_context = load_parent_subagent_context(&self.node, &request_id).await?;
        if parsed.behavior_id.trim().is_empty() {
            return self
                .fail_spawn_subagent_tool_call(
                    session_id,
                    request_id,
                    parent_context.request_deadline_at,
                    seq,
                    internal_call_id,
                    args,
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/behavior_id",
                        "behavior_id is required",
                    ),
                )
                .await;
        }
        if parsed.prompt.trim().is_empty() {
            return self
                .fail_spawn_subagent_tool_call(
                    session_id,
                    request_id,
                    parent_context.request_deadline_at,
                    seq,
                    internal_call_id,
                    args,
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/prompt",
                        "prompt is required",
                    ),
                )
                .await;
        }
        if !parent_context.subagent_spawn_enabled {
            return self
                .fail_spawn_subagent_tool_call(
                    session_id,
                    request_id,
                    parent_context.request_deadline_at,
                    seq,
                    internal_call_id,
                    args,
                    FailureClass::ServiceUnavailable,
                    tool_not_allowed_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/",
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "subagent spawning is not enabled for this behavior",
                        parent_context.allowed_targets.clone(),
                    ),
                )
                .await;
        }
        let behavior_id = parsed.behavior_id.trim();
        if !target_is_allowed(&parent_context, behavior_id) {
            return self
                .fail_spawn_subagent_tool_call(
                    session_id,
                    request_id,
                    parent_context.request_deadline_at,
                    seq,
                    internal_call_id,
                    args,
                    FailureClass::ServiceUnavailable,
                    tool_not_allowed_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/behavior_id",
                        behavior_id,
                        format!(
                            "behavior '{behavior_id}' is not allowed as a subagent target for this behavior"
                        ),
                        parent_context.allowed_targets.clone(),
                    ),
                )
                .await;
        }

        let await_mode = parsed.await_mode.as_await_mode();
        if await_mode == AwaitMode::Background && !parent_context.subagent_background_enabled {
            return self
                .fail_spawn_subagent_tool_call(
                    session_id,
                    request_id,
                    parent_context.request_deadline_at,
                    seq,
                    internal_call_id,
                    args,
                    FailureClass::ServiceUnavailable,
                    tool_not_allowed_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/await_mode",
                        "background",
                        "background subagent spawning is not enabled for this behavior",
                        parent_context.allowed_targets.clone(),
                    ),
                )
                .await;
        }

        if let Some(child_deadline) = parsed.deadline.as_ref() {
            if *child_deadline > parent_context.request_deadline_at {
                return self
                    .fail_spawn_subagent_tool_call(
                        session_id,
                        request_id,
                        parent_context.request_deadline_at,
                        seq,
                        internal_call_id,
                        args,
                        FailureClass::ArgumentInvalid,
                        invalid_tool_arguments_payload(
                            SPAWN_SUBAGENT_TOOL_NAME,
                            "/deadline",
                            "deadline must be at or before the parent request deadline",
                        ),
                    )
                    .await;
            }
        }

        if parent_context.subagent_depth + 1 > MAX_SUBAGENT_DEPTH {
            return self
                .fail_spawn_subagent_tool_call(
                    session_id,
                    request_id,
                    parent_context.request_deadline_at,
                    seq,
                    internal_call_id,
                    args,
                    FailureClass::ArgumentInvalid,
                    depth_exceeded_payload(parent_context.subagent_depth),
                )
                .await;
        }

        let child_request_id = uuid::Uuid::new_v4().to_string();
        let mut lifecycle = ToolCallLifecycle::new_subagent(
            self.node.clone(),
            request_id.clone(),
            session_id.clone(),
            internal_call_id.to_string(),
            seq,
            SPAWN_SUBAGENT_TOOL_NAME.to_string(),
            args.to_string(),
            parent_context.request_deadline_at,
            await_mode,
            CancelPolicy::Cascade,
            child_request_id.clone(),
        );
        lifecycle.start_running().await?;

        let child_session_id = if let Err(error) = create_subagent_request_with_request_id(
            &self.node,
            child_request_id.clone(),
            parent_context.request_id.clone(),
            internal_call_id.to_string(),
            parent_context.subagent_depth,
            self.agent_did.clone(),
            behavior_id.to_string(),
            parsed.prompt.clone(),
            parsed.deadline,
        )
        .await
        {
            match load_authorized_child_edge(&self.node, &parent_context, &child_request_id).await {
                Ok(edge) if edge.behavior_id == behavior_id => edge.child_session_id,
                Ok(edge) => {
                    let result = service_unavailable_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/behavior_id",
                        format!(
                            "pre-materialized child subagent request has behavior_id {}, expected {behavior_id}",
                            edge.behavior_id
                        ),
                        false,
                    );
                    lifecycle
                        .bridge_failure(ChildTerminal::Failed {
                            reason: result.clone(),
                            failure_class: FailureClass::External,
                        })
                        .await?;
                    return Ok(ToolCallHookAction::skip(result));
                }
                Err(_) => {
                    let result = service_unavailable_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/",
                        format!("failed to materialize child subagent request: {error}"),
                        true,
                    );
                    lifecycle
                        .bridge_failure(ChildTerminal::Failed {
                            reason: result.clone(),
                            failure_class: FailureClass::External,
                        })
                        .await?;
                    return Ok(ToolCallHookAction::skip(result));
                }
            }
        } else if let Some(child_session_id) =
            load_child_session_id(&self.node, &child_request_id).await?
        {
            child_session_id
        } else {
            let result = service_unavailable_payload(
                SPAWN_SUBAGENT_TOOL_NAME,
                "/child_request_id",
                "child subagent request was created without a readable session id",
                true,
            );
            lifecycle
                .bridge_failure(ChildTerminal::Failed {
                    reason: result.clone(),
                    failure_class: FailureClass::External,
                })
                .await?;
            return Ok(ToolCallHookAction::skip(result));
        };

        if await_mode == AwaitMode::Background {
            let receipt =
                background_receipt_payload(&child_request_id, &child_session_id, behavior_id);

            self.in_flight_lifecycles
                .lock()
                .await
                .insert(internal_call_id.to_string(), lifecycle);

            return Ok(ToolCallHookAction::skip(receipt));
        }

        self.in_flight_lifecycles
            .lock()
            .await
            .insert(internal_call_id.to_string(), lifecycle);

        let result = self
            .await_foreground_subagent(
                internal_call_id,
                &parent_context,
                &child_request_id,
                &child_session_id,
                behavior_id,
                parent_context.request_deadline_at,
            )
            .await?;

        Ok(ToolCallHookAction::skip(result))
    }

    async fn await_foreground_subagent(
        &self,
        internal_call_id: &str,
        parent_context: &ParentSubagentContext,
        child_request_id: &str,
        child_session_id: &str,
        behavior_id: &str,
        parent_deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<String> {
        let mut missing_owner_since = None;

        loop {
            let now = chrono::Utc::now();
            let edge =
                load_authorized_child_edge(&self.node, parent_context, child_request_id).await?;

            if edge.lifecycle_state == "cancelled" {
                self.discard_in_flight_lifecycle(internal_call_id).await;
                return Ok(foreground_terminal_failure_payload(
                    child_request_id,
                    child_session_id,
                    "interrupted",
                    "parent request was cancelled while waiting for child subagent",
                    FailureClass::External,
                ));
            }

            if edge.lifecycle_state == "failed" || edge.lifecycle_state == "timedOut" {
                self.discard_in_flight_lifecycle(internal_call_id).await;
                return Ok(foreground_terminal_failure_payload(
                    child_request_id,
                    child_session_id,
                    if edge.lifecycle_state == "timedOut" {
                        "dead"
                    } else {
                        "failed"
                    },
                    "parent subagent bridge reached a terminal failure while waiting for child subagent",
                    FailureClass::External,
                ));
            }

            if edge.lifecycle_state == "completed" {
                self.discard_in_flight_lifecycle(internal_call_id).await;
                return self
                    .foreground_completed_bridge_payload(
                        &parent_context.session_id,
                        internal_call_id,
                        child_request_id,
                        child_session_id,
                        behavior_id,
                    )
                    .await;
            }

            if edge.await_mode == AwaitMode::Background && edge.lifecycle_state == "running" {
                self.refresh_owned_in_flight_lifecycle_from_storage(
                    &parent_context.session_id,
                    internal_call_id,
                )
                .await?;
                return Ok(backgrounded_receipt_payload(
                    child_request_id,
                    child_session_id,
                    behavior_id,
                ));
            }

            if now >= parent_deadline_at {
                if edge.lifecycle_state == "running" {
                    let Some(mut lifecycle) =
                        self.take_owned_in_flight_lifecycle(internal_call_id).await
                    else {
                        wait_for_external_lifecycle_owner(
                            &mut missing_owner_since,
                            now,
                            internal_call_id,
                        )
                        .await?;
                        continue;
                    };
                    if !lifecycle.bridge_failure(ChildTerminal::Dead).await? {
                        return self
                            .foreground_external_bridge_terminal_payload(
                                parent_context,
                                internal_call_id,
                                child_request_id,
                                child_session_id,
                                behavior_id,
                            )
                            .await;
                    }
                }
                return Ok(foreground_terminal_failure_payload(
                    child_request_id,
                    child_session_id,
                    "dead",
                    "parent request deadline exceeded while waiting for child subagent",
                    FailureClass::External,
                ));
            }

            if let Some(row) = load_child_terminal_row(&self.node, child_request_id).await? {
                if child_request_completed(&row) {
                    let Some(final_response) = load_child_final_response(&self.node, &edge).await?
                    else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    if edge.lifecycle_state == "running" {
                        let Some(mut lifecycle) =
                            self.take_owned_in_flight_lifecycle(internal_call_id).await
                        else {
                            wait_for_external_lifecycle_owner(
                                &mut missing_owner_since,
                                now,
                                internal_call_id,
                            )
                            .await?;
                            continue;
                        };
                        if !lifecycle.bridge_complete(final_response.clone()).await? {
                            return self
                                .foreground_external_bridge_terminal_payload(
                                    parent_context,
                                    internal_call_id,
                                    child_request_id,
                                    child_session_id,
                                    behavior_id,
                                )
                                .await;
                        }
                    } else {
                        self.discard_in_flight_lifecycle(internal_call_id).await;
                    }
                    return Ok(json_string(json!({
                        "ok": true,
                        "child_request_id": child_request_id,
                        "child_session_id": child_session_id,
                        "behavior_id": behavior_id,
                        "await_mode": "foreground",
                        "status": "completed",
                        "final_response": final_response,
                        "error": null
                    })));
                }

                if let Some(terminal) = project_child_terminal(&row) {
                    let status = child_terminal_status(&terminal);
                    let (reason, failure_class) = child_terminal_error(&terminal);
                    if edge.lifecycle_state == "running" {
                        let Some(mut lifecycle) =
                            self.take_owned_in_flight_lifecycle(internal_call_id).await
                        else {
                            wait_for_external_lifecycle_owner(
                                &mut missing_owner_since,
                                now,
                                internal_call_id,
                            )
                            .await?;
                            continue;
                        };
                        if !lifecycle.bridge_failure(terminal).await? {
                            return self
                                .foreground_external_bridge_terminal_payload(
                                    parent_context,
                                    internal_call_id,
                                    child_request_id,
                                    child_session_id,
                                    behavior_id,
                                )
                                .await;
                        }
                    } else {
                        self.discard_in_flight_lifecycle(internal_call_id).await;
                    }
                    return Ok(foreground_terminal_failure_payload(
                        child_request_id,
                        child_session_id,
                        status,
                        reason,
                        failure_class,
                    ));
                }
            }

            let remaining = (parent_deadline_at - now)
                .to_std()
                .unwrap_or(Duration::from_millis(0));
            tokio::time::sleep(remaining.min(Duration::from_millis(250))).await;
        }
    }

    async fn foreground_external_bridge_terminal_payload(
        &self,
        parent_context: &ParentSubagentContext,
        internal_call_id: &str,
        child_request_id: &str,
        child_session_id: &str,
        behavior_id: &str,
    ) -> anyhow::Result<String> {
        self.discard_in_flight_lifecycle(internal_call_id).await;
        let edge = load_authorized_child_edge(&self.node, parent_context, child_request_id).await?;

        match edge.lifecycle_state.as_str() {
            "completed" => {
                self.foreground_completed_bridge_payload(
                    &parent_context.session_id,
                    internal_call_id,
                    child_request_id,
                    child_session_id,
                    behavior_id,
                )
                .await
            }
            "cancelled" => Ok(foreground_terminal_failure_payload(
                child_request_id,
                child_session_id,
                "interrupted",
                "parent request was cancelled while waiting for child subagent",
                FailureClass::External,
            )),
            "timedOut" => Ok(foreground_terminal_failure_payload(
                child_request_id,
                child_session_id,
                "dead",
                "parent subagent bridge timed out while waiting for child subagent",
                FailureClass::External,
            )),
            "failed" => Ok(foreground_terminal_failure_payload(
                child_request_id,
                child_session_id,
                "failed",
                "parent subagent bridge reached a terminal failure while waiting for child subagent",
                FailureClass::External,
            )),
            other => anyhow::bail!(
                "spawn_subagent foreground bridge lost running compare but persisted lifecycle_state is {other}"
            ),
        }
    }

    async fn foreground_completed_bridge_payload(
        &self,
        session_id: &str,
        internal_call_id: &str,
        child_request_id: &str,
        child_session_id: &str,
        behavior_id: &str,
    ) -> anyhow::Result<String> {
        let final_response =
            load_tool_call_result(&self.node, session_id, internal_call_id).await?;
        Ok(json_string(json!({
            "ok": true,
            "child_request_id": child_request_id,
            "child_session_id": child_session_id,
            "behavior_id": behavior_id,
            "await_mode": "foreground",
            "status": "completed",
            "final_response": final_response,
            "error": null
        })))
    }

    async fn take_owned_in_flight_lifecycle(
        &self,
        internal_call_id: &str,
    ) -> Option<ToolCallLifecycle> {
        self.in_flight_lifecycles
            .lock()
            .await
            .remove(internal_call_id)
    }

    async fn discard_in_flight_lifecycle(&self, internal_call_id: &str) {
        self.in_flight_lifecycles
            .lock()
            .await
            .remove(internal_call_id);
    }

    async fn refresh_owned_in_flight_lifecycle_from_storage(
        &self,
        session_id: &str,
        internal_call_id: &str,
    ) -> anyhow::Result<()> {
        if !self
            .in_flight_lifecycles
            .lock()
            .await
            .contains_key(internal_call_id)
        {
            return Ok(());
        }

        if let Some(lifecycle) =
            ToolCallLifecycle::load(self.node.clone(), session_id, internal_call_id).await?
        {
            let mut map = self.in_flight_lifecycles.lock().await;
            if map.contains_key(internal_call_id) {
                map.insert(internal_call_id.to_string(), lifecycle);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn fail_spawn_subagent_tool_call(
        &self,
        session_id: String,
        request_id: String,
        deadline_at: chrono::DateTime<chrono::Utc>,
        message_sequence: u32,
        internal_call_id: &str,
        args: &str,
        failure_class: FailureClass,
        result: String,
    ) -> anyhow::Result<ToolCallHookAction> {
        let mut lifecycle = ToolCallLifecycle::new(
            self.node.clone(),
            request_id,
            session_id,
            internal_call_id.to_string(),
            message_sequence,
            SPAWN_SUBAGENT_TOOL_NAME.to_string(),
            args.to_string(),
            deadline_at,
        );
        lifecycle.spawn_failed(failure_class, &result).await?;
        Ok(ToolCallHookAction::skip(result))
    }
}

fn background_receipt_payload(
    child_request_id: &str,
    child_session_id: &str,
    behavior_id: &str,
) -> String {
    json_string(json!({
        "ok": true,
        "child_request_id": child_request_id,
        "child_session_id": child_session_id,
        "behavior_id": behavior_id,
        "await_mode": "background",
        "status": "running"
    }))
}

fn backgrounded_receipt_payload(
    child_request_id: &str,
    child_session_id: &str,
    behavior_id: &str,
) -> String {
    json_string(json!({
        "ok": true,
        "child_request_id": child_request_id,
        "child_session_id": child_session_id,
        "behavior_id": behavior_id,
        "await_mode": "background",
        "status": "running",
        "backgrounded": true
    }))
}

async fn wait_for_external_lifecycle_owner(
    missing_owner_since: &mut Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    internal_call_id: &str,
) -> anyhow::Result<()> {
    let first_missing_at = *missing_owner_since.get_or_insert(now);
    if now - first_missing_at >= chrono::Duration::seconds(5) {
        anyhow::bail!(
            "spawn_subagent foreground wait lost lifecycle ownership for tool_call_id={internal_call_id}"
        );
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(())
}

impl<M: CompletionModel> PromptHook<M> for DefraSessionHook {
    async fn on_completion_call(&self, prompt: &Message, _history: &[Message]) -> HookAction {
        let result: anyhow::Result<()> = async {
            let mut state = self.state.lock().await;

            if !state.initialized {
                let session_id = session::create_session(&self.node, &state.agent_name).await?;
                state.session_id = Some(session_id);
                state.initialized = true;
            }

            state.reset_after_user_message();
            drop(state);

            self.persist_message(prompt).await?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                self.record_success();
                HookAction::Continue
            }
            Err(e) => self.on_persistence_error("persist user prompt", &e),
        }
    }

    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let result: anyhow::Result<()> = async {
            let message = Message::Assistant {
                id: response.message_id.clone(),
                content: response.choice.clone(),
            };
            let sequence = self.persist_message(&message).await?;
            self.mark_current_response_materialized(sequence).await?;
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                self.record_success();
                HookAction::Continue
            }
            Err(e) => self.on_persistence_error("persist assistant response", &e),
        }
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        if tool_name == SPAWN_SUBAGENT_TOOL_NAME {
            let result = self
                .persist_spawn_subagent_tool_call(tool_call_id, internal_call_id, args)
                .instrument(tracing::info_span!(
                    "tool.call",
                    tool_name = %tool_name,
                    tool_call_id = %internal_call_id,
                ))
                .await;

            return match result {
                Ok(action) => {
                    self.record_success();
                    action
                }
                Err(e) => self.on_tool_persistence_error("persist spawn_subagent tool call", &e),
            };
        }

        let result: anyhow::Result<()> = async {
            let (session_id, request_id, deadline_at, seq) =
                self.ensure_assistant_turn_sequence().await?;
            self.state.lock().await.register_tool_result_identity(
                internal_call_id,
                None,
                tool_call_id.as_deref(),
            );

            let mut lc = crate::tool_call_lifecycle::ToolCallLifecycle::new(
                self.node.clone(),
                request_id,
                session_id,
                internal_call_id.to_string(),
                seq,
                tool_name.to_string(),
                args.to_string(),
                deadline_at,
            );
            lc.start_running().await?;

            self.in_flight_lifecycles
                .lock()
                .await
                .insert(internal_call_id.to_string(), lc);

            Ok(())
        }
        .instrument(tracing::info_span!(
            "tool.call",
            tool_name = %tool_name,
            tool_call_id = %internal_call_id,
        ))
        .await;

        match result {
            Ok(()) => {
                self.record_success();
                ToolCallHookAction::Continue
            }
            Err(e) => self.on_tool_persistence_error("persist tool call", &e),
        }
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        result: &str,
    ) -> HookAction {
        let persist_result: anyhow::Result<HookAction> = async {
            if let Some(terminal) = classify_managed_tool_result(result) {
                let lifecycle = self
                    .in_flight_lifecycles
                    .lock()
                    .await
                    .remove(internal_call_id);

                if let Some(mut lc) = lifecycle {
                    match terminal {
                        ManagedToolTerminal::TimedOut => lc.timeout().await?,
                        ManagedToolTerminal::Cancelled => lc.cancel_during_run().await?,
                    }
                } else {
                    tracing::debug!(
                        tool_call_id = %internal_call_id,
                        lifecycle_state = ?terminal,
                        "managed terminal tool result arrived after lifecycle was already swept"
                    );
                }

                let reason = match terminal {
                    ManagedToolTerminal::TimedOut => "tool call deadline exceeded",
                    ManagedToolTerminal::Cancelled => "tool call cancelled",
                };
                return Ok(HookAction::Terminate {
                    reason: reason.to_string(),
                });
            }

            let (session_id, should_persist_message, persisted_result_id, persisted_call_id) = {
                let mut state = self.state.lock().await;
                let session_id = state
                    .session_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
                let should_persist_message = state.mark_tool_result_seen_for_persisted_turn(
                    internal_call_id,
                    None,
                    tool_call_id.as_deref(),
                );
                let (persisted_result_id, persisted_call_id) =
                    state.tool_result_message_identity(internal_call_id, tool_call_id.as_deref());
                (
                    session_id,
                    should_persist_message,
                    persisted_result_id,
                    persisted_call_id,
                )
            };

            let truncator =
                DefraSpillTruncator::new(self.node.clone(), &self.agent_did, &session_id);
            let truncated = truncator
                .truncate(
                    tool_name,
                    args,
                    result,
                    truncation_mode_for(tool_name),
                    &self.truncation_limits,
                    None,
                )
                .await?;

            let mut lc = self
                .in_flight_lifecycles
                .lock()
                .await
                .remove(internal_call_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "on_tool_result: no in-flight lifecycle for tool_call_id={internal_call_id}"
                    )
                })?;

            if let Some(failure_class) = classify_runtime_failure(result) {
                lc.fail(&truncated.text, failure_class).await?;
            } else {
                lc.complete(&truncated.text).await?;
            }

            if should_persist_message {
                let tool_result_message = Message::User {
                    content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                        id: persisted_result_id,
                        call_id: persisted_call_id,
                        content: OneOrMany::one(ToolResultContent::Text(Text {
                            text: truncated.text.clone(),
                        })),
                    })),
                };
                self.persist_message(&tool_result_message).await?;
            }

            Ok(HookAction::Continue)
        }
        .instrument(tracing::info_span!(
            "tool.result",
            tool_name = %tool_name,
            tool_call_id = %internal_call_id,
        ))
        .await;

        match persist_result {
            Ok(action) => {
                self.record_success();
                action
            }
            Err(e) => self.on_persistence_error("persist tool result", &e),
        }
    }
}

fn truncation_mode_for(tool_name: &str) -> TruncationMode {
    match tool_name {
        "bash" | "shell" | "command" => TruncationMode::Tail,
        _ => TruncationMode::Head,
    }
}

fn render_tool_result_text(tool_result: &ToolResult) -> String {
    tool_result
        .content
        .iter()
        .filter_map(|content| match content {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_result_message_key(session_id: &str, message: &Message) -> anyhow::Result<Option<String>> {
    let Message::User { content } = message else {
        return Ok(None);
    };
    if content.len() != 1 {
        return Ok(None);
    }
    let UserContent::ToolResult(tool_result) = content.first_ref() else {
        return Ok(None);
    };

    let Some(logical_id) = non_empty(Some(tool_result.id.as_str()))
        .or_else(|| non_empty(tool_result.call_id.as_deref()))
    else {
        return Ok(None);
    };
    let content_json = serde_json::to_string(&tool_result.content)?;
    Ok(Some(format!(
        "{session_id}:tool-result:{:016x}:{:016x}",
        stable_hash(logical_id.as_bytes()),
        stable_hash(content_json.as_bytes())
    )))
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn is_subagent_tool_result_payload(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    value
        .get("service_id")
        .and_then(|value| value.as_str())
        .is_some_and(|service_id| service_id == "subagent")
        || (value.get("child_request_id").is_some() && value.get("await_mode").is_some())
}

fn json_string(value: serde_json::Value) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

fn child_terminal_status(terminal: &ChildTerminal) -> &'static str {
    match terminal {
        ChildTerminal::Failed { .. } => "failed",
        ChildTerminal::Dead => "dead",
        ChildTerminal::Interrupted => "interrupted",
        ChildTerminal::Superseded => "superseded",
    }
}

fn child_terminal_error(terminal: &ChildTerminal) -> (String, FailureClass) {
    match terminal {
        ChildTerminal::Failed {
            reason,
            failure_class,
        } => (reason.clone(), *failure_class),
        ChildTerminal::Dead => (
            "child request reached terminal state dead".to_string(),
            FailureClass::External,
        ),
        ChildTerminal::Interrupted => (
            "child request was interrupted".to_string(),
            FailureClass::External,
        ),
        ChildTerminal::Superseded => (
            "child request was superseded".to_string(),
            FailureClass::External,
        ),
    }
}

fn foreground_terminal_failure_payload(
    child_request_id: &str,
    child_session_id: &str,
    status: &str,
    reason: impl Into<String>,
    failure_class: FailureClass,
) -> String {
    json_string(json!({
        "ok": false,
        "child_request_id": child_request_id,
        "child_session_id": child_session_id,
        "await_mode": "foreground",
        "status": status,
        "final_response": null,
        "error": {
            "reason": reason.into(),
            "failure_class": failure_class.as_str()
        }
    }))
}

fn invalid_tool_arguments_payload(
    tool_name: &str,
    path: &str,
    message: impl Into<String>,
) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "invalid_tool_arguments",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": "subagent",
        "tool_name": tool_name
    }))
}

fn depth_exceeded_payload(parent_subagent_depth: u32) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "invalid_tool_arguments",
        "code": "subagent_depth_exceeded",
        "path": "/behavior_id",
        "message": "subagent depth ceiling would be exceeded",
        "retryable": false,
        "service_id": "subagent",
        "tool_name": SPAWN_SUBAGENT_TOOL_NAME,
        "parent_subagent_depth": parent_subagent_depth,
        "max_subagent_depth": MAX_SUBAGENT_DEPTH
    }))
}

fn tool_not_allowed_payload(
    tool_name: &str,
    path: &str,
    requested: &str,
    message: impl Into<String>,
    allowed_targets: Vec<String>,
) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "tool_not_allowed",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": "subagent",
        "tool_name": tool_name,
        "requested_tool_name": requested,
        "allowed_subagent_targets": allowed_targets
    }))
}

fn service_unavailable_payload(
    tool_name: &str,
    path: &str,
    message: impl Into<String>,
    retryable: bool,
) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "service_unavailable",
        "path": path,
        "message": message.into(),
        "retryable": retryable,
        "service_id": "subagent",
        "tool_name": tool_name
    }))
}

/// Classify a runtime error string into a FailureClass. Defaults to
/// ToolReturnedError for unknown shapes; managed timeout/cancel markers are
/// handled before this helper so terminal outcomes stay distinct.
#[allow(dead_code)]
fn classify_runtime_error(err: &str) -> crate::tool_call_lifecycle::FailureClass {
    use crate::tool_call_lifecycle::FailureClass;
    if err.contains("timeout") || err.contains("deadline") {
        FailureClass::External // R3 will reroute to lifecycle.timeout()
    } else if err.contains("invalid argument") || err.contains("parse") {
        FailureClass::ArgumentInvalid
    } else if err.contains("unavailable") || err.contains("not found") {
        FailureClass::ServiceUnavailable
    } else if err.contains("transport") || err.contains("connection") {
        FailureClass::Transport
    } else {
        FailureClass::ToolReturnedError
    }
}

fn classify_runtime_failure(result: &str) -> Option<crate::tool_call_lifecycle::FailureClass> {
    if result.starts_with("JsonError:") {
        return Some(crate::tool_call_lifecycle::FailureClass::ArgumentInvalid);
    }
    if result.starts_with("ToolCallError:") {
        return Some(classify_runtime_error(result));
    }
    None
}
