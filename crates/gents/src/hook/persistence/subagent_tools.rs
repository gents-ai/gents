use super::*;
use anyhow::Context;

impl DefraSessionHook {
    pub(super) async fn persist_wait_subagent_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, parent_deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                WAIT_SUBAGENT_TOOL_NAME,
                args,
                parent_deadline_at,
                AwaitMode::Foreground,
                crate::tool_call_lifecycle::CancelPolicy::Cascade,
            )
            .await?;
        lifecycle.start_running().await?;

        let parsed = match serde_json::from_str::<WaitSubagentArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return self
                    .complete_control_tool_call(
                        &mut lifecycle,
                        WAIT_SUBAGENT_TOOL_NAME,
                        invalid_tool_arguments_payload(
                            WAIT_SUBAGENT_TOOL_NAME,
                            "/",
                            format!("invalid wait_subagent arguments: {error}"),
                        ),
                    )
                    .await;
            }
        };
        let child_request_id = parsed.child_request_id.trim();
        if child_request_id.is_empty() {
            return self
                .complete_control_tool_call(
                    &mut lifecycle,
                    WAIT_SUBAGENT_TOOL_NAME,
                    invalid_tool_arguments_payload(
                        WAIT_SUBAGENT_TOOL_NAME,
                        "/child_request_id",
                        "child_request_id is required",
                    ),
                )
                .await;
        }

        let Some(canonical) = crate::descendant_graph::resolve_session_descendant_edge(
            DescendantGraphAccess::Local(&self.node),
            &request_id,
            child_request_id,
        )
        .await?
        else {
            let result = service_unavailable_payload(
                WAIT_SUBAGENT_TOOL_NAME,
                "/child_request_id",
                "child subagent request is not available to this session principal",
                false,
            );
            return self
                .complete_control_tool_call(&mut lifecycle, WAIT_SUBAGENT_TOOL_NAME, result)
                .await;
        };
        if !canonical.readable() {
            let result = service_unavailable_payload(
                WAIT_SUBAGENT_TOOL_NAME,
                "/child_request_id",
                canonical.diagnostic.clone().unwrap_or_else(|| {
                    format!("child request {child_request_id} is not materialized")
                }),
                canonical.retryable(),
            );
            return self
                .complete_control_tool_call(&mut lifecycle, WAIT_SUBAGENT_TOOL_NAME, result)
                .await;
        }
        if !canonical.controllable() {
            let result = tool_not_allowed_payload(
                WAIT_SUBAGENT_TOOL_NAME,
                "/child_request_id",
                child_request_id,
                "descendant is visible but control belongs to its immediate parent session principal",
                Vec::new(),
            );
            return self
                .complete_control_tool_call(&mut lifecycle, WAIT_SUBAGENT_TOOL_NAME, result)
                .await;
        }
        let edge = ChildEdge::from_descendant(&canonical).ok_or_else(|| {
            anyhow::anyhow!("authorized descendant edge lacks materialized child identity")
        })?;
        let parent_context =
            load_parent_subagent_context(&self.node, &edge.parent_request_id).await?;

        let current_request_doc_id = lifecycle.request_doc_id().unwrap_or_default().to_owned();
        let current_generation = lifecycle
            .execution_generation()
            .unwrap_or_default()
            .to_owned();
        let current_control_doc_id = lifecycle.doc_id().unwrap_or_default().to_owned();
        let current_requester_did = lifecycle.requester_did().map(str::to_owned);
        let reservation = if edge.lifecycle_state == "running" {
            if crate::agent::worker_capacity::current_slot_capacity().is_some() {
                anyhow::ensure!(
                    !current_request_doc_id.is_empty()
                        && !current_generation.is_empty()
                        && !current_control_doc_id.is_empty(),
                    "accepted wait control lacks physical or generation identity"
                );
            }
            match crate::agent::worker_capacity::reserve_current_park(&edge.parent_tool_call_doc_id)
            {
                Ok(reservation) => reservation,
                Err(error) => {
                    let unavailable = service_unavailable_payload(
                        WAIT_SUBAGENT_TOOL_NAME,
                        "/child_request_id",
                        format!("foreground continuation capacity unavailable: {error}"),
                        false,
                    );
                    let completed = self
                        .complete_control_tool_call(
                            &mut lifecycle,
                            WAIT_SUBAGENT_TOOL_NAME,
                            unavailable,
                        )
                        .await;
                    let completed = match completed {
                        Ok(action) => action,
                        Err(failure) => {
                            return Ok(ToolCallHookAction::Terminate {
                                reason: format!("wait capacity refusal could not complete accepted control: {failure:#}"),
                            });
                        }
                    };
                    if matches!(
                        error,
                        crate::agent::worker_capacity::CapacityError::ParkedFull
                    ) {
                        return Ok(completed);
                    }
                    return Ok(ToolCallHookAction::Terminate {
                        reason: format!("wait continuation capacity owner refused: {error}"),
                    });
                }
            }
        } else {
            None
        };

        if edge.lifecycle_state == "running" {
            if edge.await_mode == AwaitMode::Background {
                self.foreground_and_track_existing_subagent_bridge(
                    &parent_context,
                    child_request_id,
                    &edge.parent_tool_call_id,
                )
                .await?;
            } else {
                self.track_in_flight_lifecycle_from_storage(
                    &parent_context.session_id,
                    &edge.parent_tool_call_id,
                )
                .await?;
            }
        }

        let parked = if reservation.is_some() {
            let bridge = ToolCallLifecycle::load_by_doc_id(
                self.node.clone(),
                &edge.parent_tool_call_doc_id,
                &edge.parent_agent_did,
                &edge.parent_session_id,
                edge.parent_requester_did.as_deref(),
            )
            .await?
            .context("authorized wait bridge disappeared before parking")?;
            if bridge.is_running()
                && bridge.request_doc_id() == Some(parent_context.request_doc_id.as_str())
                && bridge.child_request_id.as_deref() == Some(edge.child_request_id.as_str())
            {
                match crate::agent::worker_capacity::park_current(reservation) {
                    Ok(parked) => parked,
                    Err(error) => {
                        return Ok(ToolCallHookAction::Terminate {
                            reason: format!(
                                "existing-child wait could not park its continuation: {error}"
                            ),
                        });
                    }
                }
            } else {
                false
            }
        } else {
            false
        };

        let result = self
            .await_existing_subagent_bridge(
                &parent_context,
                &current_request_doc_id,
                &edge.parent_tool_call_id,
                &edge.child_request_id,
                &edge.child_session_id,
                &edge.behavior_id,
                parent_deadline_at,
            )
            .await;

        if parked {
            if let Err(error) = self
                .resume_foreground_worker(
                    &request_id,
                    &session_id,
                    &current_request_doc_id,
                    &current_generation,
                    &edge.parent_tool_call_doc_id,
                    &parent_context.request_doc_id,
                    &edge.child_request_id,
                    current_requester_did.as_deref(),
                    Some(&current_control_doc_id),
                )
                .await
            {
                return Ok(ToolCallHookAction::Terminate {
                    reason: format!(
                        "existing-child wait continuation owner refused resume: {error:#}"
                    ),
                });
            }
        }
        let result = match result {
            Ok(result) => result,
            Err(error) if parked => {
                return Ok(ToolCallHookAction::Terminate {
                    reason: format!("parked existing-child wait failed: {error:#}"),
                });
            }
            Err(error) => return Err(error),
        };

        self.complete_control_tool_call(&mut lifecycle, WAIT_SUBAGENT_TOOL_NAME, result)
            .await
    }

    pub(super) async fn persist_list_subagents_tool_call(
        &self,
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
                LIST_SUBAGENTS_TOOL_NAME,
                args,
                deadline_at,
                AwaitMode::Foreground,
                crate::tool_call_lifecycle::CancelPolicy::Cascade,
            )
            .await?;
        lifecycle.start_running().await?;

        let parsed = match serde_json::from_str::<ListSubagentsArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return self
                    .complete_control_tool_call(
                        &mut lifecycle,
                        LIST_SUBAGENTS_TOOL_NAME,
                        invalid_tool_arguments_payload(
                            LIST_SUBAGENTS_TOOL_NAME,
                            "/",
                            format!("invalid list_subagents arguments: {error}"),
                        ),
                    )
                    .await;
            }
        };
        let response = handle_list_subagents(&self.node, &request_id, parsed).await?;
        let result = serde_json::to_value(response)
            .map_err(|error| anyhow::anyhow!("serialize list_subagents response: {error}"))?;
        self.complete_control_tool_call(
            &mut lifecycle,
            LIST_SUBAGENTS_TOOL_NAME,
            json_string(result),
        )
        .await
    }

    pub(super) async fn persist_read_subagent_tool_call(
        &self,
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
                READ_SUBAGENT_TOOL_NAME,
                args,
                deadline_at,
                AwaitMode::Foreground,
                crate::tool_call_lifecycle::CancelPolicy::Cascade,
            )
            .await?;
        lifecycle.start_running().await?;

        let parsed = match serde_json::from_str::<ReadSubagentArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return self
                    .complete_control_tool_call(
                        &mut lifecycle,
                        READ_SUBAGENT_TOOL_NAME,
                        invalid_tool_arguments_payload(
                            READ_SUBAGENT_TOOL_NAME,
                            "/",
                            format!("invalid read_subagent arguments: {error}"),
                        ),
                    )
                    .await;
            }
        };
        let child_request_id = parsed.child_request_id.trim().to_string();
        if child_request_id.is_empty() {
            return self
                .complete_control_tool_call(
                    &mut lifecycle,
                    READ_SUBAGENT_TOOL_NAME,
                    invalid_tool_arguments_payload(
                        READ_SUBAGENT_TOOL_NAME,
                        "/child_request_id",
                        "child_request_id is required",
                    ),
                )
                .await;
        }

        let Some(response) = handle_read_subagent(&self.node, &request_id, parsed).await? else {
            return self
                .complete_control_tool_call(
                    &mut lifecycle,
                    READ_SUBAGENT_TOOL_NAME,
                    tool_not_allowed_payload(
                        READ_SUBAGENT_TOOL_NAME,
                        "/child_request_id",
                        &child_request_id,
                        "child is not a background subagent owned by this parent request",
                        Vec::new(),
                    ),
                )
                .await;
        };
        let result = serde_json::to_value(response)
            .map_err(|error| anyhow::anyhow!("serialize read_subagent response: {error}"))?;
        self.complete_control_tool_call(
            &mut lifecycle,
            READ_SUBAGENT_TOOL_NAME,
            json_string(result),
        )
        .await
    }

    pub(super) async fn persist_steer_subagent_tool_call(
        &self,
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
                STEER_SUBAGENT_TOOL_NAME,
                args,
                deadline_at,
                AwaitMode::Foreground,
                crate::tool_call_lifecycle::CancelPolicy::Cascade,
            )
            .await?;
        lifecycle.start_running().await?;
        macro_rules! finish {
            ($result:expr) => {
                return self
                    .complete_control_tool_call(&mut lifecycle, STEER_SUBAGENT_TOOL_NAME, $result)
                    .await
            };
        }

        let parsed = match serde_json::from_str::<SteerSubagentArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                finish!(invalid_tool_arguments_payload(
                    STEER_SUBAGENT_TOOL_NAME,
                    "/",
                    format!("invalid steer_subagent arguments: {error}"),
                ));
            }
        };
        let child_request_id = parsed.child_request_id.trim().to_string();
        if child_request_id.is_empty() {
            finish!(invalid_tool_arguments_payload(
                STEER_SUBAGENT_TOOL_NAME,
                "/child_request_id",
                "child_request_id is required",
            ));
        }
        let message = parsed.message.trim().to_string();
        if message.is_empty() {
            finish!(invalid_tool_arguments_payload(
                STEER_SUBAGENT_TOOL_NAME,
                "/message",
                "message is required",
            ));
        }

        let edge = match load_steer_subagent_target(&self.node, &request_id, &child_request_id)
            .await?
        {
            SteerSubagentTarget::Found(edge) => edge,
            SteerSubagentTarget::NotAuthorized => {
                finish!(tool_not_allowed_payload(
                    STEER_SUBAGENT_TOOL_NAME,
                    "/child_request_id",
                    &child_request_id,
                    "child not owned by this parent request",
                    Vec::new(),
                ));
            }
            SteerSubagentTarget::NotBackgrounded => {
                finish!(tool_not_allowed_payload(
                    STEER_SUBAGENT_TOOL_NAME,
                    "/child_request_id",
                    &child_request_id,
                    "foreground subagents cannot be steered; call cancel_subagent first",
                    Vec::new(),
                ));
            }
            SteerSubagentTarget::AwaitingMaterialization { message, retryable } => {
                let result = service_unavailable_payload(
                    STEER_SUBAGENT_TOOL_NAME,
                    "/child_request_id",
                    message,
                    retryable,
                );
                finish!(result);
            }
            SteerSubagentTarget::Terminal(state) => {
                let result = invalid_tool_arguments_payload(
                    STEER_SUBAGENT_TOOL_NAME,
                    "/child_request_id",
                    format!("child is in terminal state '{state}'; spawn a new subagent instead"),
                );
                finish!(result);
            }
        };

        let mut interrupted_active_request_id = None;
        let mut drained_wake_up_request_ids = Vec::new();
        if parsed.interrupt {
            if let Some(active_request) = crate::interrupt::active_session_request(
                &self.node,
                &edge.child_session_id,
                &edge.child_agent_did,
                edge.child_requester_did.as_deref(),
            )
            .await?
            {
                drained_wake_up_request_ids =
                    crate::interrupt::interrupt_request_by_doc_id_returning_drained_wake_ids(
                        &self.node,
                        active_request
                            .doc_id
                            .as_deref()
                            .context("active request missing physical identity")?,
                        &edge.child_agent_did,
                        edge.child_requester_did.as_deref(),
                    )
                    .await?;
                let _descendants_cancelled = self
                    .cancel_live_subagent_descendants(
                        &edge.child_session_id,
                        &edge.child_agent_did,
                        edge.child_requester_did.as_deref(),
                        CancelCause::UserCancelled,
                    )
                    .await?;
                interrupted_active_request_id = Some(active_request.request_id);
            } else {
                drained_wake_up_request_ids = drain_automated_wakeups_returning_ids(
                    &self.node,
                    &edge.child_session_id,
                    &edge.child_agent_did,
                    edge.child_requester_did.as_deref(),
                    "automated wake-up drained because subagent was steered with interrupt=true",
                )
                .await?;
            }
        }

        let response = append_steering_request(
            &self.node,
            &request_id,
            &edge,
            &message,
            interrupted_active_request_id,
            drained_wake_up_request_ids,
        )
        .await?;
        let result = serde_json::to_value(response)
            .map_err(|error| anyhow::anyhow!("serialize steer_subagent response: {error}"))?;
        self.complete_control_tool_call(
            &mut lifecycle,
            STEER_SUBAGENT_TOOL_NAME,
            json_string(result),
        )
        .await
    }

    pub(super) async fn persist_cancel_subagent_tool_call(
        &self,
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
                CANCEL_SUBAGENT_TOOL_NAME,
                args,
                deadline_at,
                AwaitMode::Foreground,
                crate::tool_call_lifecycle::CancelPolicy::Cascade,
            )
            .await?;
        lifecycle.start_running().await?;
        macro_rules! finish {
            ($result:expr) => {
                return self
                    .complete_control_tool_call(&mut lifecycle, CANCEL_SUBAGENT_TOOL_NAME, $result)
                    .await
            };
        }

        let parsed = match serde_json::from_str::<CancelSubagentArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                finish!(invalid_tool_arguments_payload(
                    CANCEL_SUBAGENT_TOOL_NAME,
                    "/",
                    format!("invalid cancel_subagent arguments: {error}"),
                ));
            }
        };
        let child_request_id = parsed.child_request_id.trim();
        if child_request_id.is_empty() {
            finish!(invalid_tool_arguments_payload(
                CANCEL_SUBAGENT_TOOL_NAME,
                "/child_request_id",
                "child_request_id is required",
            ));
        }
        if parsed
            .reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            finish!(invalid_tool_arguments_payload(
                CANCEL_SUBAGENT_TOOL_NAME,
                "/reason",
                "reason must be omitted or non-empty",
            ));
        }

        let outcome = crate::cancel_session_subagent(
            self.node.clone(),
            &request_id,
            child_request_id,
            parsed.reason.as_deref(),
        )
        .await?;
        let edge = match outcome {
            crate::CancelSubagentOutcome::Unavailable {
                diagnostic,
                retryable,
            } => {
                finish!(service_unavailable_payload(
                    CANCEL_SUBAGENT_TOOL_NAME,
                    "/child_request_id",
                    diagnostic,
                    retryable,
                ));
            }
            crate::CancelSubagentOutcome::NotAuthorized => {
                finish!(tool_not_allowed_payload(
                    CANCEL_SUBAGENT_TOOL_NAME,
                    "/child_request_id",
                    child_request_id,
                    "descendant is visible but control belongs to its immediate parent session principal",
                    Vec::new(),
                ));
            }
            crate::CancelSubagentOutcome::Cancelled(receipt)
            | crate::CancelSubagentOutcome::AlreadyTerminal(receipt) => receipt,
        };
        // The shared helper owns persisted cancellation. Refresh the hook's
        // execution handle so it cannot retain a now-terminal bridge.
        self.refresh_owned_in_flight_lifecycle_from_storage(
            &edge.parent_session_id,
            &edge.parent_tool_call_id,
        )
        .await?;

        self.complete_control_tool_call(
            &mut lifecycle,
            CANCEL_SUBAGENT_TOOL_NAME,
            json_string(json!({
                "ok": true,
                "child_request_id": edge.child_request_id,
                "child_session_id": edge.child_session_id,
                "behavior_id": edge.behavior_id,
                "status": "cancelled",
                "active_interrupted": edge.active_interrupted,
                "descendants_cancelled": edge.descendants_cancelled,
                "queued_drained": edge.queued_drained
            })),
        )
        .await
    }
}
