use super::*;

async fn project_background_completion_if_owned<F>(
    won_terminal_compare: bool,
    projection: F,
) -> Option<F::Output>
where
    F: std::future::Future,
{
    if won_terminal_compare {
        Some(projection.await)
    } else {
        None
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

const BACKGROUND_TIMEOUT_NOTIFICATION: (&str, &str) = ("failed", "deadline_exceeded");

struct FinishIdentity {
    call_id: String,
    tool_doc_id: String,
    session_id: String,
    tool_name: String,
    request_doc_id: String,
    node_did: String,
    requester_did: Option<String>,
}

#[derive(Clone, Copy)]
struct FinishCtx<'a> {
    node: &'a std::sync::Arc<defra_node::Node>,
    live_outputs: &'a crate::tools::call::hook::BackgroundLiveOutputState,
    executions: &'a crate::tools::call::hook::BackgroundExecutionRegistry,
    ids: &'a FinishIdentity,
}

type BackgroundExecutionResult =
    Result<gents_loop::tool_call_lifecycle::ToolOutcome, Box<dyn std::any::Any + Send>>;

fn resolve_execution(
    lifecycle: &ToolCallLifecycle,
    executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
    execution: BackgroundExecutionResult,
) -> BackgroundExecutionResult {
    let execution = if let Some(doc) = lifecycle.doc_id() {
        match executions.finalize_pending_failure(lifecycle.tool_call_id(), doc) {
            Some(outcome) => Ok(outcome),
            None => execution,
        }
    } else {
        execution
    };
    if crate::tools::native::is_bash_tool_name(lifecycle.tool_name()) {
        let outcome = execution.as_ref().ok();
        if crate::tools::exec::ownership::native_terminal_needs_confirmation(outcome) {
            let receipt = match outcome {
                Some(outcome) => {
                    crate::tools::call::hook::RetainedStopReceipt::from_outcome(outcome)
                }
                None => crate::tools::call::hook::RetainedStopReceipt::Failure(
                    gents_loop::tool_call_lifecycle::ToolOutcome::tool_returned_error(
                        "native command panicked",
                    ),
                ),
            };
            if let Some(doc) = lifecycle.doc_id() {
                executions.set_stop_receipt(lifecycle.tool_call_id(), doc, receipt);
            }
        }
    }
    execution
}

async fn settle_native_terminal(
    ctx: FinishCtx<'_>,
    lifecycle: ToolCallLifecycle,
    execution_reservation: crate::tools::call::hook::BackgroundExecutionReservation,
    execution: BackgroundExecutionResult,
) {
    let FinishCtx {
        executions, ids, ..
    } = ctx;
    let FinishIdentity {
        call_id: execution_call_id,
        tool_doc_id: execution_tool_doc_id,
        ..
    } = ids;
    let confirmed = match executions.process_record(execution_call_id, execution_tool_doc_id) {
        Some(record) => matches!(
            record
                .identity
                .await_signalled_group_exit(crate::tools::exec::ownership::KILL_GRACE)
                .await,
            crate::tools::exec::ownership::ProcessObservation::Exited
        ),
        None => !matches!(
            executions.live_process_evidence(execution_call_id, execution_tool_doc_id),
            crate::tools::exec::ownership::LiveProcessEvidence::SpawnedUnverified
        ),
    };
    let receipt = match execution.as_ref().ok() {
        Some(gents_loop::tool_call_lifecycle::ToolOutcome::Cancelled) => {
            crate::tools::call::hook::RetainedStopReceipt::Cancel(
                crate::session::tool_call_state::CancelCause::Interrupted,
                executions.cancellation_reason(execution_call_id, execution_tool_doc_id),
            )
        }
        Some(gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { .. }) => {
            crate::tools::call::hook::RetainedStopReceipt::Timeout
        }
        Some(outcome @ gents_loop::tool_call_lifecycle::ToolOutcome::Failed { .. }) => {
            crate::tools::call::hook::RetainedStopReceipt::Failure(outcome.clone())
        }
        None => crate::tools::call::hook::RetainedStopReceipt::Failure(
            gents_loop::tool_call_lifecycle::ToolOutcome::tool_returned_error(
                "native command panicked",
            ),
        ),
        Some(gents_loop::tool_call_lifecycle::ToolOutcome::Completed(_)) => {
            unreachable!("completed execution does not require stop confirmation")
        }
    };
    executions
        .retain_terminal_settlement(lifecycle, execution_reservation, receipt, confirmed)
        .await;
}

async fn finish_timed_out(ctx: FinishCtx<'_>, lifecycle: &mut ToolCallLifecycle) {
    let FinishCtx { node, ids, .. } = ctx;
    let FinishIdentity {
        call_id: execution_call_id,
        tool_doc_id: execution_tool_doc_id,
        session_id: execution_session_id,
        tool_name: execution_tool_name,
        ..
    } = ids;
    let won_terminal_compare = match lifecycle.timeout().await {
        Ok(updated) => updated,
        Err(error) => {
            tracing::warn!(
                tool_call_id = %execution_call_id,
                error = %error,
                "failed to terminalize timed-out background tool"
            );
            false
        }
    };
    if let Some(Err(error)) = project_background_completion_if_owned(won_terminal_compare, async {
        let execution_request_id = lifecycle.calling_request_id().await?;
        crate::tools::background::completion::append_background_tool_completion(
            node,
            &lifecycle.access()?,
            execution_session_id,
            &execution_request_id,
            execution_tool_doc_id,
            execution_tool_name,
            BACKGROUND_TIMEOUT_NOTIFICATION.0,
            "",
            Some(BACKGROUND_TIMEOUT_NOTIFICATION.1),
            gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        )
        .await
    })
    .await
    {
        tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append timed-out background tool notification");
    }
}

async fn finish_cancelled(ctx: FinishCtx<'_>, lifecycle: &mut ToolCallLifecycle) {
    let FinishCtx {
        node,
        executions,
        ids,
        ..
    } = ctx;
    let FinishIdentity {
        call_id: execution_call_id,
        tool_doc_id: execution_tool_doc_id,
        session_id: execution_session_id,
        tool_name: execution_tool_name,
        ..
    } = ids;
    let cancel_reason = executions.cancellation_reason(execution_call_id, execution_tool_doc_id);
    let won_terminal_compare = match lifecycle
        .cancel_during_run_owned(CancelCause::UserCancelled, &cancel_reason)
        .await
    {
        Ok(updated) => updated,
        Err(error) => {
            tracing::warn!(
                tool_call_id = %execution_call_id,
                error = %error,
                "failed to terminalize cancelled background tool"
            );
            false
        }
    };
    if let Some(Err(error)) = project_background_completion_if_owned(won_terminal_compare, async {
        let execution_request_id = lifecycle.calling_request_id().await?;
        crate::tools::background::completion::append_background_tool_completion(
            node,
            &lifecycle.access()?,
            execution_session_id,
            &execution_request_id,
            execution_tool_doc_id,
            execution_tool_name,
            "cancelled",
            "",
            Some(&cancel_reason),
            gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        )
        .await
    })
    .await
    {
        tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append cancelled background tool notification");
    }
}

async fn finish_completed(ctx: FinishCtx<'_>, lifecycle: &mut ToolCallLifecycle, output: String) {
    let FinishCtx {
        node,
        live_outputs,
        ids,
        ..
    } = ctx;
    let FinishIdentity {
        call_id: execution_call_id,
        tool_doc_id: execution_tool_doc_id,
        session_id: execution_session_id,
        tool_name: execution_tool_name,
        request_doc_id: execution_request_doc_id,
        node_did: execution_node_did,
        requester_did: execution_requester_did,
    } = ids;
    let presentation = match live_outputs
        .registry
        .take_prepared_presentation(execution_tool_doc_id, &output)
        .await
    {
        Ok(presentation) => presentation,
        Err(error) => {
            tracing::warn!(
                tool_call_id = %execution_call_id,
                error = %error,
                "failed to resolve background tool presentation"
            );
            None
        }
    };
    let won_terminal_compare = match lifecycle
        .complete_owned(output.as_str(), presentation)
        .await
    {
        Ok(updated) => updated,
        Err(error) => {
            tracing::warn!(
                tool_call_id = %execution_call_id,
                error = %error,
                "failed to complete background tool"
            );
            false
        }
    };
    let notification_result = if won_terminal_compare {
        match crate::session::tool_output_rows::observe_canonical_tool_output_with_access(
            &crate::ConfigAccess::local_as(node.clone(), lifecycle.actor().clone()),
            execution_tool_doc_id,
            execution_request_doc_id,
            execution_session_id,
            execution_node_did,
            execution_requester_did.as_deref(),
        )
        .await
        {
            Ok(output) => output.into_text(),
            Err(error) => {
                tracing::warn!(
                    tool_call_id = %execution_call_id,
                    error = %error,
                    "failed to reconstruct completed background tool output"
                );
                String::new()
            }
        }
    } else {
        String::new()
    };
    if let Some(Err(error)) = project_background_completion_if_owned(won_terminal_compare, async {
        let execution_request_id = lifecycle.calling_request_id().await?;
        crate::tools::background::completion::append_background_tool_completion(
            node,
            &lifecycle.access()?,
            execution_session_id,
            &execution_request_id,
            execution_tool_doc_id,
            execution_tool_name,
            "completed",
            &notification_result,
            None,
            gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        )
        .await
    })
    .await
    {
        tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append completed background tool notification");
    }
}

async fn finish_failed(
    ctx: FinishCtx<'_>,
    lifecycle: &mut ToolCallLifecycle,
    failure_class: gents_loop::tool_call_lifecycle::FailureClass,
    reason: String,
) {
    let FinishCtx {
        node,
        live_outputs,
        ids,
        ..
    } = ctx;
    let FinishIdentity {
        call_id: execution_call_id,
        tool_doc_id: execution_tool_doc_id,
        session_id: execution_session_id,
        tool_name: execution_tool_name,
        request_doc_id: execution_request_doc_id,
        node_did: execution_node_did,
        requester_did: execution_requester_did,
    } = ids;
    let presentation = match live_outputs
        .registry
        .take_prepared_presentation(execution_tool_doc_id, &reason)
        .await
    {
        Ok(presentation) => presentation,
        Err(error) => {
            tracing::warn!(
                tool_call_id = %execution_call_id,
                error = %error,
                "failed to resolve failed background tool presentation"
            );
            None
        }
    };
    let won_terminal_compare = match lifecycle
        .fail_owned(&reason, failure_class, presentation)
        .await
    {
        Ok(updated) => updated,
        Err(error) => {
            tracing::warn!(
                tool_call_id = %execution_call_id,
                error = %error,
                "failed to fail background tool"
            );
            false
        }
    };
    let notification_result = if won_terminal_compare {
        match crate::session::tool_output_rows::observe_canonical_tool_output_with_access(
            &crate::ConfigAccess::local_as(node.clone(), lifecycle.actor().clone()),
            execution_tool_doc_id,
            execution_request_doc_id,
            execution_session_id,
            execution_node_did,
            execution_requester_did.as_deref(),
        )
        .await
        {
            Ok(output) => output.into_text(),
            Err(error) => {
                tracing::warn!(
                    tool_call_id = %execution_call_id,
                    error = %error,
                    "failed to reconstruct failed background tool output"
                );
                String::new()
            }
        }
    } else {
        String::new()
    };
    if let Some(Err(error)) = project_background_completion_if_owned(won_terminal_compare, async {
        let execution_request_id = lifecycle.calling_request_id().await?;
        crate::tools::background::completion::append_background_tool_completion(
            node,
            &lifecycle.access()?,
            execution_session_id,
            &execution_request_id,
            execution_tool_doc_id,
            execution_tool_name,
            "failed",
            &notification_result,
            Some("tool_failed"),
            gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        )
        .await
    })
    .await
    {
        tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append failed background tool notification");
    }
}

async fn finish_panicked(
    ctx: FinishCtx<'_>,
    lifecycle: &mut ToolCallLifecycle,
    panic: Box<dyn std::any::Any + Send>,
) {
    let FinishCtx { node, ids, .. } = ctx;
    let FinishIdentity {
        call_id: execution_call_id,
        tool_doc_id: execution_tool_doc_id,
        session_id: execution_session_id,
        tool_name: execution_tool_name,
        ..
    } = ids;
    let panic = panic_payload_message(panic.as_ref());
    let reason = format!("background tool panicked: {panic}");
    let won_terminal_compare = match lifecycle
        .fail_owned(&reason, FailureClass::External, None)
        .await
    {
        Ok(updated) => updated,
        Err(error) => {
            tracing::warn!(
                tool_call_id = %execution_call_id,
                error = %error,
                "failed to terminalize panicking background tool"
            );
            false
        }
    };
    if let Some(Err(error)) = project_background_completion_if_owned(won_terminal_compare, async {
        let execution_request_id = lifecycle.calling_request_id().await?;
        crate::tools::background::completion::append_background_tool_completion(
            node,
            &lifecycle.access()?,
            execution_session_id,
            &execution_request_id,
            execution_tool_doc_id,
            execution_tool_name,
            "failed",
            &reason,
            Some("tool_panicked"),
            gents_protocol::request_lifecycle::RequestHopCause::Continuation,
        )
        .await
    })
    .await
    {
        tracing::warn!(
            tool_call_id = %execution_call_id,
            error = %error,
            "failed to append panicking background tool notification"
        );
    }
}

pub(in crate::tools::call::hook::persistence) async fn finish_background_execution(
    mut lifecycle: ToolCallLifecycle,
    node: std::sync::Arc<defra_node::Node>,
    live_outputs: crate::tools::call::hook::BackgroundLiveOutputState,
    executions: crate::tools::call::hook::BackgroundExecutionRegistry,
    execution_reservation: crate::tools::call::hook::BackgroundExecutionReservation,
    execution: Result<gents_loop::tool_call_lifecycle::ToolOutcome, Box<dyn std::any::Any + Send>>,
) {
    let execution = resolve_execution(&lifecycle, &executions, execution);
    let identity = FinishIdentity {
        call_id: lifecycle.tool_call_id().to_owned(),
        tool_doc_id: lifecycle
            .doc_id()
            .expect("registered execution identity")
            .to_owned(),
        session_id: lifecycle.session_id().to_owned(),
        tool_name: lifecycle.tool_name().to_owned(),
        request_doc_id: lifecycle
            .request_doc_id()
            .expect("registered request identity")
            .to_owned(),
        node_did: lifecycle.node_did().to_owned(),
        requester_did: lifecycle.requester_did().map(str::to_owned),
    };
    let FinishIdentity {
        call_id: execution_call_id,
        tool_doc_id: execution_tool_doc_id,
        tool_name: execution_tool_name,
        ..
    } = &identity;
    lifecycle.stage_plugin_receipt(
        live_outputs
            .registry
            .plugin_receipt(execution_tool_doc_id)
            .await,
    );
    lifecycle.stage_deferred_writes(
        live_outputs
            .registry
            .deferred_writes(execution_tool_doc_id)
            .await,
    );
    let ctx = FinishCtx {
        node: &node,
        live_outputs: &live_outputs,
        executions: &executions,
        ids: &identity,
    };
    if crate::tools::native::is_bash_tool_name(execution_tool_name)
        && crate::tools::exec::ownership::native_terminal_needs_confirmation(
            execution.as_ref().ok(),
        )
    {
        settle_native_terminal(ctx, lifecycle, execution_reservation, execution).await;
        return;
    }
    match execution {
        Ok(outcome) => match outcome {
            gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { .. } => {
                finish_timed_out(ctx, &mut lifecycle).await
            }
            gents_loop::tool_call_lifecycle::ToolOutcome::Cancelled => {
                finish_cancelled(ctx, &mut lifecycle).await
            }
            gents_loop::tool_call_lifecycle::ToolOutcome::Completed(output) => {
                finish_completed(ctx, &mut lifecycle, output).await
            }
            gents_loop::tool_call_lifecycle::ToolOutcome::Failed {
                class: failure_class,
                text: reason,
                ..
            } => finish_failed(ctx, &mut lifecycle, failure_class, reason).await,
        },
        Err(panic) => finish_panicked(ctx, &mut lifecycle, panic).await,
    }

    live_outputs.remove(execution_tool_doc_id).await;
    executions
        .release_process_record(execution_call_id, execution_tool_doc_id)
        .await;
    drop(execution_reservation);
}
