use anyhow::Result;

use super::super::terminal::{finalize_request_failure, terminalize_request};
use super::{
    request_failure_reason, AgentDaemon, AgentRequest, DefraStreamWriter, ProviderModel,
    RequestLifecycle,
};
use crate::observe::runtime_trace::{record_current_failure_class, record_current_request_outcome};
use crate::request::lifecycle::RequestTerminalOutcome;

impl<M> AgentDaemon<M>
where
    M: ProviderModel,
{
    pub(super) async fn complete_already_sealed(
        &mut self,
        request: &AgentRequest,
        lifecycle: &mut RequestLifecycle,
        stream_writer: &DefraStreamWriter,
    ) -> Result<()> {
        if let Err(error) = lifecycle.begin_owned_execution(stream_writer).await {
            finalize_request_failure(
                lifecycle,
                stream_writer,
                &request_failure_reason(&error),
                &request.request_id,
            )
            .await;
            return Ok(());
        }
        record_current_request_outcome("completed");
        if let Err(error) = lifecycle.validate_owned_execution().await {
            tracing::warn!(request_id = %request.request_id, %error, "stopping workspace completion after execution ownership loss");
            return Ok(());
        }
        if let Err(error) = crate::tools::workspace::seal_on_writer_success(
            &self.node,
            self.agent_config.agent_identity.as_ref(),
            request,
            self.operator_tool_root.as_deref(),
        )
        .await
        {
            record_current_failure_class(&error);
            tracing::error!(
                request_id = %request.request_id,
                error = %crate::request::failure_text::BoundedErrorChain(error.as_ref()),
                "failed to repair workspace seal after writer success"
            );
            finalize_request_failure(
                lifecycle,
                stream_writer,
                &request_failure_reason(&error),
                &request.request_id,
            )
            .await;
            return Ok(());
        }
        if let Err(error) = terminalize_request(
            lifecycle,
            stream_writer,
            RequestTerminalOutcome::Completed,
            None,
        )
        .await
        {
            record_current_request_outcome("terminalization_failed");
            record_current_failure_class(&error);
            tracing::error!(request_id = %request.request_id, error = %crate::request::failure_text::BoundedErrorChain(error.as_ref()),
                "failed to atomically terminalize completed request and response");
        }
        Ok(())
    }
}
