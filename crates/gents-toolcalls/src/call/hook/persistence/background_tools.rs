use super::*;
use anyhow::Context;
use futures::FutureExt;
use gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

mod cancel;
pub(super) mod finish;
mod inspection;

use finish::finish_background_execution;

async fn complete_spawn_process_failure(
    hook: &DefraSessionHook,
    lifecycle: &mut ToolCallLifecycle,
    failure: FailureClass,
    result: String,
    denial: Option<&gents_tools::native::CommandPolicyDenial>,
) -> anyhow::Result<ToolCallHookAction> {
    let (bounded, presentation) = bounded_tool_result_with_presentation(
        SPAWN_PROCESS_TOOL_NAME,
        &result,
        &hook.truncation_limits,
    );
    let won = match denial {
        Some(denial) => lifecycle.fail_with_command_denial(&bounded, denial).await?,
        None => {
            lifecycle
                .fail_raw_with_presentation(
                    &result,
                    &bounded,
                    failure,
                    presentation.unwrap_or(gents_protocol::output::PayloadPresentation::Full),
                )
                .await?
        }
    };
    if won {
        return Ok(ToolCallHookAction::skip(bounded));
    }
    hook.persisted_direct_tool_result(lifecycle).await
}

impl DefraSessionHook {
    pub(super) async fn persist_background_tool_call(
        &self,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        context: &gents_loop::tool::ToolCallEnvelope,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _seq) =
            self.ensure_assistant_turn_sequence().await?;
        let mut parent_lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                SPAWN_PROCESS_TOOL_NAME,
                args,
                deadline_at,
                AwaitMode::Foreground,
            )
            .await?;
        parent_lifecycle.start_running().await?;

        let parsed = match serde_json::from_str::<BackgroundToolArgs>(args) {
            Ok(args) => args,
            Err(error) => {
                return complete_spawn_process_failure(
                    self,
                    &mut parent_lifecycle,
                    FailureClass::ArgumentInvalid,
                    background_invalid_tool_arguments_payload(
                        SPAWN_PROCESS_TOOL_NAME,
                        "/",
                        format!("invalid spawn_process arguments: {error}"),
                    ),
                    None,
                )
                .await;
            }
        };

        let target_name = parsed.tool_name.trim();
        if target_name.is_empty() {
            return complete_spawn_process_failure(
                self,
                &mut parent_lifecycle,
                FailureClass::ArgumentInvalid,
                background_invalid_tool_arguments_payload(
                    SPAWN_PROCESS_TOOL_NAME,
                    "/tool_name",
                    "tool_name is required",
                ),
                None,
            )
            .await;
        }

        let Some(target_tool) = self.background_tool_registry.get(target_name) else {
            return complete_spawn_process_failure(
                self,
                &mut parent_lifecycle,
                FailureClass::ServiceUnavailable,
                background_tool_not_allowed_payload(
                    SPAWN_PROCESS_TOOL_NAME,
                    "/tool_name",
                    target_name,
                    format!("tool '{target_name}' is not allowed for backgrounding by this agent"),
                    self.background_tool_registry.allowlist(),
                ),
                None,
            )
            .await;
        };

        let target_tool_name = target_name.to_string();
        let target_args = serde_json::to_string(&parsed.args)?;
        if let Err(error) = target_tool.admit(&target_args) {
            let gents_loop::tool_call_lifecycle::ToolOutcome::Failed {
                class,
                denial,
                text,
            } = gents_loop::tool_call_lifecycle::ToolOutcome::from_dispatch(
                target_name,
                Err(error),
            )
            else {
                anyhow::bail!("admission rejection must carry a failed outcome");
            };
            return complete_spawn_process_failure(
                self,
                &mut parent_lifecycle,
                class,
                text,
                denial.as_ref(),
            )
            .await;
        }
        let selected_tool_identity = self.remote_tools.as_ref().and_then(|remote| {
            gents_tools::mcp::meta::selected_remote_identity(target_name, &target_args, remote)
        });
        let lifetime = self
            .background_tool_registry
            .timeouts()
            .for_execution(
                target_name,
                selected_tool_identity
                    .as_ref()
                    .map(|(service, _)| service.as_str()),
            )
            .lifetime;
        let background_deadline_at = chrono::Utc::now()
            + chrono::Duration::from_std(lifetime)
                .context("background lifetime exceeds the representable range")?;
        let background_tool_call_id = format!(
            "spawned:{}",
            parent_lifecycle.doc_id().ok_or_else(|| anyhow::anyhow!(
                "running spawn_process parent lacks physical identity"
            ))?
        );
        let effective_timeout = if gents_tools::native::is_bash_tool_name(target_name) {
            let requested = parsed.args.get("timeout_secs").and_then(|v| v.as_u64());
            gents_tools::native::background_command_timeout(requested).min(lifetime)
        } else {
            lifetime
        };
        let receipt = json_string(json!({
            "ok": true,
            "tool_call_id": background_tool_call_id,
            "tool_name": target_tool_name,
            "await_mode": "background",
            "status": "running",
            "effective_timeout_secs": effective_timeout.as_secs()
        }));
        let admission = parent_lifecycle
            .admit_spawned_background(
                crate::call::lifecycle::SpawnedBackgroundToolAdmission {
                    tool_name: target_tool_name.clone(),
                    deadline_at: background_deadline_at,
                    selected_tool_identity,
                },
                &receipt,
            )
            .await;
        let mut lifecycle = match admission {
            Ok(lifecycle) => lifecycle,
            Err(error) => {
                match error.downcast_ref::<BackgroundSlotRefusal>() {
                    Some(BackgroundSlotRefusal::CapacityExceeded(admitted)) => {
                        return complete_spawn_process_failure(
                            self,
                            &mut parent_lifecycle,
                            FailureClass::ServiceUnavailable,
                            background_budget_exceeded_payload(SPAWN_PROCESS_TOOL_NAME, *admitted),
                            None,
                        )
                        .await;
                    }
                    // A script that outlived its request cannot start background
                    // work; that is this child's failure, not the script's.
                    Some(BackgroundSlotRefusal::RequestStopped)
                        if parent_lifecycle.is_plugin_effect() =>
                    {
                        return complete_spawn_process_failure(
                            self,
                            &mut parent_lifecycle,
                            FailureClass::ServiceUnavailable,
                            json_string(json!({
                                "ok": false,
                                "failure_class": "service_unavailable",
                                "code": "request_stopped",
                                "path": "/",
                                "message": "the request that started this script has stopped; a script can start a background process only while that request runs",
                                "retryable": false,
                                "service_id": "process",
                                "tool_name": SPAWN_PROCESS_TOOL_NAME
                            })),
                            None,
                        )
                        .await;
                    }
                    _ => {}
                }
                return Err(error);
            }
        };
        anyhow::ensure!(
            lifecycle.tool_call_id() == background_tool_call_id,
            "spawned admission returned an unexpected stable process handle"
        );
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        let execution_tool_doc_id = lifecycle
            .doc_id()
            .ok_or_else(|| anyhow::anyhow!("spawned background lifecycle lacks physical identity"))?
            .to_owned();
        let execution_reservation = self.background_executions.reserve(
            background_tool_call_id.clone(),
            execution_tool_doc_id.clone(),
            cancellation_token.clone(),
        )?;
        lifecycle.start_running().await?;

        let composes = parent_lifecycle.spawns_composing_invocation();
        let selected_tools = self.selected_loop_tools.lock().await.clone();
        let dispatcher = if let Some(tools) = selected_tools.filter(|_| composes) {
            let mut hook = self.clone();
            let mut parents = rapidhash::RapidHashMap::default();
            parents.insert(background_tool_call_id.clone(), lifecycle.clone());
            hook.in_flight_lifecycles = Arc::new(tokio::sync::Mutex::new(parents));
            hook.effect_tool_calls = Default::default();
            hook.effect_lifecycles = Default::default();
            Some(gents_loop::loop_stream::tool_effect_dispatcher(
                tools,
                Arc::new(hook),
                background_tool_call_id.clone(),
            ))
        } else {
            None
        };
        let stopped_children = dispatcher.clone();

        let node = self.node.clone();
        let live_outputs = self.background_live_outputs.clone();
        let execution_call_id = background_tool_call_id.clone();
        let execution_session_id = session_id.clone();
        let execution_request_id = request_id.clone();
        let live_output_writer = live_outputs
            .canonical_writer_for(lifecycle.tool_output_binding()?)
            .await;
        let runtime_context = gents_tools::runtime_context::current_tool_runtime_context();
        let workspace = gents_tools::runtime_context::ToolWorkspaceScope {
            runtime_home: runtime_context
                .as_ref()
                .and_then(|runtime| runtime.runtime_home.clone()),
            workspace_cwd: runtime_context
                .as_ref()
                .and_then(|runtime| runtime.workspace_cwd.clone()),
            workspace_root: runtime_context
                .as_ref()
                .and_then(|runtime| runtime.workspace_root.clone()),
            workspace_authority: runtime_context
                .as_ref()
                .and_then(|runtime| runtime.workspace_authority),
            workspace_artifact: runtime_context
                .as_ref()
                .and_then(|runtime| runtime.workspace_artifact.clone()),
        };
        let correlation = runtime_context
            .as_ref()
            .and_then(|runtime| runtime.correlation.clone());
        let requester_did = runtime_context
            .as_ref()
            .and_then(|runtime| runtime.requester_did.clone());
        let request_node_did = runtime_context
            .as_ref()
            .and_then(|runtime| runtime.node_did.clone());
        let request_agent_did = runtime_context
            .as_ref()
            .and_then(|runtime| runtime.agent_did.clone());
        let source_fields = runtime_context
            .map(|runtime| runtime.source_fields)
            .unwrap_or_default();
        let executions = self.background_executions.clone();
        let process_tool = gents_tools::native::is_bash_tool_name(target_name);
        let process_recorder =
            executions.process_recorder(&execution_call_id, &execution_tool_doc_id);
        let envelope = crate::call::spawned_envelope::background_envelope(
            context,
            crate::call::spawned_envelope::BackgroundAdmission {
                request_id: execution_request_id.clone(),
                deadline_at: background_deadline_at,
                cancellation_token: cancellation_token.clone(),
                live_output: live_output_writer.clone(),
                session_id: execution_session_id.clone(),
            },
            process_recorder.clone(),
            dispatcher.clone(),
            process_tool,
        );
        tokio::spawn(gents_tools::plugin::approval::carry_interactive(
            async move {
                let execution = AssertUnwindSafe(gents_session::exec::ownership::scope_process_recorder(
                process_recorder,
                async {
                        gents_tools::runtime_context::scope_request_tool_execution_with_workspace_overlay(
                            Some(gents_loop::tool_call_lifecycle::runtime::ToolRequestIdentity::new(requester_did, request_node_did, request_agent_did, Some(execution_request_id.clone()))),
                            Some(background_deadline_at),
                            cancellation_token.clone(),
                            workspace,
                            Some(live_output_writer),
                            Some(execution_session_id.clone()),
                            correlation,
                            source_fields,
                            true,
                            async {
                                if process_tool {
                                    gents_tools::runtime_context::debug_assert_ambient(&envelope);
                                    gents_loop::tool_call_lifecycle::runtime::call_tool_managed_with_cleanup(
                                        target_tool.as_ref(), target_args, &envelope, |outcome| {
                                            if let Some(outcome) = outcome.filter(|outcome| gents_session::exec::ownership::native_terminal_needs_confirmation(Some(outcome))) {
                                                executions.set_stop_receipt(&execution_call_id, &execution_tool_doc_id, crate::call::hook::RetainedStopReceipt::from_outcome(outcome));
                                            }
                                            executions.live_process_evidence(&execution_call_id, &execution_tool_doc_id)
                                                != gents_session::exec::ownership::LiveProcessEvidence::NeverSpawned
                                        },
                                    ).await
                                } else {
                                    gents_loop::tool_effects::scope(dispatcher, async {
                                        gents_tools::runtime_context::debug_assert_ambient(&envelope);
                                        gents_loop::tool_call_lifecycle::runtime::call_tool_managed(target_tool.as_ref(), target_args, &envelope).await
                                    }).await
                                }
                            },
                        )
                        .await
                    },
            ))
            .catch_unwind()
            .await;

                // Cancellation, lifetime expiry and a guest failure all drop the
                // in-flight child call, so its row is settled here, before the
                // background owner publishes its own terminal.
                if let Some(children) = stopped_children {
                    let panicked = gents_loop::tool_call_lifecycle::ToolOutcome::Cancelled;
                    let stop = execution.as_ref().unwrap_or(&panicked);
                    if let Err(error) = children.settle_stopped(stop).await {
                        tracing::warn!(%error, tool_call_id = %lifecycle.tool_call_id(),
                        "background invocation left an unsettled child");
                    }
                }

                finish_background_execution(
                    lifecycle,
                    node,
                    live_outputs,
                    executions,
                    execution_reservation,
                    execution,
                )
                .await;
            },
        ));

        Ok(self.skip_tool_result(SPAWN_PROCESS_TOOL_NAME, receipt))
    }

    pub(in crate::call::hook) async fn complete_control_tool_call(
        &self,
        lifecycle: &mut crate::call::lifecycle::ToolCallLifecycle,
        tool_name: &str,
        raw_result: String,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (bounded, presentation) =
            bounded_tool_result_with_presentation(tool_name, &raw_result, &self.truncation_limits);
        let won = lifecycle
            .complete_raw_with_presentation(
                &raw_result,
                &bounded,
                presentation.unwrap_or(gents_protocol::output::PayloadPresentation::Full),
            )
            .await?;
        if won {
            return Ok(ToolCallHookAction::skip(bounded));
        }
        self.persisted_direct_tool_result(lifecycle).await
    }

    /// Failed native control cannot be presented as a successful interception:
    /// that would let the provider repeat unavailable authorization indefinitely
    /// inside the same request, bypassing the durable Goal retry owner.
    pub(in crate::call::hook) async fn fail_control_tool_call(
        &self,
        lifecycle: &mut crate::call::lifecycle::ToolCallLifecycle,
        tool_name: &str,
        raw_result: String,
        failure: gents_loop::tool_call_lifecycle::FailureClass,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (bounded, presentation) =
            bounded_tool_result_with_presentation(tool_name, &raw_result, &self.truncation_limits);
        let won = lifecycle
            .fail_raw_with_presentation(
                &raw_result,
                &bounded,
                failure,
                presentation.unwrap_or(gents_protocol::output::PayloadPresentation::Full),
            )
            .await?;
        if won {
            return Ok(ToolCallHookAction::terminate(bounded));
        }
        self.persisted_direct_tool_result(lifecycle).await
    }

    async fn persisted_direct_tool_result(
        &self,
        lifecycle: &crate::call::lifecycle::ToolCallLifecycle,
    ) -> anyhow::Result<ToolCallHookAction> {
        let message = gents_session::tool_call::query::result::load_tool_call_result(
            &self.actor_access()?,
            lifecycle
                .doc_id()
                .context("lost terminal compare requires physical tool identity")?,
            lifecycle.node_did(),
            lifecycle.session_id(),
            lifecycle.requester_did(),
        )
        .await?;
        Ok(ToolCallHookAction::skip(
            gents_session::tool_call::query::result::render_tool_result(&message)?,
        ))
    }
}

#[cfg(test)]
#[path = "background_tools_tests.rs"]
mod tests;
