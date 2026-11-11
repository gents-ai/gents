use std::sync::Arc;

use anyhow::{bail, Context, Result};
use gents_desktop_core::client::ClientCore;

use crate::types::{DesktopInterruptRequest, InterruptRequestResult};

/// Interrupt exactly `req.request_id`. Only `"userCancelled"` is
/// operator-authentic; the runtime derives every other cause.
pub async fn interrupt_request(
    core: &Arc<ClientCore>,
    req: &DesktopInterruptRequest,
) -> Result<InterruptRequestResult> {
    if req.cause != "userCancelled" {
        bail!(
            "operator may only authentically produce cause=\"userCancelled\", got {:?}",
            req.cause
        );
    }
    let node_did = req
        .node_did
        .clone()
        .context("Select the hosting runtime before interrupting a request.")?;
    let access = core.operator_access(&node_did)?;
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
