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

pub(in crate::tools::call::hook::persistence) async fn finish_background_execution(
    mut lifecycle: ToolCallLifecycle,
    node: std::sync::Arc<defra_node::EmbeddedNode>,
    live_outputs: crate::tools::call::hook::BackgroundLiveOutputState,
    executions: crate::tools::call::hook::BackgroundExecutionRegistry,
    execution_reservation: crate::tools::call::hook::BackgroundExecutionReservation,
    execution: Result<crate::tools::call::lifecycle::ToolOutcome, Box<dyn std::any::Any + Send>>,
) {
    let execution = if let Some(doc) = lifecycle.doc_id() {
        match executions.finalize_pending_failure(lifecycle.tool_call_id(), doc) {
            Some(outcome) => Ok(outcome),
            None => execution,
        }
    } else {
        execution
    };
    if crate::toolset::is_bash_tool_name(lifecycle.tool_name()) {
        let outcome = execution.as_ref().ok();
        if crate::managed_exec::ownership::native_terminal_needs_confirmation(outcome) {
            let receipt = match outcome {
                Some(outcome) => {
                    crate::tools::call::hook::RetainedStopReceipt::from_outcome(outcome)
                }
                None => crate::tools::call::hook::RetainedStopReceipt::Failure(
                    crate::tools::call::lifecycle::ToolOutcome::tool_returned_error(
                        "native command panicked",
                    ),
                ),
            };
            if let Some(doc) = lifecycle.doc_id() {
                executions.set_stop_receipt(lifecycle.tool_call_id(), doc, receipt);
            }
        }
    }
    let execution_call_id = lifecycle.tool_call_id().to_owned();
    let execution_tool_doc_id = lifecycle
        .doc_id()
        .expect("registered execution identity")
        .to_owned();
    let execution_session_id = lifecycle.session_id().to_owned();
    let execution_tool_name = lifecycle.tool_name().to_owned();
    let execution_request_doc_id = lifecycle
        .request_doc_id()
        .expect("registered request identity")
        .to_owned();
    let execution_node_did = lifecycle.node_did().to_owned();
    let execution_requester_did = lifecycle.requester_did().map(str::to_owned);

    lifecycle.stage_plugin_receipt(
        live_outputs
            .registry
            .plugin_receipt(&execution_tool_doc_id)
            .await,
    );
    lifecycle.stage_deferred_writes(
        live_outputs
            .registry
            .deferred_writes(&execution_tool_doc_id)
            .await,
    );

    if crate::toolset::is_bash_tool_name(&execution_tool_name)
        && crate::managed_exec::ownership::native_terminal_needs_confirmation(
            execution.as_ref().ok(),
        )
    {
        let confirmed = match executions.process_record(&execution_call_id, &execution_tool_doc_id)
        {
            Some(record) => matches!(
                record
                    .identity
                    .await_signalled_group_exit(crate::managed_exec::ownership::KILL_GRACE)
                    .await,
                crate::managed_exec::ownership::ProcessObservation::Exited
            ),
            None => !matches!(
                executions.live_process_evidence(&execution_call_id, &execution_tool_doc_id),
                crate::managed_exec::ownership::LiveProcessEvidence::SpawnedUnverified
            ),
        };
        {
            let receipt = match execution.as_ref().ok() {
                Some(crate::tools::call::lifecycle::ToolOutcome::Cancelled) => {
                    crate::tools::call::hook::RetainedStopReceipt::Cancel(
                        crate::tools::call::lifecycle::CancelCause::Interrupted,
                        executions.cancellation_reason(&execution_call_id, &execution_tool_doc_id),
                    )
                }
                Some(crate::tools::call::lifecycle::ToolOutcome::TimedOut { .. }) => {
                    crate::tools::call::hook::RetainedStopReceipt::Timeout
                }
                Some(outcome @ crate::tools::call::lifecycle::ToolOutcome::Failed { .. }) => {
                    crate::tools::call::hook::RetainedStopReceipt::Failure(outcome.clone())
                }
                None => crate::tools::call::hook::RetainedStopReceipt::Failure(
                    crate::tools::call::lifecycle::ToolOutcome::tool_returned_error(
                        "native command panicked",
                    ),
                ),
                Some(crate::tools::call::lifecycle::ToolOutcome::Completed(_)) => {
                    unreachable!("completed execution does not require stop confirmation")
                }
            };
            executions
                .retain_terminal_settlement(lifecycle, execution_reservation, receipt, confirmed)
                .await;
            return;
        }
    }

    match execution {
        Ok(outcome) => match outcome {
            crate::tools::call::lifecycle::ToolOutcome::TimedOut { .. } => {
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
                if let Some(Err(error)) =
                    project_background_completion_if_owned(won_terminal_compare, async {
                        let execution_request_id = lifecycle.calling_request_id().await?;
                        crate::tools::background::completion::append_background_tool_completion(
                            &node,
                            &execution_session_id,
                            &execution_request_id,
                            &execution_tool_doc_id,
                            &execution_tool_name,
                            BACKGROUND_TIMEOUT_NOTIFICATION.0,
                            "",
                            Some(BACKGROUND_TIMEOUT_NOTIFICATION.1),
                            crate::request::lifecycle::RequestHopCause::Continuation,
                        )
                        .await
                    })
                    .await
                {
                    tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append timed-out background tool notification");
                }
            }
            crate::tools::call::lifecycle::ToolOutcome::Cancelled => {
                let cancel_reason =
                    executions.cancellation_reason(&execution_call_id, &execution_tool_doc_id);
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
                if let Some(Err(error)) =
                    project_background_completion_if_owned(won_terminal_compare, async {
                        let execution_request_id = lifecycle.calling_request_id().await?;
                        crate::tools::background::completion::append_background_tool_completion(
                            &node,
                            &execution_session_id,
                            &execution_request_id,
                            &execution_tool_doc_id,
                            &execution_tool_name,
                            "cancelled",
                            "",
                            Some(&cancel_reason),
                            crate::request::lifecycle::RequestHopCause::Continuation,
                        )
                        .await
                    })
                    .await
                {
                    tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append cancelled background tool notification");
                }
            }
            crate::tools::call::lifecycle::ToolOutcome::Completed(output) => {
                let presentation = match live_outputs
                    .registry
                    .take_prepared_presentation(&execution_tool_doc_id, &output)
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
                    match crate::tools::background::status::canonical_tool_output(
                        node.as_ref(),
                        &execution_tool_doc_id,
                        &execution_request_doc_id,
                        &execution_session_id,
                        &execution_node_did,
                        execution_requester_did.as_deref(),
                    )
                    .await
                    {
                        Ok(output) => output,
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
                if let Some(Err(error)) =
                    project_background_completion_if_owned(won_terminal_compare, async {
                        let execution_request_id = lifecycle.calling_request_id().await?;
                        crate::tools::background::completion::append_background_tool_completion(
                            &node,
                            &execution_session_id,
                            &execution_request_id,
                            &execution_tool_doc_id,
                            &execution_tool_name,
                            "completed",
                            &notification_result,
                            None,
                            crate::request::lifecycle::RequestHopCause::Continuation,
                        )
                        .await
                    })
                    .await
                {
                    tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append completed background tool notification");
                }
            }
            crate::tools::call::lifecycle::ToolOutcome::Failed {
                class: failure_class,
                text: reason,
                ..
            } => {
                let presentation = match live_outputs
                    .registry
                    .take_prepared_presentation(&execution_tool_doc_id, &reason)
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
                    match crate::tools::background::status::canonical_tool_output(
                        node.as_ref(),
                        &execution_tool_doc_id,
                        &execution_request_doc_id,
                        &execution_session_id,
                        &execution_node_did,
                        execution_requester_did.as_deref(),
                    )
                    .await
                    {
                        Ok(output) => output,
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
                if let Some(Err(error)) =
                    project_background_completion_if_owned(won_terminal_compare, async {
                        let execution_request_id = lifecycle.calling_request_id().await?;
                        crate::tools::background::completion::append_background_tool_completion(
                            &node,
                            &execution_session_id,
                            &execution_request_id,
                            &execution_tool_doc_id,
                            &execution_tool_name,
                            "failed",
                            &notification_result,
                            Some("tool_failed"),
                            crate::request::lifecycle::RequestHopCause::Continuation,
                        )
                        .await
                    })
                    .await
                {
                    tracing::warn!(tool_call_id = %execution_call_id, error = %error, "failed to append failed background tool notification");
                }
            }
        },
        Err(panic) => {
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
            if let Some(Err(error)) =
                project_background_completion_if_owned(won_terminal_compare, async {
                    let execution_request_id = lifecycle.calling_request_id().await?;
                    crate::tools::background::completion::append_background_tool_completion(
                        &node,
                        &execution_session_id,
                        &execution_request_id,
                        &execution_tool_doc_id,
                        &execution_tool_name,
                        "failed",
                        &reason,
                        Some("tool_panicked"),
                        crate::request::lifecycle::RequestHopCause::Continuation,
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
    }

    live_outputs.remove(&execution_tool_doc_id).await;
    executions
        .release_process_record(&execution_call_id, &execution_tool_doc_id)
        .await;
    drop(execution_reservation);
}
