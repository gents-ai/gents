use super::*;
use anyhow::Context;

impl DefraSessionHook {
    /// Prepare immutable bridge provenance before the provider turn creates
    /// its pending ToolCall row. A plan is deliberately optional for malformed
    /// or disallowed provider intent, which retains a dispatch-time diagnostic.
    /// Configuration/read failures instead fail the request before publication.
    pub(crate) async fn preplan_spawn_admissions(
        &self,
        message: &Message,
        internal_call_ids: &[String],
    ) -> anyhow::Result<Vec<crate::streaming::SpawnAdmissionPlan>> {
        let Message::Assistant { content, .. } = message else {
            return Ok(Vec::new());
        };
        let tool_calls = content
            .iter()
            .filter_map(|block| match block {
                crate::llm::message::AssistantContent::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect::<Vec<_>>();
        if tool_calls.len() != internal_call_ids.len() {
            return Ok(Vec::new());
        }
        let candidates = tool_calls
            .into_iter()
            .filter_map(|call| {
                if call.function.name != SPAWN_SUBAGENT_TOOL_NAME {
                    return None;
                }
                serde_json::from_value::<SpawnSubagentArgs>(call.function.arguments.clone())
                    .ok()
                    .filter(|parsed| {
                        !parsed.name.trim().is_empty() && !parsed.prompt.trim().is_empty()
                    })
                    .map(|parsed| (call, parsed))
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let request_id = {
            let state = self.state.lock().await;
            state.current_request_id.clone()
        };
        let Some(request_id) = request_id else {
            return Ok(Vec::new());
        };
        let parent = load_parent_subagent_context(&self.node, &request_id)
            .await
            .with_context(|| format!("preplan spawn admission for parent request {request_id}"))?;
        let delegated_workspace = parent_delegated_workspace(&parent).with_context(|| {
            format!("preplan spawn admission for parent request {request_id}: invalid workspace provenance")
        })?;
        let mut plans = Vec::new();
        for (call, parsed) in candidates {
            if !parent.subagent_spawn_enabled || parent.subagent_depth >= MAX_SUBAGENT_DEPTH {
                continue;
            }
            let Some(target) = resolve_context_target(&parent, parsed.name.trim()) else {
                continue;
            };
            let await_mode = parsed
                .await_mode
                .map(|mode| mode.as_await_mode())
                .unwrap_or(parent.subagent_default_await_mode);
            let host = self.subagent_target_host(target);
            if (host == SubagentTargetHost::Remote && !parent.subagent_allow_cross_deployment)
                || (host == SubagentTargetHost::Remote && await_mode == AwaitMode::Foreground)
                || (await_mode == AwaitMode::Background && !parent.subagent_background_enabled)
                || parsed.deadline.is_some_and(|deadline| {
                    deadline <= chrono::Utc::now() || deadline > parent.request_deadline_at
                })
            {
                continue;
            }
            if host == SubagentTargetHost::Local {
                let behavior = load_agent_behavior(&self.node, &target.behavior_id)
                    .await
                    .with_context(|| {
                        format!(
                            "preplan spawn admission for parent request {request_id}: read target behavior {}",
                            target.behavior_id
                        )
                    })?;
                if behavior.is_none() {
                    continue;
                }
            }
            plans.push(crate::streaming::SpawnAdmissionPlan {
                tool_call_id: call.id.clone(),
                child_request_id: uuid::Uuid::new_v4().to_string(),
                spawn_target_did: target.target_agent_did.clone(),
                spawn_behavior_id: target.behavior_id.clone(),
                delegated_workspace: delegated_workspace.clone(),
                await_mode,
            });
        }
        Ok(plans)
    }

    pub(super) async fn ensure_assistant_turn_sequence(
        &self,
    ) -> anyhow::Result<(String, String, chrono::DateTime<chrono::Utc>, u32)> {
        let mut state = self.state.lock().await;
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
        let request_id = state
            .current_request_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("tool call is missing its active request id"))?;
        if state.current_request_doc_id.is_none() {
            anyhow::bail!("tool call is missing its active request document id");
        }
        let deadline_at = state
            .request_deadline_at
            .ok_or_else(|| anyhow::anyhow!("tool call is missing its request deadline"))?;

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
                    return Err(error).with_context(|| {
                        format!("spawn guard: failed to verify local target behavior {behavior_id}")
                    })
                }
            }
        }

        if let Some(child_deadline) = parsed.deadline.as_ref() {
            if *child_deadline <= chrono::Utc::now() {
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
                            "deadline must be in the future",
                        ),
                    )
                    .await;
            }
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

