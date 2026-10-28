use super::*;
use crate::config_client::ConfigAccess;
use crate::tool_call_lifecycle::runtime::{
    current_tool_runtime_context, scope_request_tool_execution_with_workspace_overlay,
    ToolWorkspaceScope,
};
use futures::FutureExt;
use gents_loop::session_hook::{ForegroundCall, ForegroundResult};
use std::panic::AssertUnwindSafe;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

impl DefraSessionHook {
    pub(in crate::hook) async fn run_backgroundable_foreground(
        &self,
        name: &str,
        internal_id: &str,
        call: ForegroundCall,
    ) -> ForegroundResult {
        let context = current_tool_runtime_context();
        if !crate::toolset::is_bash_tool_name(name) {
            return ForegroundResult::Settled(
                call(
                    context
                        .map(|value| value.cancellation_token)
                        .unwrap_or_default(),
                )
                .await,
            );
        }
        let Some(context) = context else {
            return ForegroundResult::Settled(call(CancellationToken::new()).await);
        };
        let Some(lifecycle) = self
            .in_flight_lifecycles
            .lock()
            .await
            .get(internal_id)
            .cloned()
        else {
            return ForegroundResult::Settled(call(context.cancellation_token).await);
        };
        let changes = match crate::subscribe_collections(&self.node, ["AgentToolCall"]) {
            Ok(changes) => changes,
            Err(error) => {
                tracing::warn!(%error, "foreground handoff observation unavailable");
                return ForegroundResult::Settled(call(context.cancellation_token).await);
            }
        };
        let parent_token = context.cancellation_token.clone();
        let (answer, result) = oneshot::channel();
        let hook = self.clone();
        let internal_id = internal_id.to_owned();
        let doc = lifecycle
            .doc_id()
            .expect("accepted foreground tool")
            .to_owned();
        let process_token = CancellationToken::new();
        let reservation = match self.background_executions.reserve(
            lifecycle.tool_call_id().to_owned(),
            doc.clone(),
            process_token.clone(),
        ) {
            Ok(reservation) => reservation,
            Err(error) => {
                return ForegroundResult::Settled(
                    crate::tool_call_lifecycle::ToolOutcome::from_tool_call_error(
                        &error.to_string(),
                    ),
                );
            }
        };
        tokio::spawn(crate::plugin::approval::carry_interactive(async move {
            let mut lifecycle = lifecycle;
            let recorder = hook
                .background_executions
                .process_recorder(lifecycle.tool_call_id(), &doc);
            let workspace = ToolWorkspaceScope {
                runtime_home: context.runtime_home,
                workspace_cwd: context.workspace_cwd,
                workspace_root: context.workspace_root,
                workspace_authority: context.workspace_authority,
                workspace_artifact: context.workspace_artifact,
            };
            let execution =
                AssertUnwindSafe(crate::managed_exec::ownership::scope_process_recorder(
                    recorder,
                    scope_request_tool_execution_with_workspace_overlay(
                        context.deadline_at,
                        process_token.clone(),
                        workspace,
                        None,
                        context.session_id,
                        context.correlation,
                        context.source_fields,
                        false,
                        call(process_token.clone()),
                    ),
                ))
                .catch_unwind();
            tokio::pin!(execution);
            let mut changes = Some(changes);
            let mut answer = Some(answer);
            let mut backgrounded = false;
            let mut inspect = true;
            loop {
                if inspect && !backgrounded {
                    inspect = false;
                    let query = format!(
                        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ background_requested_at }} }}"#,
                        crate::graphql::escape_graphql_string(&doc)
                    );
                    match ConfigAccess::local(hook.node.clone()).execute(&query).await {
                        Ok(value)
                            if value
                                .pointer("/data/AgentToolCall/0/background_requested_at")
                                .is_some_and(|value| value.as_str().is_some()) =>
                        {
                            let receipt = json!({
                                "tool_call_id": lifecycle.tool_call_id(),
                                "status": "running", "await_mode": "background",
                                "message": "The same process continues in the background. Its result will arrive in this conversation."
                            }).to_string();
                            let requested_at = value
                                .pointer("/data/AgentToolCall/0/background_requested_at")
                                .and_then(serde_json::Value::as_str)
                                .unwrap()
                                .to_owned();
                            // Cancellation drains this same request-local map. Hold it through
                            // the receipt commit so it cannot terminalize a stale foreground copy.
                            let mut in_flight = hook.in_flight_lifecycles.lock().await;
                            let admitted = if in_flight.contains_key(&internal_id)
                                && !parent_token.is_cancelled()
                            {
                                lifecycle.background_with_receipt(&receipt).await
                            } else {
                                Err(anyhow::anyhow!("foreground request already stopped"))
                            };
                            match admitted {
                                Ok(()) => {
                                    // The receipt and mode commit before releasing the parent. Once
                                    // committed, dropping the parent's receiver must not cancel this process.
                                    backgrounded = true;
                                    changes = None;
                                    in_flight.remove(&internal_id);
                                    if let Some(answer) = answer.take() {
                                        let _ =
                                            answer.send(ForegroundResult::Backgrounded(receipt));
                                    }
                                }
                                Err(error) => {
                                    drop(in_flight);
                                    tracing::warn!(%error, tool_call_doc_id = %doc, "foreground background handoff refused");
                                    if let Err(error) = lifecycle
                                        .clear_background_request(Some(&requested_at))
                                        .await
                                    {
                                        tracing::warn!(%error, "could not clear refused background request");
                                    }
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!(%error, tool_call_doc_id = %doc,
                            "foreground background request read failed"),
                    }
                }
                tokio::select! {
                    biased;
                    _ = async {
                        tokio::select! {
                            _ = parent_token.cancelled() => {},
                            _ = async {
                                if let Some(answer) = answer.as_mut() { answer.closed().await }
                                else { std::future::pending::<()>().await }
                            } => {},
                        }
                    }, if !backgrounded => {
                        process_token.cancel();
                        if let Err(error) = lifecycle.clear_background_request(None).await {
                            tracing::warn!(%error, "could not clear cancelled background request");
                        }
                        let outcome = execution.await.unwrap_or_else(|_| crate::tool_call_lifecycle::ToolOutcome::tool_returned_error("native command panicked"));
                        if let Some(answer) = answer.take() { let _ = answer.send(ForegroundResult::Settled(outcome)); }
                        hook.background_executions.release_process_record(lifecycle.tool_call_id(), &doc).await;
                        drop(reservation);
                        return;
                    }
                    outcome = &mut execution => {
                        if backgrounded {
                            super::background_tools::finish_background_execution(
                                lifecycle, hook.node.clone(), hook.background_live_outputs.clone(),
                                hook.background_executions.clone(), reservation, outcome,
                            ).await;
                        } else {
                            let outcome = outcome.unwrap_or_else(|_| crate::tool_call_lifecycle::ToolOutcome::tool_returned_error("native command panicked"));
                            if let Some(answer) = answer.take() {
                                let _ = answer.send(ForegroundResult::Settled(outcome));
                            }
                            hook.background_executions.release_process_record(lifecycle.tool_call_id(), &doc).await;
                            drop(reservation);
                        }
                        return;
                    }
                    batch = async {
                        match changes.as_mut() {
                            Some(changes) => changes.recv().await,
                            None => std::future::pending().await,
                        }
                    }, if !backgrounded => match batch {
                        Some(batch) => inspect = batch.resync_required
                            || batch.changes.iter().any(|change| change.doc_id == doc),
                        None => changes = None,
                    },
                }
            }
        }));
        result.await.unwrap_or_else(|_| {
            ForegroundResult::Settled(
                crate::tool_call_lifecycle::ToolOutcome::tool_returned_error(
                    "native execution owner stopped",
                ),
            )
        })
    }
}
