use super::*;
use serde::{Deserialize, Serialize};

use crate::defra_node::EmbeddedNode;
use crate::graphql::escape_graphql_string;
use crate::workflow::{
    workflow_barrier_projection_legal, WORKFLOW_ROLE_FAN_OUT_CHILD, WORKFLOW_ROLE_SYNTHESIS,
};

#[derive(Debug, Clone)]
struct WorkflowSpawnSpec {
    task_id: Option<String>,
    target_name: String,
    agent_did: String,
    behavior_id: String,
    prompt: String,
}

/// A persisted workflow bridge row, read back from `AgentToolCall` by
/// `workflow_group_id`. This is the durable projection the barrier is proven
/// over (`workflow_barrier_projection_legal`) and the source for idempotent
/// adoption on a parent reclaim mid-barrier.
#[derive(Debug, Clone, Deserialize)]
struct WorkflowGroupBridgeRow {
    tool_call_id: String,
    #[serde(default)]
    child_request_id: Option<String>,
    #[serde(default)]
    workflow_role: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
}

impl WorkflowGroupBridgeRow {
    fn is_role(&self, role: &str) -> bool {
        self.workflow_role.as_deref() == Some(role)
    }
}

/// Load every persisted bridge row in one workflow group (the durable barrier
/// projection surface). Ordered by `started_at` so adopted fan-out indices are
/// stable across a reclaim.
async fn load_workflow_group_bridges(
    node: &EmbeddedNode,
    session_id: &str,
    workflow_group_id: &str,
) -> anyhow::Result<Vec<WorkflowGroupBridgeRow>> {
    let escaped_session = escape_graphql_string(session_id);
    let escaped_group = escape_graphql_string(workflow_group_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session}" }},
                    workflow_group_id: {{ _eq: "{escaped_group}" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                tool_call_id
                child_request_id
                workflow_role
                lifecycle_state
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "loading workflow group bridges for group {workflow_group_id} failed: {:?}",
            response.errors
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default())
}

#[derive(Debug, Clone, Serialize)]
struct WorkflowOutcome {
    task_id: String,
    child_request_id: String,
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
                );
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
        );

        if let Some((failure_class, payload)) =
            self.validate_workflow_invocation(&parent_context, &parsed)
        {
            lifecycle.spawn_failed(failure_class, &payload).await?;
            return Ok(self.skip_tool_result(FAN_OUT_AND_SYNTHESIZE_TOOL_NAME, payload));
        }

        lifecycle.start_running().await?;

        let result = match self
            .run_fan_out_and_synthesize(internal_call_id, seq, &parent_context, &parsed)
            .await
        {
            Ok(result) => {
                lifecycle.complete(&result).await?;
                result
            }
            Err(error) => {
                let payload = service_unavailable_payload(
                    FAN_OUT_AND_SYNTHESIZE_TOOL_NAME,
                    "/",
                    format!("fan_out_and_synthesize failed: {error:#}"),
                    true,
                );
                lifecycle.fail(&payload, FailureClass::External).await?;
                payload
            }
        };

        Ok(self.skip_tool_result(FAN_OUT_AND_SYNTHESIZE_TOOL_NAME, result))
    }

    fn validate_workflow_invocation(
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
        if parent_context.subagent_depth + 1 > MAX_SUBAGENT_DEPTH {
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
        let existing =
            load_workflow_group_bridges(&self.node, &parent_context.session_id, workflow_group_id)
                .await?;
        let existing_fan_out: Vec<&WorkflowGroupBridgeRow> = existing
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

        // Barrier enforcement, projection-side: gate synthesis on the proven
        // predicate evaluated over the DURABLE fan-out bridge rows (the exact
        // surface `Proofs/Workflow/FanOut.lean` and the conformance fence are
        // stated over). The per-bridge await above terminalizes each bridge; this
        // re-reads persisted state so a best-effort/deadline path that left a
        // bridge non-terminal can never reach synthesis.
        let durable_fan_out_states: Vec<String> =
            load_workflow_group_bridges(&self.node, &parent_context.session_id, workflow_group_id)
                .await?
                .into_iter()
                .filter(|row| row.is_role(WORKFLOW_ROLE_FAN_OUT_CHILD))
                .filter_map(|row| row.lifecycle_state)
                .collect();
        if !workflow_barrier_projection_legal(
            durable_fan_out_states.iter().map(String::as_str),
            true,
        ) {
            anyhow::bail!(
                "workflow barrier not satisfied for group {workflow_group_id}: fan-out bridges \
                 are not all terminal in durable rows ({durable_fan_out_states:?}); refusing to \
                 spawn synthesis"
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

    /// Terminalize a set of just-spawned workflow bridges so a spawn failure
    /// mid-fan-out cannot leave durable `running` bridge rows (and the cascade
    /// policy interrupts their child requests). Best-effort per bridge.
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
        let child_request_id = uuid::Uuid::new_v4().to_string();
        let tool_call_id = uuid::Uuid::new_v4().to_string();
        let bridge_args = serde_json::json!({
            "name": spec.target_name,
            "agent_did": spec.agent_did,
            "behavior_id": spec.behavior_id,
            "prompt": spec.prompt,
            "deadline": serde_json::Value::Null,
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
        );
        lifecycle.set_workflow_group(workflow_group_id, workflow_role);
        if await_mode == AwaitMode::Background {
            lifecycle.set_unclaimed_deadline_at(Some(
                chrono::Utc::now()
                    + chrono::Duration::seconds(
                        effective_context_cross_deployment_spawn_timeout_seconds(parent_context),
                    ),
            ));
        }
        lifecycle.start_running().await?;
        self.in_flight_lifecycles
            .lock()
            .await
            .insert(tool_call_id.clone(), lifecycle);
        Ok(WorkflowBridge {
            tool_call_id,
            child_request_id,
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
                // A child can complete in the last poll window; prefer that real
                // completion over recording 'dead' on the deadline edge.
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
                // Genuinely timed out: terminalize the bridge (propagate a failed
                // mutation rather than silently leaving a `running` row — the
                // durable barrier gate also refuses synthesis on any non-terminal
                // fan-out bridge).
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
}

fn workflow_task_id(index: usize, spec: &WorkflowSpawnSpec) -> String {
    spec.task_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| format!("{}:{index}", spec.target_name))
}
