use std::time::Duration;

use anyhow::Result;
use gents_desktop_core::client::{BearerPairingResult, ClientCore, PeerMutationResult};

use super::super::types::{BearerPairingRequest, PeerAddRequest};
use super::util::{require_trimmed, trim_optional};

pub async fn add_peer(core: &ClientCore, request: PeerAddRequest) -> Result<()> {
    let label = require_trimmed("label", request.label)?;
    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let addr = require_trimmed("addr", request.addr)?;
    let graphql = trim_optional(request.graphql);
    let default_behavior_id = trim_optional(request.default_behavior_id);
    core.add_peer(
        &label,
        &addr,
        &agent_did,
        graphql.as_deref(),
        default_behavior_id.as_deref(),
    )
    .await?;
    Ok(())
}

pub async fn pair_bearer(
    core: &ClientCore,
    request: BearerPairingRequest,
) -> Result<BearerPairingResult> {
    let token = require_trimmed("token", request.token)?;
    let label = trim_optional(request.label);
    core.pair_with_bearer_invite(&token, label.as_deref()).await
}

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
