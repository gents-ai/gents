use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use crate::error::BridgeError;
use gents_protocol::process_control::{OperatorProcessControlRequest, ProcessControlOutcome};

/// Native processes live in the owning runtime, not in the client's replica.
/// The endpoint authenticates the client's identity.
pub async fn process_control(
    core: &Arc<ClientCore>,
    request: &OperatorProcessControlRequest,
) -> Result<ProcessControlOutcome, BridgeError> {
    let access = core
        .operator_access(&request.node_did)
        .map_err(BridgeError::classify_transport_error)?;
    gents::tools::call::control::client::send::control_process(
        &access,
        core.node_identity(),
        &request.node_did,
        &request.tool_call_doc_id,
        (&request.session_id, &request.tool_call_id),
    )
    .await
    .map_err(BridgeError::from)
}
