use std::sync::Arc;

use crate::client_runtime::ClientRuntime;
use gents_protocol::process_control::{OperatorProcessControlRequest, ProcessControlOutcome};
use gents_store::client_error::BridgeError;

/// Native processes live in the owning runtime, not in the client's replica.
/// The endpoint authenticates the client's identity.
pub async fn process_control(
    core: &Arc<ClientRuntime>,
    request: &OperatorProcessControlRequest,
) -> Result<ProcessControlOutcome, BridgeError> {
    let access = core
        .operator_access(&request.node_did)
        .map_err(BridgeError::classify_transport_error)?;
    gents_toolcalls::call::control::client::send::control_process(
        &access,
        core.node_identity(),
        &request.node_did,
        &request.tool_call_doc_id,
        (&request.session_id, &request.tool_call_id),
    )
    .await
    .map_err(crate::error::control_send_error)
}
