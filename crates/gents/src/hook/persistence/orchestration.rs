use super::*;
use serde::Serialize;

use crate::workflow::{
    fan_out_barrier_satisfied, load_workflow_group_bridges, WORKFLOW_ROLE_FAN_OUT_CHILD,
    WORKFLOW_ROLE_SYNTHESIS,
};

#[derive(Debug)]
struct WorkflowSpawnRejected {
    failure_class: FailureClass,
    payload: String,
}

impl std::fmt::Display for WorkflowSpawnRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "workflow spawn rejected ({:?})", self.failure_class)
    }
}

impl std::error::Error for WorkflowSpawnRejected {}

fn classify_workflow_run_error(error: &anyhow::Error) -> (FailureClass, String) {
    match error.downcast_ref::<WorkflowSpawnRejected>() {
        Some(rejected) => (rejected.failure_class, rejected.payload.clone()),
        None => (
            FailureClass::External,
            service_unavailable_payload(
                FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                "/",
                format!("fan_out_and_synthesize failed: {error:#}"),
                true,
            ),
        ),
    }
}

#[derive(Debug, Clone)]
struct WorkflowSpawnSpec {
    task_id: Option<String>,
    target_name: String,
    agent_did: String,
    behavior_id: String,
    prompt: String,
}

#[derive(Debug, Clone, Serialize)]
struct WorkflowOutcome {
    task_id: String,
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    child_request_id: String,
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    behavior_id: String,
    status: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl DefraSessionHook {
    pub(super) async fn persist_fan_out_and_synthesize_tool_call(
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

        let parsed = match serde_json::from_str::<crate::workflow::FanOutAndSynthesizeArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                let mut lifecycle = ToolCallLifecycle::new(
                    self.node.clone(),
                    request_id,
                    session_id,
                    self.agent_did.clone(),
                    internal_call_id.to_string(),
                    seq,
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME.to_string(),
                    args.to_string(),
                    hook_deadline_at,
                )
                .with_requester_did(self.active_requester_did().await);
                let payload = invalid_tool_arguments_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/",
                    format!("invalid fan_out_and_synthesize arguments: {error}"),
                );
                lifecycle
                    .spawn_failed(FailureClass::ArgumentInvalid, &payload)
                    .await?;
                return Ok(self.skip_tool_result(FAN_OUT_AND_SYNTHESIZE_TOOL_NAME, payload));
            }
        };

        let parent_context = load_parent_subagent_context(&self.node, &request_id).await?;
        let mut lifecycle = ToolCallLifecycle::new(
            self.node.clone(),
            request_id.clone(),
            session_id.clone(),
            self.agent_did.clone(),
            internal_call_id.to_string(),
            seq,
            FAN_OUT_AND_SYNTHESIZE_TOOL_NAME.to_string(),
            args.to_string(),
            parent_context.request_deadline_at,
        )
        .with_requester_did(self.active_requester_did().await);

        if let Some((failure_class, payload)) = self
            .validate_workflow_invocation(&parent_context, &parsed)
            .await
        {
            lifecycle.spawn_failed(failure_class, &payload).await?;
            return Ok(self.skip_tool_result(FAN_OUT_AND_SYNTHESIZE_TOOL_NAME, payload));
        }

        lifecycle.start_running().await?;
        // Register the outer composite in the cancel-visible map so parent
        // interrupt can terminalize it promptly (#837). The workflow future
        // must not hold exclusive ownership across await points; completion
        // re-takes (or reloads) the lifecycle and uses CAS terminalization so
        // a concurrent interrupt wins cleanly.
        self.in_flight_lifecycles
            .lock()
            .await
            .insert(internal_call_id.to_string(), lifecycle);

        let run_result = self
            .run_fan_out_and_synthesize(internal_call_id, seq, &parent_context, &parsed)
            .await;

        let result = self
            .terminalize_fan_out_composite(&parent_context.session_id, internal_call_id, run_result)
            .await?;

        Ok(self.skip_tool_result(FAN_OUT_AND_SYNTHESIZE_TOOL_NAME, result))
    }

    async fn terminalize_fan_out_composite(
        &self,
        session_id: &str,
        internal_call_id: &str,
        run_result: anyhow::Result<String>,
    ) -> anyhow::Result<String> {
        let mut lifecycle = match self
            .take_or_load_in_flight_lifecycle(session_id, internal_call_id)
            .await?
        {
            Some(lifecycle) => lifecycle,
            None => {
                return Ok("tool call cancelled".to_string());
            }
        };

        if lifecycle.is_terminal() {
            // Interrupt, deadline sweep, or recovery already won the durable race.
            self.discard_in_flight_lifecycle(internal_call_id).await;
            return Ok(self
                .composite_terminal_payload(session_id, &lifecycle)
                .await);
        }

        match run_result {
            Ok(result) => {
                lifecycle.complete(&result).await?;
                if lifecycle.state() == crate::tool_call_lifecycle::ToolCallState::Completed {
                    Ok(result)
                } else {
                    // Lost CAS to cancel/timeout/fail: durable terminal wins.
                    Ok(self
                        .composite_terminal_payload(session_id, &lifecycle)
                        .await)
                }
            }
            Err(error) => {
                let (failure_class, payload) = classify_workflow_run_error(&error);
                lifecycle.fail(&payload, failure_class).await?;
                if lifecycle.state() == crate::tool_call_lifecycle::ToolCallState::Failed {
                    Ok(payload)
                } else {
                    Ok(self
                        .composite_terminal_payload(session_id, &lifecycle)
                        .await)
                }
            }
        }
    }

    async fn composite_terminal_payload(
        &self,
        session_id: &str,
        lifecycle: &ToolCallLifecycle,
    ) -> String {
        match lifecycle.state() {
            crate::tool_call_lifecycle::ToolCallState::Cancelled => {
                "tool call cancelled".to_string()
            }
            crate::tool_call_lifecycle::ToolCallState::TimedOut => {
                "tool call deadline exceeded".to_string()
            }
            crate::tool_call_lifecycle::ToolCallState::Failed => "tool call failed".to_string(),
            crate::tool_call_lifecycle::ToolCallState::Completed => {
                crate::tool_call_lifecycle::query::load_tool_call_result(
                    &self.node,
                    session_id,
                    lifecycle.tool_call_id(),
                )
                .await
                .unwrap_or_else(|_| "tool call completed".to_string())
            }
            _ => "tool call cancelled".to_string(),
        }
    }

    async fn local_target_behavior_guard(
        &self,
        parent_context: &ParentSubagentContext,
        resolved: &SubagentTarget,
        pointer: &str,
    ) -> Option<(FailureClass, String)> {
        if self.subagent_target_host(resolved) != SubagentTargetHost::Local {
            return None;
        }
        match load_agent_behavior(&self.node, &resolved.behavior_id).await {
            Ok(None) => Some((
                FailureClass::ServiceUnavailable,
                tool_not_allowed_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    pointer,
                    &resolved.name,
                    format!(
                        "target '{}' refers to behavior '{}' which no longer exists; it may have \
                         been removed after this session started",
                        resolved.name, resolved.behavior_id
                    ),
                    context_allowed_target_names(parent_context),
                ),
            )),
            Ok(Some(_)) => None,
            Err(error) => {
                tracing::warn!(
                    behavior_id = %resolved.behavior_id,
                    %error,
                    "workflow guard: failed to verify local target behavior existence; proceeding"
                );
                None
            }
        }
    }

    async fn validate_workflow_invocation(
        &self,
        parent_context: &ParentSubagentContext,
        args: &crate::workflow::FanOutAndSynthesizeArgs,
    ) -> Option<(FailureClass, String)> {
        if !parent_context.orchestration_enabled {
            return Some((
                FailureClass::ServiceUnavailable,
                tool_not_allowed_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/",
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "workflow orchestration is not enabled for this behavior",
                    context_allowed_target_names(parent_context),
                ),
            ));
        }
        if !parent_context.subagent_spawn_enabled {
            return Some((
                FailureClass::ServiceUnavailable,
                tool_not_allowed_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/",
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "subagent spawning is not enabled for this behavior",
                    context_allowed_target_names(parent_context),
                ),
            ));
        }
        if !parent_context.subagent_background_enabled {
            return Some((
                FailureClass::ServiceUnavailable,
                tool_not_allowed_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/",
                    "background",
                    "background subagent spawning is required for workflow orchestration",
                    context_allowed_target_names(parent_context),
                ),
            ));
        }
        if parent_context.subagent_depth >= MAX_SUBAGENT_DEPTH {
            return Some((
                FailureClass::ArgumentInvalid,
                depth_exceeded_payload(parent_context.subagent_depth),
            ));
        }

        let width = args.fan_out_width();
        if width == 0 {
            return Some((
                FailureClass::ArgumentInvalid,
                invalid_tool_arguments_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/tasks",
                    "tasks must contain at least one fan-out task",
                ),
            ));
        }
        if width > crate::workflow::MAX_FAN_OUT_TASKS {
            return Some((
                FailureClass::ArgumentInvalid,
                invalid_tool_arguments_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/tasks",
                    format!(
                        "tasks may contain at most {} fan-out tasks",
                        crate::workflow::MAX_FAN_OUT_TASKS
                    ),
                ),
            ));
        }
        if args.synthesis_prompt.trim().is_empty() {
            return Some((
                FailureClass::ArgumentInvalid,
                invalid_tool_arguments_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/synthesis_prompt",
                    "synthesis_prompt is required",
                ),
            ));
        }
        let Some(synthesis_target) = args
            .synthesis_target
            .as_deref()
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .and_then(|target| resolve_context_target(parent_context, target))
        else {
            return Some((
                FailureClass::ServiceUnavailable,
                tool_not_allowed_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/synthesis_target",
                    args.synthesis_target.as_deref().unwrap_or_default(),
                    "synthesis_target is not an allowed subagent target for this behavior",
                    context_allowed_target_names(parent_context),
                ),
            ));
        };
        if self.subagent_target_host(synthesis_target) == SubagentTargetHost::Remote {
            if !parent_context.subagent_allow_cross_deployment {
                return Some((
                    FailureClass::ServiceUnavailable,
                    tool_not_allowed_payload(
                        FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                        "/synthesis_target",
                        &synthesis_target.name,
                        "cross-deployment subagent delegation is not enabled",
                        context_allowed_target_names(parent_context),
                    ),
                ));
            }
            return Some((FailureClass::ArgumentInvalid, invalid_tool_arguments_payload(
                FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                "/synthesis_target",
                "foreground cross-deployment synthesis is not supported in cut 1; use a local synthesis target",
            )));
        }
        if let Some(failure) = self
            .local_target_behavior_guard(parent_context, synthesis_target, "/synthesis_target")
            .await
        {
            return Some(failure);
        }

        let default_target = args
            .target
            .as_deref()
            .map(str::trim)
            .filter(|target| !target.is_empty());
        if let Some(target) = default_target {
            let Some(resolved) = resolve_context_target(parent_context, target) else {
                return Some((
                    FailureClass::ServiceUnavailable,
                    tool_not_allowed_payload(
                        FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                        "/target",
                        target,
                        "target is not an allowed subagent target for this behavior",
                        context_allowed_target_names(parent_context),
                    ),
                ));
            };
            if self.subagent_target_host(resolved) == SubagentTargetHost::Remote
                && !parent_context.subagent_allow_cross_deployment
            {
                return Some((
                    FailureClass::ServiceUnavailable,
                    tool_not_allowed_payload(
                        FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                        "/target",
                        target,
                        "cross-deployment subagent delegation is not enabled",
                        context_allowed_target_names(parent_context),
                    ),
                ));
            }
            if let Some(failure) = self
                .local_target_behavior_guard(parent_context, resolved, "/target")
                .await
            {
                return Some(failure);
            }
        }
        for (index, task) in args.tasks.iter().enumerate() {
            if task.prompt.trim().is_empty() {
                return Some((
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(
                        FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                        &format!("/tasks/{index}/prompt"),
                        "task prompt is required",
                    ),
                ));
            }
            let target = task
                .target
                .as_deref()
                .map(str::trim)
                .filter(|target| !target.is_empty())
                .or(default_target);
            let Some(target) = target else {
                return Some((
                    FailureClass::ArgumentInvalid,
                    invalid_tool_arguments_payload(
                        FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                        &format!("/tasks/{index}/target"),
                        "task target is required when no default target is set",
                    ),
                ));
            };
            let Some(resolved) = resolve_context_target(parent_context, target) else {
                return Some((
                    FailureClass::ServiceUnavailable,
                    tool_not_allowed_payload(
                        FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                        &format!("/tasks/{index}/target"),
                        target,
                        "task target is not an allowed subagent target for this behavior",
                        context_allowed_target_names(parent_context),
                    ),
                ));
            };
            if self.subagent_target_host(resolved) == SubagentTargetHost::Remote
                && !parent_context.subagent_allow_cross_deployment
            {
                return Some((
                    FailureClass::ServiceUnavailable,
                    tool_not_allowed_payload(
                        FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                        &format!("/tasks/{index}/target"),
                        target,
                        "cross-deployment subagent delegation is not enabled",
                        context_allowed_target_names(parent_context),
                    ),
                ));
            }
            if let Some(failure) = self
                .local_target_behavior_guard(
                    parent_context,
                    resolved,
                    &format!("/tasks/{index}/target"),
                )
                .await
            {
                return Some(failure);
            }
        }

        None
    }

    async fn run_fan_out_and_synthesize(
        &self,
        workflow_group_id: &str,
        message_sequence: u32,
        parent_context: &ParentSubagentContext,
        args: &crate::workflow::FanOutAndSynthesizeArgs,
    ) -> anyhow::Result<String> {
        let fan_out_specs = self.resolve_fan_out_specs(parent_context, args)?;

        // Idempotent adoption (reclaim safety): a parent reclaim mid-barrier
        // must not double-spawn the group. Adopt any bridges already persisted
        // under this `workflow_group_id`; spawn only the missing slots.
        //
        // Cut-1 limitations (accepted, fail-safe): adopted fan-out rows are
        // aligned to specs positionally over an `started_at ASC` order — ties are
        // impossible today because spawns are sequential round-trips, and the
        // downstream barrier gate count-check (`fan_out_barrier_satisfied`) fails
        // closed on any misalignment, so a wrong alignment cannot admit synthesis.
        // A persisted slot index (deterministic key) is a cut-2 hardening.
        let existing =
            load_workflow_group_bridges(&self.node, &parent_context.session_id, workflow_group_id)
                .await?;
        let existing_fan_out: Vec<&crate::workflow::WorkflowBridgeRow> = existing
            .iter()
            .filter(|row| row.is_role(WORKFLOW_ROLE_FAN_OUT_CHILD))
            .collect();

        let mut fan_out_bridges: Vec<(usize, WorkflowSpawnSpec, WorkflowBridge)> =
            Vec::with_capacity(fan_out_specs.len());
        for (index, spec) in fan_out_specs.iter().enumerate() {
            let bridge = if let Some(row) = existing_fan_out.get(index) {
                WorkflowBridge {
                    tool_call_id: row.tool_call_id.clone(),
                    child_request_id: row.child_request_id.clone().unwrap_or_default(),
                    unclaimed_deadline_at: None,
                }
            } else {
                match self
                    .spawn_workflow_subagent_bridge(
                        workflow_group_id,
                        message_sequence,
                        WORKFLOW_ROLE_FAN_OUT_CHILD,
                        parent_context,
                        spec,
                        AwaitMode::Background,
                    )
                    .await
                {
                    Ok(bridge) => bridge,
                    Err(error) => {
                        // A spawn failure mid-fan-out must not orphan the bridges
                        // already spawned this call; terminalize them before bailing.
                        self.cancel_workflow_bridges(parent_context, &fan_out_bridges)
                            .await;
                        return Err(error);
                    }
                }
            };
            fan_out_bridges.push((index, spec.clone(), bridge));
        }

        let mut outcomes = Vec::with_capacity(fan_out_bridges.len());
        for (index, spec, bridge) in &fan_out_bridges {
            outcomes.push(
                self.await_workflow_fan_out_bridge(
                    parent_context,
                    workflow_group_id,
                    *index,
                    spec,
                    bridge,
                )
                .await?,
            );
        }

        let durable_rows =
            load_workflow_group_bridges(&self.node, &parent_context.session_id, workflow_group_id)
                .await?;
        if !fan_out_barrier_satisfied(&durable_rows, fan_out_bridges.len()) {
            let states: Vec<_> = durable_rows
                .iter()
                .filter(|row| row.is_role(WORKFLOW_ROLE_FAN_OUT_CHILD))
                .map(|row| row.lifecycle_state.clone())
                .collect();
            anyhow::bail!(
                "workflow barrier not satisfied for group {workflow_group_id}: expected \
                 {} fan-out bridges all terminal in durable rows, got {states:?}; refusing to \
                 spawn synthesis",
                fan_out_bridges.len()
            );
        }

        let synthesis_target = args
            .synthesis_target
            .as_deref()
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .and_then(|target| resolve_context_target(parent_context, target))
            .expect("validated synthesis target")
            .clone();
        let outcomes_json = serde_json::to_string_pretty(&outcomes)?;
        let synthesis_prompt = format!(
            "{}\n\nFan-out outcomes (JSON):\n{}",
            args.synthesis_prompt.trim(),
            outcomes_json
        );
        let synthesis_spec = WorkflowSpawnSpec {
            task_id: None,
            target_name: synthesis_target.name.clone(),
            agent_did: synthesis_target.agent_did.clone(),
            behavior_id: synthesis_target.behavior_id.clone(),
            prompt: synthesis_prompt,
        };
        let synthesis_bridge = if let Some(row) = existing
            .iter()
            .find(|row| row.is_role(WORKFLOW_ROLE_SYNTHESIS))
        {
            WorkflowBridge {
                tool_call_id: row.tool_call_id.clone(),
                child_request_id: row.child_request_id.clone().unwrap_or_default(),
                unclaimed_deadline_at: None,
            }
        } else {
            self.spawn_workflow_subagent_bridge(
                workflow_group_id,
                message_sequence,
                WORKFLOW_ROLE_SYNTHESIS,
                parent_context,
                &synthesis_spec,
                AwaitMode::Foreground,
            )
            .await?
        };
        self.await_foreground_subagent(
            &synthesis_bridge.tool_call_id,
            parent_context,
            &synthesis_bridge.child_request_id,
            "",
            &synthesis_spec.behavior_id,
            parent_context.request_deadline_at,
        )
        .await
    }

    fn resolve_fan_out_specs(
        &self,
        parent_context: &ParentSubagentContext,
        args: &crate::workflow::FanOutAndSynthesizeArgs,
    ) -> anyhow::Result<Vec<WorkflowSpawnSpec>> {
        let default_target = args
            .target
            .as_deref()
            .map(str::trim)
            .filter(|target| !target.is_empty());
        args.tasks
            .iter()
            .map(|task| {
                let target_name = task
                    .target
                    .as_deref()
                    .map(str::trim)
                    .filter(|target| !target.is_empty())
                    .or(default_target)
                    .ok_or_else(|| anyhow::anyhow!("validated task target missing"))?;
                let target = resolve_context_target(parent_context, target_name)
                    .ok_or_else(|| anyhow::anyhow!("validated target {target_name} missing"))?;
                Ok(WorkflowSpawnSpec {
                    task_id: task.id.clone(),
                    target_name: target.name.clone(),
                    agent_did: target.agent_did.clone(),
                    behavior_id: target.behavior_id.clone(),
                    prompt: task.prompt.clone(),
                })
            })
            .collect()
    }

    async fn cancel_workflow_bridges(
        &self,
        parent_context: &ParentSubagentContext,
        bridges: &[(usize, WorkflowSpawnSpec, WorkflowBridge)],
    ) {
        for (_, _, bridge) in bridges {
            if let Ok(Some(mut lifecycle)) = self
                .take_or_load_in_flight_lifecycle(&parent_context.session_id, &bridge.tool_call_id)
                .await
            {
                let _ = lifecycle.bridge_failure(ChildTerminal::Dead).await;
            }
            self.discard_in_flight_lifecycle(&bridge.tool_call_id).await;
        }
    }

    async fn spawn_workflow_subagent_bridge(
        &self,
        workflow_group_id: &str,
        message_sequence: u32,
        workflow_role: &str,
        parent_context: &ParentSubagentContext,
        spec: &WorkflowSpawnSpec,
        await_mode: AwaitMode,
    ) -> anyhow::Result<WorkflowBridge> {
        if let Some(target) = resolve_context_target(parent_context, &spec.target_name) {
            if self.subagent_target_host(target) == SubagentTargetHost::Local {
                match load_agent_behavior(&self.node, &spec.behavior_id).await {
                    Ok(None) => {
                        let pointer = if workflow_role == WORKFLOW_ROLE_SYNTHESIS {
                            "/synthesis_target"
                        } else {
                            "/tasks"
                        };
                        return Err(anyhow::Error::new(WorkflowSpawnRejected {
                            failure_class: FailureClass::ServiceUnavailable,
                            payload: tool_not_allowed_payload(
                                FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                                pointer,
                                &spec.target_name,
                                format!(
                                    "target '{}' refers to behavior '{}' which no longer exists; \
                                     it may have been removed after this session started",
                                    spec.target_name, spec.behavior_id
                                ),
                                context_allowed_target_names(parent_context),
                            ),
                        }));
                    }
                    Ok(Some(_)) => {}
                    Err(error) => tracing::warn!(
                        behavior_id = %spec.behavior_id,
                        %error,
                        "workflow spawn guard: failed to verify local target behavior; proceeding"
                    ),
                }
            }
        }
        let child_request_id = uuid::Uuid::new_v4().to_string();
        let tool_call_id = uuid::Uuid::new_v4().to_string();
        let target_agent_did = spec.agent_did.clone();
        let bridge_args = serde_json::json!({
            "name": spec.target_name,
            "agent_did": target_agent_did.clone(),
            "behavior_id": spec.behavior_id,
            "prompt": spec.prompt,
            "deadline": serde_json::Value::Null,
            "parent_subagent_depth": parent_context.subagent_depth,
        })
        .to_string();

        let mut lifecycle = ToolCallLifecycle::new_subagent(
            self.node.clone(),
            parent_context.request_id.clone(),
            parent_context.session_id.clone(),
            self.agent_did.clone(),
            tool_call_id.clone(),
            message_sequence,
            SPAWN_SUBAGENT_TOOL_NAME.to_string(),
            bridge_args,
            parent_context.request_deadline_at,
            await_mode,
            CancelPolicy::Cascade,
            child_request_id.clone(),
            target_agent_did,
        )
        .with_requester_did(self.active_requester_did().await);
        lifecycle.set_workflow_group(workflow_group_id, workflow_role);
        // A background (cross-deployment) child that is never CLAIMED by its
        // remote node must not hold the barrier open until the whole parent
        // request deadline — it goes dead at the spawn timeout, mirroring the
        // regular background-subagent path.
        let unclaimed_deadline_at = if await_mode == AwaitMode::Background {
            let deadline = chrono::Utc::now()
                + chrono::Duration::seconds(
                    effective_context_cross_deployment_spawn_timeout_seconds(parent_context),
                );
            lifecycle.set_unclaimed_deadline_at(Some(deadline));
            Some(deadline)
        } else {
            None
        };
        lifecycle.start_running().await?;
        self.in_flight_lifecycles
            .lock()
            .await
            .insert(tool_call_id.clone(), lifecycle);
        Ok(WorkflowBridge {
            tool_call_id,
            child_request_id,
            unclaimed_deadline_at,
        })
    }

    async fn await_workflow_fan_out_bridge(
        &self,
        parent_context: &ParentSubagentContext,
        _workflow_group_id: &str,
        index: usize,
        spec: &WorkflowSpawnSpec,
        bridge: &WorkflowBridge,
    ) -> anyhow::Result<WorkflowOutcome> {
        loop {
            let now = chrono::Utc::now();
            if now >= parent_context.request_deadline_at {
                if let Some(row) =
                    load_child_terminal_row(&self.node, &bridge.child_request_id).await?
                {
                    if child_request_completed(&row) {
                        if let Some(edge) = try_load_authorized_child_edge(
                            &self.node,
                            parent_context,
                            &bridge.child_request_id,
                        )
                        .await?
                        {
                            if let Some(final_response) =
                                load_child_final_response(&self.node, &edge).await?
                            {
                                if edge.lifecycle_state == "running" {
                                    if let Some(mut lifecycle) = self
                                        .take_or_load_in_flight_lifecycle(
                                            &parent_context.session_id,
                                            &bridge.tool_call_id,
                                        )
                                        .await?
                                    {
                                        let _ = lifecycle
                                            .bridge_complete(final_response.clone())
                                            .await?;
                                    }
                                }
                                self.discard_in_flight_lifecycle(&bridge.tool_call_id).await;
                                return Ok(WorkflowOutcome {
                                    task_id: workflow_task_id(index, spec),
                                    child_request_id: bridge.child_request_id.clone(),
                                    behavior_id: spec.behavior_id.clone(),
                                    status: "completed".to_string(),
                                    ok: true,
                                    final_response: Some(final_response),
                                    error: None,
                                });
                            }
                        }
                    }
                }
                if let Some(mut lifecycle) = self
                    .take_or_load_in_flight_lifecycle(
                        &parent_context.session_id,
                        &bridge.tool_call_id,
                    )
                    .await?
                {
                    // Parent-deadline expiry takes the licensed deadline
                    // transition (`timedOut`), never a fabricated
                    // `ChildTerminal::Dead` for a possibly-live child (#1002).
                    let _ = lifecycle.timeout().await?;
                }
                self.discard_in_flight_lifecycle(&bridge.tool_call_id).await;
                return Ok(WorkflowOutcome {
                    task_id: workflow_task_id(index, spec),
                    child_request_id: bridge.child_request_id.clone(),
                    behavior_id: spec.behavior_id.clone(),
                    status: "dead".to_string(),
                    ok: false,
                    final_response: None,
                    error: Some(
                        "parent request deadline exceeded while waiting for workflow child"
                            .to_string(),
                    ),
                });
            }

            let Some(edge) = try_load_authorized_child_edge(
                &self.node,
                parent_context,
                &bridge.child_request_id,
            )
            .await?
            else {
                if bridge.unclaimed_deadline_at.is_some_and(|dl| now >= dl) {
                    if let Some(mut lifecycle) = self
                        .take_or_load_in_flight_lifecycle(
                            &parent_context.session_id,
                            &bridge.tool_call_id,
                        )
                        .await?
                    {
                        let _ = lifecycle.bridge_failure(ChildTerminal::Dead).await?;
                    }
                    self.discard_in_flight_lifecycle(&bridge.tool_call_id).await;
                    return Ok(WorkflowOutcome {
                        task_id: workflow_task_id(index, spec),
                        child_request_id: bridge.child_request_id.clone(),
                        behavior_id: spec.behavior_id.clone(),
                        status: "dead".to_string(),
                        ok: false,
                        final_response: None,
                        error: Some(
                            "cross-deployment child not claimed before spawn timeout".to_string(),
                        ),
                    });
                }
                let remaining = (parent_context.request_deadline_at - now)
                    .to_std()
                    .unwrap_or(Duration::from_millis(0));
                tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
                continue;
            };

            if let Some(row) = load_child_terminal_row(&self.node, &bridge.child_request_id).await?
            {
                if child_request_completed(&row) {
                    let Some(final_response) = load_child_final_response(&self.node, &edge).await?
                    else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    if edge.lifecycle_state == "running" {
                        if let Some(mut lifecycle) = self
                            .take_or_load_in_flight_lifecycle(
                                &parent_context.session_id,
                                &bridge.tool_call_id,
                            )
                            .await?
                        {
                            let _ = lifecycle.bridge_complete(final_response.clone()).await?;
                        }
                    }
                    self.discard_in_flight_lifecycle(&bridge.tool_call_id).await;
                    return Ok(WorkflowOutcome {
                        task_id: workflow_task_id(index, spec),
                        child_request_id: bridge.child_request_id.clone(),
                        behavior_id: spec.behavior_id.clone(),
                        status: "completed".to_string(),
                        ok: true,
                        final_response: Some(final_response),
                        error: None,
                    });
                }

                if let Some(terminal) = project_child_terminal(&row) {
                    let status = child_terminal_status(&terminal).to_string();
                    let (reason, _) = child_terminal_error(&terminal);
                    if edge.lifecycle_state == "running" {
                        if let Some(mut lifecycle) = self
                            .take_or_load_in_flight_lifecycle(
                                &parent_context.session_id,
                                &bridge.tool_call_id,
                            )
                            .await?
                        {
                            let _ = lifecycle.bridge_failure(terminal).await?;
                        }
                    }
                    self.discard_in_flight_lifecycle(&bridge.tool_call_id).await;
                    return Ok(WorkflowOutcome {
                        task_id: workflow_task_id(index, spec),
                        child_request_id: bridge.child_request_id.clone(),
                        behavior_id: spec.behavior_id.clone(),
                        status,
                        ok: false,
                        final_response: None,
                        error: Some(reason.to_string()),
                    });
                }
            }

            let remaining = (parent_context.request_deadline_at - now)
                .to_std()
                .unwrap_or(Duration::from_millis(0));
            tokio::time::sleep(remaining.min(Duration::from_millis(250))).await;
        }
    }
}

