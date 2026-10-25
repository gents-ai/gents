use super::{CancelCause, ToolCallState, TransitionError};

type Result<T, E = TransitionError> = std::result::Result<T, E>;

impl super::ToolCallLifecycle {
    pub const CANCEL_DURING_RUN_OUTPUT: &'static str = "tool call cancelled";

    pub async fn cancel_during_run(&mut self, cause: CancelCause) -> Result<bool> {
        self.cancel_during_run_inner(cause, None).await
    }

    pub(crate) async fn cancel_during_run_owned(
        &mut self,
        cause: CancelCause,
        completion_reason: &str,
    ) -> Result<bool> {
        self.cancel_during_run_inner(cause, Some(completion_reason))
            .await
    }

    async fn cancel_during_run_inner(
        &mut self,
        cause: CancelCause,
        completion_reason_override: Option<&str>,
    ) -> Result<bool> {
        self.ensure_state(&[ToolCallState::Running], "cancel_during_run")?;

        let completion_reason = completion_reason_override.unwrap_or(match cause {
            CancelCause::Deadline => "deadline_exceeded",
            CancelCause::Interrupted => "parent_interrupted",
            CancelCause::UserCancelled => "explicit_cancel",
        });
        let fields = super::super::delivery::TerminalFields {
            state: ToolCallState::Cancelled,
            failure: None,
            cancel: Some(cause),
            completion_reason: Some(completion_reason),
        };
        let raw = Self::CANCEL_DURING_RUN_OUTPUT;
        let updated = self
            .terminalize_raw_with_presentation(
                ToolCallState::Running,
                fields,
                raw,
                raw,
                gents_protocol::output::PayloadPresentation::Full,
                "tool_call.cancel_during_run_delivery",
            )
            .await?;
        if !updated {
            self.sync_after_lost_running_compare("cancel_during_run")
                .await?;
            return Ok(false);
        }
        Ok(true)
    }
}
