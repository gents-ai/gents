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