#[derive(Debug, Clone)]
struct WorkflowBridge {
    tool_call_id: String,
    child_request_id: String,
    unclaimed_deadline_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn workflow_task_id(index: usize, spec: &WorkflowSpawnSpec) -> String {
    spec.task_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| format!("{}:{index}", spec.target_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the spawn-time local-behavior guard: a missing local target
    /// at spawn time must persist `serviceUnavailable` (matching the invocation
    /// guard and the normal spawn path), NOT the generic `external` of the
    /// catch-all run-error handler — `tool_failure_class` drives
    /// trace/projection/retry semantics.
    #[test]
    fn workflow_spawn_rejection_classifies_as_service_unavailable() {
        let rejected = anyhow::Error::new(WorkflowSpawnRejected {
            failure_class: FailureClass::ServiceUnavailable,
            payload: "synthesis-target-gone".to_string(),
        });
        let (class, payload) = classify_workflow_run_error(&rejected);
        assert_eq!(class, FailureClass::ServiceUnavailable);
        assert_eq!(payload, "synthesis-target-gone");

        // A generic run-path error remains External.
        let generic = anyhow::anyhow!("inference backend exploded");
        let (class, _) = classify_workflow_run_error(&generic);
        assert_eq!(class, FailureClass::External);
    }
}
