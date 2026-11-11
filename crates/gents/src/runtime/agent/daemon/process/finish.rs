use anyhow::Result;

use super::super::terminal::{finalize_request_failure, terminalize_request};
use super::{
    request_failure_reason, AgentDaemon, AgentRequest, DefraStreamWriter, ProviderModel,
    RequestInterruptionSource, RequestLifecycle,
};
use crate::observe::runtime_trace::{record_current_failure_class, record_current_request_outcome};
use crate::request::task_hooks::TaskHookRun;

pub(super) struct OwnedWorkEnd {
    pub(super) reason: Option<String>,
    pub(super) release_writer_binding: bool,
    pub(super) interruption_source: RequestInterruptionSource,
    pub(super) skipped_before_work: bool,
}

impl<M> AgentDaemon<M>
where
    M: ProviderModel,
{
    pub(super) async fn finish_task_hooks(
        &mut self,
        request: &AgentRequest,
        lifecycle: &mut RequestLifecycle,
        stream_writer: &DefraStreamWriter,
        run: &TaskHookRun,
        work: OwnedWorkEnd,
    ) -> Result<()> {
        let OwnedWorkEnd {
            reason: work_reason,
            release_writer_binding,
            interruption_source,
            skipped_before_work,
        } = work;
        let final_outcome = run.final_outcome();
        let mut reason = match &final_outcome {
            crate::request::task_hooks::TaskHookOutcome::Failure(
                crate::request::task_hooks::HookPrimaryError::Hook(hook_id),
            ) => Some(run.hook_failure_reason(hook_id)),
            _ => work_reason,
        };
        let cleanup_errors = run.cleanup_errors();
        if !cleanup_errors.is_empty() {
            let note = format!("task cleanup hooks failed: {}", cleanup_errors.join(", "));
            tracing::error!(
                agent_did = %self.agent_config.agent_did,
                request_id = %request.request_id,
                cleanup_errors = %cleanup_errors.join(","),
                "task cleanup hooks failed"
            );
            reason = Some(reason.map_or(note.clone(), |reason| format!("{reason}\n{note}")));
        }

        let terminal = final_outcome.terminal_outcome();
        match final_outcome {
            crate::request::task_hooks::TaskHookOutcome::Success => {
                if skipped_before_work {
                    if let Err(error) = lifecycle.begin_owned_execution(stream_writer).await {
                        self.finalize_failure_before_work(
                            lifecycle,
                            stream_writer,
                            &request_failure_reason(&error),
                            request,
                        )
                        .await;
                        return Ok(());
                    }
                }
                match terminalize_request(lifecycle, stream_writer, terminal, None).await {
                    Ok(true) if skipped_before_work => {
                        record_current_request_outcome("completed");
                        if let Err(error) = crate::tools::workspace::release_writer_binding(
                            &self.node,
                            self.agent_config.agent_identity.as_ref(),
                            request,
                        )
                        .await
                        {
                            tracing::warn!(request_id = %request.request_id, error = %crate::request::failure_text::BoundedErrorChain(error.as_ref()),
                                "failed to release writer workspace binding after skipped task gate");
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        record_current_request_outcome("terminalization_failed");
                        record_current_failure_class(&error);
                        tracing::error!(request_id = %request.request_id, error = %crate::request::failure_text::BoundedErrorChain(error.as_ref()),
                            "failed to atomically terminalize completed request and response");
                    }
                }
            }
            crate::request::task_hooks::TaskHookOutcome::Failure(_) => {
                let reason = reason.unwrap_or_else(|| "request failed".to_string());
                if finalize_request_failure(lifecycle, stream_writer, &reason, &request.request_id)
                    .await
                    && release_writer_binding
                {
                    self.release_failed_writer_binding(request).await;
                }
            }
            crate::request::task_hooks::TaskHookOutcome::Interrupted => {
                record_current_request_outcome("interrupted");
                let reason = reason.unwrap_or_else(|| "interrupted".to_string());
                match terminalize_request(lifecycle, stream_writer, terminal, Some(&reason)).await {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(error) => {
                        record_current_request_outcome("terminalization_failed");
                        record_current_failure_class(&error);
                        tracing::error!(request_id = %request.request_id, error = %crate::request::failure_text::BoundedErrorChain(error.as_ref()),
                            "failed to atomically terminalize interrupted request and response");
                        return Ok(());
                    }
                }
                self.record_request_interruption(request, interruption_source);
                if let Err(error) = crate::tools::workspace::release_writer_binding(
                    &self.node,
                    self.agent_config.agent_identity.as_ref(),
                    request,
                )
                .await
                {
                    tracing::warn!(
                        request_id = %request.request_id,
                        error = %crate::request::failure_text::BoundedErrorChain(error.as_ref()),
                        "failed to release writer workspace binding after interrupt"
                    );
                }
            }
        }
        Ok(())
    }
}
