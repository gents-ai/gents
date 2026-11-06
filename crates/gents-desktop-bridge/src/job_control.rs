use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use crate::error::BridgeError;
use gents_protocol::job::JobControlOutcome;

use crate::types::DesktopJobControlRequest;

/// Sends the authenticated client's control intent to the owning runtime.
pub async fn job_control(
    core: &Arc<ClientCore>,
    request: &DesktopJobControlRequest,
) -> Result<JobControlOutcome, BridgeError> {
    let access = core
        .operator_access(&request.node_did)
        .map_err(BridgeError::classify_transport_error)?;
    gents::tools::call::control::client::send::control_job(
        &access,
        core.node_identity(),
        &request.node_did,
        &request.tool_call_doc_id,
        request.action,
    )
    .await
    .map_err(BridgeError::from)
}
