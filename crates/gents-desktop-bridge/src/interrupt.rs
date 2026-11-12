use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use gents_protocol::operations::request::{RequestInterruptInput, RequestRef};

use crate::error::BridgeError;
use crate::types::{DesktopInterruptRequest, InterruptRequestResult};

/// Interrupt exactly `req.request_id`. Only `"userCancelled"` is
/// operator-authentic; the runtime derives every other cause.
pub async fn interrupt_request(
    core: &Arc<ClientCore>,
    req: &DesktopInterruptRequest,
) -> Result<InterruptRequestResult, BridgeError> {
    if req.cause != "userCancelled" {
        return Err(BridgeError::untyped(format!(
            "operator may only authentically produce cause=\"userCancelled\", got {:?}",
            req.cause
        )));
    }
    let node_did = req
        .node_did
        .clone()
        .ok_or("Select the hosting runtime before interrupting a request.")?;
    let access = core
        .operator_access(&node_did)
        .map_err(BridgeError::classify_transport_error)?;
    let receipt = gents::operations::request::interrupt(
        &access,
        RequestInterruptInput {
            request: RequestRef {
                node_did,
                requester_did: None,
                request_id: req.request_id.clone(),
            },
        },
    )
    .await
    .map_err(|error| {
        BridgeError::classify_transport_error(
            gents_desktop_core::error::request_operation_failure(error),
        )
    })?;
    Ok(InterruptRequestResult {
        request_id: receipt.request_id,
        accepted: true,
        interrupt_requested_at: receipt.interrupt_requested_at,
        already_interrupted: receipt.already_interrupted,
    })
}
