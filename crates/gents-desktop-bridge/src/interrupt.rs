use std::sync::Arc;

use gents_desktop_core::client::ClientCore;

use crate::types::{DesktopInterruptRequest, InterruptRequestResult};

/// Interrupt exactly `req.request_id`. Only `"userCancelled"` is
/// operator-authentic; the runtime derives every other cause.
pub async fn interrupt_request(
    core: &Arc<ClientCore>,
    req: &DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    if req.cause != "userCancelled" {
        return Err(format!(
            "operator may only authentically produce cause=\"userCancelled\", got {:?}",
            req.cause
        ));
    }
    let node_did = req
        .node_did
        .clone()
        .ok_or("Select the hosting runtime before interrupting a request.")?;
    let access = core
        .operator_access(&node_did)
        .map_err(|error| error.to_string())?;
    let interrupt =
        gents_desktop_core::client::interrupt_request_on(&access, &node_did, &req.request_id)
            .await?;
    Ok(InterruptRequestResult {
        request_id: req.request_id.clone(),
        accepted: true,
        interrupt_requested_at: interrupt.interrupt_requested_at,
        already_interrupted: interrupt.already_interrupted,
    })
}
