use super::*;

impl DefraSessionHook {
    pub async fn persist_message(&self, message: &Message) -> anyhow::Result<u32> {
        let content = serde_json::to_string(message)?;
        let (session_id, turn_state, message_key, existing_sequence) = {
            let state = self.state.lock().await;
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
            let message_key = tool_result_message_key(&session_id, message)?;
            let existing_sequence = message_key
                .as_ref()
                .and_then(|key| state.persisted_tool_result_message_sequences.get(key))
                .copied();
            (
                session_id,
                state.transcript_turn,
                message_key,
                existing_sequence,
            )
        };

        if let Some(sequence) = existing_sequence {
            return Ok(sequence);
        }

        if matches!(turn_state, TranscriptTurnState::Idle) {
            let role = match message {
                Message::User { .. } => "user",
                Message::Assistant { .. } => "assistant",
                Message::System { .. } => {
                    anyhow::bail!("system messages are not persisted in session history");
                }
            };
            let sequence = session::append_message(&self.node, &session_id, role, &content).await?;
            let mut state = self.state.lock().await;
            if state.session_id.as_deref() == Some(session_id.as_str()) {
                state.sequence = state.sequence.max(sequence);
                if let Some(key) = message_key {
                    state
                        .persisted_tool_result_message_sequences
                        .insert(key, sequence);
                }
                match message {
                    Message::User { .. } => state.reset_after_user_message(),
                    Message::Assistant { .. } => {
                        state.transcript_turn =
                            TranscriptTurnState::AssistantPersisted { sequence };
                    }
                    Message::System { .. } => {}
                }
            }
            return Ok(sequence);
        }

        let (session_id, sequence, role, message_key) = {
            let mut state = self.state.lock().await;
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
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
        let (tool_name, stored_result) =
            match load_stored_tool_call_result(&self.node, &session_id, internal_call_id).await {
                Ok(stored) if !stored.result.is_empty() && !prefer_stream_payload => {
                    (stored.tool_name, stored.result)
                }
                Ok(stored) => {
                    let (text, _, _) = truncate_text(
                        &raw_stream_result,
                        TruncationMode::Head,
                        &self.truncation_limits,
                    );
                    (stored.tool_name, text)
                }
                Err(e) => {
                    if is_missing_tool_call_result(&e) {
                        tracing::debug!(
                            error = %e,
                            tool_call_id = %internal_call_id,
                            "stored tool result not found, falling back to stream payload"
                        );
                    } else {
                        tracing::warn!(
                            error = %e,
                            tool_call_id = %internal_call_id,
                            "failed to load stored tool result, falling back to stream payload"
                        );
                    }
                    let (text, _, _) = truncate_text(
                        &raw_stream_result,
                        TruncationMode::Head,
                        &self.truncation_limits,
                    );
                    ("unknown".to_string(), text)
                }
            };
        let model_observation = model_observation_for_tool_result(&tool_name, &stored_result);

        let persisted_result = ToolResult {
            id: tool_result.id.clone(),
            call_id: tool_result.call_id.clone(),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: model_observation,
            })),
        };

        let message = Message::User {
            content: OneOrMany::one(UserContent::ToolResult(persisted_result)),
        };
        self.persist_message(&message).await?;
        Ok(())
    }

    pub(super) async fn ensure_assistant_turn_sequence(
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

    pub(super) async fn persist_spawn_subagent_tool_call(
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
        if parsed.name.trim().is_empty() {
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
                        "/name",
                        "name is required",
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
                        context_allowed_target_names(&parent_context),
                    ),
                )
                .await;
        }
        let name = parsed.name.trim();
        let Some(target) = resolve_context_target(&parent_context, name).cloned() else {
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
                        "/name",
                        name,
                        format!("'{name}' is not an allowed subagent target for this behavior"),
                        context_allowed_target_names(&parent_context),
                    ),
                )
                .await;
        };
        let behavior_id = target.behavior_id.as_str();

        let await_mode = parsed
            .await_mode
            .map(|mode| mode.as_await_mode())
            .unwrap_or(parent_context.subagent_default_await_mode);
        let target_host = self.subagent_target_host(&target);
        // Cross-deployment (remote-DID) subagent delegation is deferred behind a
        // default-OFF flag (#377). When the parent behavior has not opted in,
        // reject ANY remote spawn (both await modes). Remote targets should not
        // even be surfaced to the model in this case (see tool_surface), so a
        // remote spawn here means a stale/forged target name.
        if target_host == SubagentTargetHost::Remote
            && !parent_context.subagent_allow_cross_deployment
        {
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
                        "/name",
                        name,
                        "cross-deployment subagent delegation is not enabled",
                        context_allowed_target_names(&parent_context),
                    ),
                )
                .await;
        }
        if target_host == SubagentTargetHost::Remote && await_mode == AwaitMode::Foreground {
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
                        "/await_mode",
                        "foreground cross-deployment subagents are not supported; use await_mode=background",
                    ),
                )
                .await;
        }
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
                        context_allowed_target_names(&parent_context),
                    ),
                )
                .await;
        }

        // Fail-safe for local targets whose behavior was deleted mid-session
        // (#377). If the resolved target is LOCAL (same agent DID) but its
        // behavior no longer exists in the DB, writing a child AgentRequest
        // would produce an orphan that can never be claimed. Reject cleanly
        // with a service_unavailable payload instead of writing the orphan.
        if target_host == SubagentTargetHost::Local {
            match load_agent_behavior(&self.node, behavior_id).await {
                Ok(None) => {
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
                                "/name",
                                name,
                                format!(
                                    "subagent target '{name}' refers to behavior '{behavior_id}' \
                                     which no longer exists; the target may have been removed \
                                     after this session started"
                                ),
                                context_allowed_target_names(&parent_context),
                            ),
                        )
                        .await;
                }
                Ok(Some(_)) => {}
                Err(error) => {
                    tracing::warn!(
                        behavior_id = %behavior_id,
                        %error,
                        "spawn guard: failed to verify local target behavior existence; \
                         proceeding with spawn"
                    );
                }
            }
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

        // Persist a normalized bridge args payload that carries the RESOLVED
        // target `(agent_did, behavior_id)` alongside the model-facing `name`.
        // `SubagentSource` reads these resolved fields directly, so the child
        // `AgentRequest` is written with the TARGET's agent_did + behavior_id
        // -- for a remote target this is the remote DID, and out-of-band
        // replication carries the child to the owning node. The claiming
        // deployment never needs to re-resolve the friendly name (it has no
        // access to the parent's target table), which is what removes the
        // resolution seam.
        let bridge_args = serde_json::json!({
            "name": name,
            "agent_did": target.agent_did,
            "behavior_id": target.behavior_id,
            "prompt": parsed.prompt,
            "deadline": parsed.deadline,
        })
        .to_string();

        let child_request_id = uuid::Uuid::new_v4().to_string();
        let mut lifecycle = ToolCallLifecycle::new_subagent(
            self.node.clone(),
            request_id.clone(),
            session_id.clone(),
            internal_call_id.to_string(),
            seq,
            SPAWN_SUBAGENT_TOOL_NAME.to_string(),
            bridge_args,
            parent_context.request_deadline_at,
            await_mode,
            CancelPolicy::Cascade,
            child_request_id.clone(),
        );
        if await_mode == AwaitMode::Background {
            let timeout_secs =
                effective_context_cross_deployment_spawn_timeout_seconds(&parent_context);
            lifecycle.set_unclaimed_deadline_at(Some(
                chrono::Utc::now() + chrono::Duration::seconds(timeout_secs),
            ));
        }
        lifecycle.start_running().await?;

        // Spawn convergence (#377): both same-deployment (local) and
        // cross-deployment (remote) spawns now follow ONE path — write the
        // `AgentToolCall` bridge (done by `start_running()` above) and let
        // `SubagentSource` create the child `AgentRequest`. `SubagentSource`
        // dedups via `child_request_exists`, so there is exactly one creator
        // regardless of locality. The hook no longer synchronously creates the
        // child, so the background receipt does not yet carry the child session
        // id (the claiming deployment assigns it when it materializes the
        // child); foreground waits adopt the session id from the edge once
        // `SubagentSource` has materialized the child.
        self.in_flight_lifecycles
            .lock()
            .await
            .insert(internal_call_id.to_string(), lifecycle);

        if await_mode == AwaitMode::Background {
            let receipt = background_receipt_payload(&child_request_id, None, behavior_id);
            return Ok(self.skip_tool_result(SPAWN_SUBAGENT_TOOL_NAME, receipt));
        }

        // Foreground spawns are local-only (the remote-foreground case is
        // rejected above). Block until `SubagentSource` materializes the child
        // and the bridge reaches a terminal state.
        let result = self
            .await_foreground_subagent(
                internal_call_id,
                &parent_context,
                &child_request_id,
                "",
                behavior_id,
                parent_context.request_deadline_at,
            )
            .await?;

        Ok(self.skip_tool_result(SPAWN_SUBAGENT_TOOL_NAME, result))
    }

    /// Classify a resolved target as local or remote by comparing the target's
    /// `agent_did` to this deployment's own DID. No behavior DB lookup is
    /// needed: the target carries the owning agent's DID directly, which is
    /// also what removes the cross-node resolution seam.
    pub(super) fn subagent_target_host(&self, target: &SubagentTarget) -> SubagentTargetHost {
        if target.agent_did == self.agent_did {
            SubagentTargetHost::Local
        } else {
            SubagentTargetHost::Remote
        }
    }
}

fn is_missing_tool_call_result(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("no AgentToolCall")
}
