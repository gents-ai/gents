use std::time::Duration;

use anyhow::Result;
use gents_desktop_core::client::{ClientCore, PeerMutationResult};

use super::util::require_trimmed;

pub async fn repair_p2p(core: &ClientCore, settle_delay: Duration) -> Result<()> {
    core.request_p2p_repair().await?;
    tokio::time::sleep(settle_delay).await;
    Ok(())
}

pub async fn remove_peer(core: &ClientCore, peer_id: String) -> Result<PeerMutationResult> {
    let peer_id = require_trimmed("peer_id", peer_id)?;
    core.remove_peer(&peer_id).await
}

pub async fn rename_peer(core: &ClientCore, peer_id: String, label: String) -> Result<()> {
    let peer_id = require_trimmed("peer_id", peer_id)?;
    let label = require_trimmed("label", label)?;
    core.rename_peer(&peer_id, &label).await?;
    Ok(())
}

pub async fn request_status_enrollment(
    core: &ClientCore,
    request: crate::types::EnrollmentStatusRequest,
) -> Result<crate::types::EnrollmentRequestView, crate::error::BridgeError> {
    use crate::error::BridgeError;
    use crate::types::EnrollmentRequestView;
    use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
    let address = request.server_address.trim();
    if address.is_empty() {
        return Err(BridgeError::untyped("server_address is required"));
    }
    let status = fetch_runtime_connection_payload(address)
        .await
        .map_err(|error| BridgeError::classify_transport_error(&error))?;
    let server_label = status
        .get("node_name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned);
    core.request_status_enrollment_with_label(&status, server_label.as_deref())
        .await
        .map(|result| {
            let mut view = EnrollmentRequestView::from(result);
            view.server_label = server_label;
            view
        })
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))
}