        if parent_context.subagent_depth >= MAX_SUBAGENT_DEPTH {
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

        let parent_workspace = ParentWorkspaceStamp::from_fields(
            &self.agent_did,
            parent_context.workspace_id.as_deref(),
            parent_context.workspace_owner_agent_did.as_deref(),
            parent_context.workspace_authority.as_deref(),
            parent_context.workspace_seal_hash.as_deref(),
        );
        let _resolved_workspace = match resolve_spawn_workspace(
            &self.node,
            &parent_workspace,
            parsed.workspace.as_ref(),
            &target.target_agent_did,
            internal_call_id,
            &request_id,
            self.operator_tool_root.as_deref(),
        )
        .await
        {
            Ok(lineage) => lineage,
            Err(SpawnWorkspaceError { class, message }) => {
                let payload = match class {
                    FailureClass::ArgumentInvalid => invalid_tool_arguments_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/workspace",
                        message,
                    ),
                    _ => service_unavailable_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/workspace",
                        message,
                        false,
                    ),
                };
                return self
                    .fail_spawn_subagent_tool_call(
                        session_id,
                        request_id,
                        parent_context.request_deadline_at,
                        seq,
                        internal_call_id,
                        args,
                        class,
                        payload,
                    )
                    .await;
            }
        };

        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                SPAWN_SUBAGENT_TOOL_NAME,
                args,
                parent_context.request_deadline_at,
                await_mode,
                CancelPolicy::Cascade,
            )
            .await?;
        let child_request_id = lifecycle.child_request_id.clone().context(
            "valid spawn_subagent dispatch lacks immutable child admission prepared at provider publication",
        )?;
        anyhow::ensure!(
            lifecycle.spawn_target_did.as_deref() == Some(target.target_agent_did.as_str()),
            "spawn_subagent dispatch target conflicts with immutable accepted bridge admission"
        );
        anyhow::ensure!(
            lifecycle.spawn_behavior_id.as_deref() == Some(target.behavior_id.as_str()),
            "spawn_subagent dispatch behavior conflicts with immutable accepted bridge admission"
        );
        if await_mode == AwaitMode::Background {
            let timeout_secs =
                effective_context_cross_deployment_spawn_timeout_seconds(&parent_context);
            lifecycle.set_unclaimed_deadline_at(Some(
                chrono::Utc::now() + chrono::Duration::seconds(timeout_secs),
            ));
        }
        let bridge_doc_id = lifecycle.doc_id().unwrap_or_default().to_owned();
        let bridge_generation = lifecycle
            .execution_generation()
            .unwrap_or_default()
            .to_owned();
        let bridge_request_doc_id = lifecycle.request_doc_id().unwrap_or_default().to_owned();
        let bridge_requester_did = lifecycle.requester_did().map(str::to_owned);
        let parked_reservation = if await_mode == AwaitMode::Foreground {
            if crate::agent::worker_capacity::current_slot_capacity().is_some() {
                anyhow::ensure!(
                    !bridge_doc_id.is_empty()
                        && !bridge_generation.is_empty()
                        && !bridge_request_doc_id.is_empty(),
                    "accepted foreground bridge lacks physical or generation identity"
                );
            }
            match crate::agent::worker_capacity::reserve_current_park(&bridge_doc_id) {
                Ok(reservation) => reservation,
                Err(error) => {
                    let result = service_unavailable_payload(
                        SPAWN_SUBAGENT_TOOL_NAME,
                        "/await_mode",
                        format!("foreground continuation capacity unavailable: {error}"),
                        false,
                    );
                    if let Err(failure) = lifecycle
                        .spawn_failed(FailureClass::ServiceUnavailable, &result)
                        .await
                    {
                        return Ok(ToolCallHookAction::Terminate {
                            reason: format!("foreground spawn capacity refusal could not terminalize accepted bridge: {failure:#}"),
                        });
                    }
                    if matches!(
                        error,
                        crate::agent::worker_capacity::CapacityError::ParkedFull
                    ) {
                        return Ok(self.skip_tool_result(SPAWN_SUBAGENT_TOOL_NAME, result));
                    }
                    return Ok(ToolCallHookAction::Terminate {
                        reason: format!("foreground spawn capacity owner refused: {error}"),
                    });
                }
            }
        } else {
            None
        };
        lifecycle.start_running().await?;

        if await_mode == AwaitMode::Background {
            let receipt = background_receipt_payload(&child_request_id, None, behavior_id);
            lifecycle.publish_background_receipt(&receipt).await?;
            self.in_flight_lifecycles
                .lock()
                .await
                .insert(internal_call_id.to_string(), lifecycle);
            return Ok(self.skip_tool_result(SPAWN_SUBAGENT_TOOL_NAME, receipt));
        }
        if !lifecycle.is_running() {
            return Ok(ToolCallHookAction::Terminate {
                reason: "foreground spawn bridge did not enter running state".to_owned(),
            });
        }
        let parked = match crate::agent::worker_capacity::park_current(parked_reservation) {
            Ok(parked) => parked,
            Err(error) => {
                return Ok(ToolCallHookAction::Terminate {
                    reason: format!("foreground spawn could not park its continuation: {error}"),
                });
            }
        };

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
            .await;
        if parked {
            let resumed = self
                .resume_foreground_worker(
                    &request_id,
                    &session_id,
                    &bridge_request_doc_id,
                    &bridge_generation,
                    &bridge_doc_id,
                    &bridge_request_doc_id,
                    &child_request_id,
                    bridge_requester_did.as_deref(),
                    None,
                )
                .await;
            if let Err(error) = resumed {
                return Ok(ToolCallHookAction::Terminate {
                    reason: format!(
                        "foreground spawn continuation owner refused resume: {error:#}"
                    ),
                });
            }
        }
        let result = match result {
            Ok(result) => result,
            Err(error) if parked => {
                return Ok(ToolCallHookAction::Terminate {
                    reason: format!("parked foreground spawn wait failed: {error:#}"),
                });
            }
            Err(error) => return Err(error),
        };

        Ok(self.skip_tool_result(SPAWN_SUBAGENT_TOOL_NAME, result))
    }

    /// Classify a resolved target as local or remote by comparing the target's
    /// destination principal to this runtime principal. Configuration ownership
    /// does not determine the destination.
    pub(super) fn subagent_target_host(
        &self,
        target: &SubagentTargetDocument,
    ) -> SubagentTargetHost {
        if target.target_agent_did == self.agent_did {
            SubagentTargetHost::Local
        } else {
            SubagentTargetHost::Remote
        }
    }
}

