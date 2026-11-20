use std::sync::Arc;

use crate::client_runtime::ClientRuntime;
use gents_protocol::job::JobControlOutcome;
use gents_store::client_error::BridgeError;

use crate::types::DesktopJobControlRequest;

/// Sends the authenticated client's control intent to the owning runtime.
pub async fn job_control(
    core: &Arc<ClientRuntime>,
    request: &DesktopJobControlRequest,
) -> Result<JobControlOutcome, BridgeError> {
    let access = core
        .operator_access(&request.node_did)
        .map_err(BridgeError::classify_transport_error)?;
    gents_toolcalls::call::control::client::send::control_job(
        &access,
        core.node_identity(),
        &request.node_did,
        &request.tool_call_doc_id,
        request.action,
    )
    .await
    .map_err(crate::error::control_send_error)
}
