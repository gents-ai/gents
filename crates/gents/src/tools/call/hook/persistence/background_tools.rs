use super::*;
use anyhow::Context;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;

mod cancel;
pub(super) mod finish;
mod inspection;

use finish::finish_background_execution;

async fn complete_spawn_process_failure(
    hook: &DefraSessionHook,
    lifecycle: &mut ToolCallLifecycle,
    failure: FailureClass,
    result: String,
    denial: Option<&crate::toolset::CommandPolicyDenial>,
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
            let crate::tools::call::lifecycle::ToolOutcome::Failed {
                class,
                denial,
                text,
            } = crate::tools::call::lifecycle::ToolOutcome::from_dispatch(target_name, Err(error))
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
            crate::meta_tools::selected_remote_identity(target_name, &target_args, remote)
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
        let effective_timeout = if crate::toolset::is_bash_tool_name(target_name) {
            let requested = parsed.args.get("timeout_secs").and_then(|v| v.as_u64());
            crate::toolset::background_command_timeout(requested).min(lifetime)
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
                crate::tools::call::lifecycle::SpawnedBackgroundToolAdmission {
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
                if let Some(capacity) = error.downcast_ref::<BackgroundCapacityExceeded>() {
                    return complete_spawn_process_failure(
                        self,
                        &mut parent_lifecycle,
                        FailureClass::ServiceUnavailable,
                        background_budget_exceeded_payload(SPAWN_PROCESS_TOOL_NAME, capacity.0),
                        None,
                    )
                    .await;
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

        let node = self.node.clone();
        let live_outputs = self.background_live_outputs.clone();
        let execution_call_id = background_tool_call_id.clone();
        let execution_session_id = session_id.clone();
        let execution_request_id = request_id.clone();
        let live_output_writer = live_outputs
            .canonical_writer_for(lifecycle.tool_output_binding()?)
            .await;
        let runtime_context =
            crate::tools::call::lifecycle::runtime::current_tool_runtime_context();
        let workspace = crate::tools::call::lifecycle::runtime::ToolWorkspaceScope {
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
        let request_agent_id = runtime_context
            .as_ref()
            .and_then(|runtime| runtime.agent_id.clone());
        let source_fields = runtime_context
            .map(|runtime| runtime.source_fields)
            .unwrap_or_default();
        let executions = self.background_executions.clone();
        let process_tool = crate::toolset::is_bash_tool_name(target_name);
        let process_recorder =
            executions.process_recorder(&execution_call_id, &execution_tool_doc_id);
        tokio::spawn(crate::plugin::approval::carry_interactive(async move {
            let execution = AssertUnwindSafe(crate::managed_exec::ownership::scope_process_recorder(
                process_recorder,
                crate::tools::call::lifecycle::runtime::scope_tool_request_identity(
                    requester_did,
                    request_node_did,
                    request_agent_id,
                    Some(execution_request_id.clone()),
                    async {
                        crate::tools::call::lifecycle::runtime::scope_request_tool_execution_with_workspace_overlay(
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
                                    crate::tools::call::lifecycle::runtime::call_tool_managed_with_cleanup(
                                        target_tool.as_ref(), target_args, |outcome| {
                                            if let Some(outcome) = outcome.filter(|outcome| crate::managed_exec::ownership::native_terminal_needs_confirmation(Some(outcome))) {
                                                executions.set_stop_receipt(&execution_call_id, &execution_tool_doc_id, crate::tools::call::hook::RetainedStopReceipt::from_outcome(outcome));
                                            }
                                            executions.live_process_evidence(&execution_call_id, &execution_tool_doc_id)
                                                != crate::managed_exec::ownership::LiveProcessEvidence::NeverSpawned
                                        },
                                    ).await
                                } else {
                                    crate::tools::call::lifecycle::runtime::call_tool_managed(target_tool.as_ref(), target_args).await
                                }
                            },
                        )
                        .await
                    },
                ),
            ))
            .catch_unwind()
            .await;

            finish_background_execution(
                lifecycle,
                node,
                live_outputs,
                executions,
                execution_reservation,
                execution,
            )
            .await;
        }));

        Ok(self.skip_tool_result(SPAWN_PROCESS_TOOL_NAME, receipt))
    }

    pub(in crate::tools::call::hook) async fn complete_control_tool_call(
        &self,
        lifecycle: &mut crate::tools::call::lifecycle::ToolCallLifecycle,
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
    pub(in crate::tools::call::hook) async fn fail_control_tool_call(
        &self,
        lifecycle: &mut crate::tools::call::lifecycle::ToolCallLifecycle,
        tool_name: &str,
        raw_result: String,
        failure: crate::tools::call::lifecycle::FailureClass,
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
        lifecycle: &crate::tools::call::lifecycle::ToolCallLifecycle,
    ) -> anyhow::Result<ToolCallHookAction> {
        let message = crate::tools::call::lifecycle::query::load_tool_call_result(
            &crate::config_client::ConfigAccess::local(self.node.clone()),
            lifecycle
                .doc_id()
                .context("lost terminal compare requires physical tool identity")?,
            lifecycle.node_did(),
            lifecycle.session_id(),
            lifecycle.requester_did(),
        )
        .await?;
        Ok(ToolCallHookAction::skip(
            crate::tools::call::lifecycle::query::render_tool_result(&message)?,
        ))
    }
}

#[cfg(test)]
#[path = "background_tools_tests.rs"]
mod tests;
