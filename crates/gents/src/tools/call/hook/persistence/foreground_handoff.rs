use super::*;
use crate::store::config_client::ConfigAccess;
use crate::tools::call::lifecycle::runtime::{
    current_tool_runtime_context, scope_request_tool_execution_with_workspace_overlay,
    ToolWorkspaceScope,
};
use futures::FutureExt;
use gents_loop::session_hook::{ForegroundCall, ForegroundResult};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

impl DefraSessionHook {
    pub(in crate::tools::call::hook) async fn handoff_unverified_process(
        &self,
        lifecycle: &mut ToolCallLifecycle,
        internal_id: &str,
        doc: &str,
    ) -> anyhow::Result<String> {
        let receipt = json!({"tool_call_id": lifecycle.tool_call_id(), "status": "running", "await_mode": "background", "message": "Process exit could not be confirmed; the call remains available for control."}).to_string();
        lifecycle.background_with_recovery_receipt(&receipt).await?;
        self.in_flight_lifecycles.lock().await.remove(internal_id);
        self.foreground_process_evidence.lock().await.remove(doc);
        Ok(receipt)
    }

    pub(in crate::tools::call::hook) async fn run_backgroundable_foreground(
        &self,
        name: &str,
        internal_id: &str,
        call: ForegroundCall,
    ) -> ForegroundResult {
        let context = current_tool_runtime_context();
        if !crate::tools::native::is_bash_tool_name(name) {
            return ForegroundResult::Settled(
                call(
                    context
                        .map(|value| value.cancellation_token)
                        .unwrap_or_default(),
                    None,
                )
                .await,
            );
        }
        let Some(context) = context else {
            return ForegroundResult::Settled(
                crate::tools::call::lifecycle::ToolOutcome::from_tool_call_error(
                    "native process requires an owned execution context",
                ),
            );
        };
        let Some(lifecycle) = self
            .in_flight_lifecycles
            .lock()
            .await
            .get(internal_id)
            .cloned()
        else {
            return ForegroundResult::Settled(
                crate::tools::call::lifecycle::ToolOutcome::from_tool_call_error(
                    "native process requires an accepted invocation",
                ),
            );
        };
        let changes = match crate::subscribe_collections(&self.node, ["AgentToolCall"]) {
            Ok(changes) => changes,
            Err(error) => {
                tracing::warn!(%error, "foreground handoff observation unavailable");
                return ForegroundResult::Settled(
                    crate::tools::call::lifecycle::ToolOutcome::from_tool_call_error(
                        "native process observation is unavailable",
                    ),
                );
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
                    crate::tools::call::lifecycle::ToolOutcome::from_tool_call_error(
                        &error.to_string(),
                    ),
                );
            }
        };
        // Serialize the pre-spawn ownership registration with interruption sweeps.
        let in_flight = hook.in_flight_lifecycles.lock().await;
        if !in_flight.contains_key(&internal_id) || parent_token.is_cancelled() {
            drop(in_flight);
            drop(reservation);
            return ForegroundResult::Settled(
                crate::tools::call::lifecycle::ToolOutcome::Cancelled,
            );
        }
        let recorder = hook
            .background_executions
            .process_recorder(lifecycle.tool_call_id(), &doc);
        let evidence = hook
            .background_executions
            .live_process_evidence_owner(lifecycle.tool_call_id(), &doc)
            .expect("reserved foreground execution has evidence");
        hook.foreground_process_evidence
            .lock()
            .await
            .insert(doc.clone(), evidence);
        drop(in_flight);
        tokio::spawn(crate::plugin::approval::carry_interactive(async move {
            let mut lifecycle = lifecycle;
            let check_registry = hook.background_executions.clone();
            let check_call = lifecycle.tool_call_id().to_owned();
            let check_doc = doc.clone();
            let cleanup: gents_loop::session_hook::NativeCleanupCheck = Arc::new(move |outcome| {
                if let Some(outcome) = outcome.filter(|outcome| {
                    crate::tools::exec::ownership::native_terminal_needs_confirmation(Some(outcome))
                }) {
                    check_registry.set_stop_receipt(
                        &check_call,
                        &check_doc,
                        crate::tools::call::hook::RetainedStopReceipt::from_outcome(outcome),
                    );
                }
                check_registry.live_process_evidence(&check_call, &check_doc)
                    != crate::tools::exec::ownership::LiveProcessEvidence::NeverSpawned
            });
            let workspace = ToolWorkspaceScope {
                runtime_home: context.runtime_home,
                workspace_cwd: context.workspace_cwd,
                workspace_root: context.workspace_root,
                workspace_authority: context.workspace_authority,
                workspace_artifact: context.workspace_artifact,
            };
            let execution =
                AssertUnwindSafe(crate::tools::exec::ownership::scope_process_recorder(
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
                        call(process_token.clone(), Some(cleanup)),
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
                        crate::store::graphql::escape_graphql_string(&doc)
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
                                    hook.foreground_process_evidence.lock().await.remove(&doc);
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
                        let outcome = execution.await.unwrap_or_else(|_| {
                            if let Some(evidence) = hook.background_executions.live_process_evidence_owner(lifecycle.tool_call_id(), &doc) { evidence.mark_unverified_failure(); }
                            crate::tools::call::lifecycle::ToolOutcome::tool_returned_error("native command panicked")
                        });
                        let outcome = hook.background_executions.finalize_pending_failure(lifecycle.tool_call_id(), &doc).unwrap_or(outcome);
                        if crate::tools::exec::ownership::native_terminal_needs_confirmation(Some(&outcome))
                            && !hook.background_executions.process_exit_confirmed(lifecycle.tool_call_id(), &doc).await {
                            hook.background_executions.set_stop_receipt(lifecycle.tool_call_id(), &doc, crate::tools::call::hook::RetainedStopReceipt::from_outcome(&outcome));
                            match hook.handoff_unverified_process(&mut lifecycle, &internal_id, &doc).await {
                                Ok(receipt) => {
                                    if let Some(answer) = answer.take() { let _ = answer.send(ForegroundResult::Backgrounded(receipt)); }
                                    reservation.retain_stopped_worker(lifecycle, false);
                                }
                                Err(error) => {
                                    tracing::warn!(%error, tool_call_doc_id = %doc, "foreground recovery handoff retained for retry");
                                    reservation.retain_for_recovery(lifecycle);
                                    if let Some(answer) = answer.take() { let _ = answer.send(ForegroundResult::Settled(outcome)); }
                                }
                            }
                            return;
                        }
                        if crate::tools::exec::ownership::native_terminal_needs_confirmation(Some(&outcome)) {
                            let receipt = crate::tools::call::hook::RetainedStopReceipt::from_outcome(&outcome);
                            hook.background_executions.retain_confirmed_worker(lifecycle, reservation, receipt);
                            if let Some(answer) = answer.take() { let _ = answer.send(ForegroundResult::Settled(outcome)); }
                            return;
                        }
                        if hook.background_executions.process_exit_confirmed(lifecycle.tool_call_id(), &doc).await {
                            if let Some(evidence) = hook.background_executions.live_process_evidence_owner(lifecycle.tool_call_id(), &doc) { evidence.exited(); }
                            hook.background_executions.release_process_record(lifecycle.tool_call_id(), &doc).await;
                        }
                        if let Some(answer) = answer.take() { let _ = answer.send(ForegroundResult::Settled(outcome)); }
                        drop(reservation);
                        return;
                    }
                    outcome = &mut execution => {
                        if backgrounded {
                            super::background_tools::finish::finish_background_execution(
                                lifecycle, hook.node.clone(), hook.background_live_outputs.clone(),
                                hook.background_executions.clone(), reservation, outcome,
                            ).await;
                        } else {
                            let outcome = outcome.unwrap_or_else(|_| {
                                if let Some(evidence) = hook.background_executions.live_process_evidence_owner(lifecycle.tool_call_id(), &doc) { evidence.mark_unverified_failure(); }
                                crate::tools::call::lifecycle::ToolOutcome::tool_returned_error("native command panicked")
                            });
                            let outcome = hook.background_executions.finalize_pending_failure(lifecycle.tool_call_id(), &doc).unwrap_or(outcome);
                            let needs_confirmation = crate::tools::exec::ownership::native_terminal_needs_confirmation(Some(&outcome));
                            if needs_confirmation && !hook.background_executions.process_exit_confirmed(lifecycle.tool_call_id(), &doc).await {
                                hook.background_executions.set_stop_receipt(lifecycle.tool_call_id(), &doc, crate::tools::call::hook::RetainedStopReceipt::from_outcome(&outcome));
                                match hook.handoff_unverified_process(&mut lifecycle, &internal_id, &doc).await {
                                    Ok(receipt) => {
                                        if let Some(answer) = answer.take() { let _ = answer.send(ForegroundResult::Backgrounded(receipt)); }
                                        reservation.retain_stopped_worker(lifecycle, false);
                                    }
                                    Err(error) => {
                                        tracing::warn!(%error, tool_call_doc_id = %doc, "foreground recovery handoff retained for retry");
                                        reservation.retain_for_recovery(lifecycle);
                                        if let Some(answer) = answer.take() { let _ = answer.send(ForegroundResult::Settled(outcome)); }
                                    }
                                }
                                return;
                            }
                            if needs_confirmation {
                                let receipt = crate::tools::call::hook::RetainedStopReceipt::from_outcome(&outcome);
                                hook.background_executions.retain_confirmed_worker(lifecycle, reservation, receipt);
                                if let Some(answer) = answer.take() { let _ = answer.send(ForegroundResult::Settled(outcome)); }
                                return;
                            }
                            if hook.background_executions.process_exit_confirmed(lifecycle.tool_call_id(), &doc).await {
                                if let Some(evidence) = hook.background_executions.live_process_evidence_owner(lifecycle.tool_call_id(), &doc) { evidence.exited(); }
                                hook.background_executions.release_process_record(lifecycle.tool_call_id(), &doc).await;
                            }
                            if let Some(answer) = answer.take() {
                                let _ = answer.send(ForegroundResult::Settled(outcome));
                            }
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
                crate::tools::call::lifecycle::ToolOutcome::tool_returned_error(
                    "native execution owner stopped",
                ),
            )
        })
    }
}