fn parent_delegated_workspace(
    parent: &crate::background_tools::ParentSubagentContext,
) -> anyhow::Result<Option<gents_protocol::output::DelegatedWorkspace>> {
    let lineage = crate::lifecycle::WorkspaceLineage {
        workspace_id: parent.workspace_id.clone(),
        workspace_owner_agent_did: parent.workspace_owner_agent_did.clone(),
        workspace_authority: parent.workspace_authority.clone(),
        workspace_seal_hash: parent.workspace_seal_hash.clone(),
    };
    lineage.require_authority_if_workspace_id()?;
    Ok(lineage
        .workspace_id
        .map(|workspace_id| gents_protocol::output::DelegatedWorkspace {
            workspace_id,
            workspace_owner_agent_did: lineage
                .workspace_owner_agent_did
                .expect("validated workspace owner"),
            workspace_authority: lineage
                .workspace_authority
                .expect("validated workspace authority"),
            workspace_seal_hash: lineage.workspace_seal_hash,
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_parent_workspace_provenance_is_an_error_not_absent_delegation() {
        let mut parent = crate::background_tools::ParentSubagentContext {
            session_id: "session".into(),
            request_id: "request".into(),
            request_doc_id: "request-doc".into(),
            behavior_id: "behavior".into(),
            subagent_depth: 0,
            request_deadline_at: chrono::Utc::now(),
            allowed_targets: Vec::new(),
            subagent_spawn_enabled: true,
            subagent_background_enabled: false,
            subagent_default_await_mode: AwaitMode::Foreground,
            subagent_allow_cross_deployment: false,
            cross_deployment_spawn_timeout_seconds: None,
            workspace_id: Some("workspace".into()),
            workspace_authority: None,
            workspace_owner_agent_did: None,
            workspace_seal_hash: None,
        };
        assert!(parent_delegated_workspace(&parent).is_err());
        parent.workspace_id = None;
        assert!(parent_delegated_workspace(&parent).unwrap().is_none());
    }
}
