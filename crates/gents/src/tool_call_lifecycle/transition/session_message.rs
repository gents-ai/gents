use super::*;

/// The durable terminal of the request an `agent_new`/`agent_message` row
/// caused (Lean `Recovery.SessionMessageRecoveryCause`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CausedRequestTerminal {
    Completed { output: String },
    Failed { reason: String },
    Dead,
    Interrupted,
    Superseded,
}

impl CausedRequestTerminal {
    pub(crate) fn tool_state(&self) -> ToolCallState {
        match self {
            Self::Completed { .. } => ToolCallState::Completed,
            Self::Interrupted => ToolCallState::Cancelled,
            Self::Failed { .. } | Self::Dead | Self::Superseded => ToolCallState::Failed,
        }
    }

    pub(crate) fn notification_status(&self) -> &'static str {
        match self.tool_state() {
            ToolCallState::Completed => "completed",
            ToolCallState::Cancelled => "cancelled",
            _ => "failed",
        }
    }

    pub(crate) fn completion_reason(&self) -> Option<&'static str> {
        match self {
            Self::Completed { .. } => None,
            Self::Failed { .. } => Some("request_failed"),
            Self::Dead => Some("request_dead"),
            Self::Interrupted => Some("request_interrupted"),
            Self::Superseded => Some("request_superseded"),
        }
    }

    pub(crate) fn output(&self) -> &str {
        match self {
            Self::Completed { output } => output,
            Self::Failed { reason } => reason,
            Self::Dead => "the started request died before it completed",
            Self::Interrupted => "the started request was interrupted",
            Self::Superseded => "the started request was superseded",
        }
    }
}

impl ToolCallLifecycle {
    /// Running -> terminal carrying the output of the request it caused;
    /// returns whether this caller won the running compare.
    pub(crate) async fn settle_session_message(
        &mut self,
        terminal: &CausedRequestTerminal,
    ) -> Result<bool> {
        anyhow::ensure!(
            self.is_session_message() && self.await_mode == AwaitMode::Background,
            "session-message settlement requires a background agent_new/agent_message row"
        );
        let state = terminal.tool_state();
        self.ensure_state(&[ToolCallState::Running, state], "settle_session_message")?;
        let fields = super::super::delivery::TerminalFields {
            state,
            failure: (state == ToolCallState::Failed).then_some(FailureClass::External),
            cancel: (state == ToolCallState::Cancelled).then_some(CancelCause::Interrupted),
            completion_reason: terminal.completion_reason(),
        };
        let updated = self
            .terminalize_bridge_with_delivery(
                ToolCallState::Running,
                fields,
                terminal.output(),
                "tool_call.settle_session_message",
            )
            .await?;
        if !updated {
            self.sync_after_lost_running_compare("settle_session_message")
                .await?;
            return Ok(false);
        }
        Ok(true)
    }
}
