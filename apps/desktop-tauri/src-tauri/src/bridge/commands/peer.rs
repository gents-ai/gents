use std::time::Duration;

use anyhow::Result;
use defra_agent_desktop_core::client::ClientCore;

use super::super::types::PeerAddRequest;
use super::util::{require_trimmed, trim_optional};

pub(crate) async fn add_peer(core: &ClientCore, request: PeerAddRequest) -> Result<()> {
    let label = require_trimmed("label", request.label)?;
    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let addr = require_trimmed("addr", request.addr)?;
    let graphql = trim_optional(request.graphql);
    core.add_peer(&label, &addr, &agent_did, graphql.as_deref())
        .await?;
    Ok(())
}

pub(crate) async fn repair_p2p(core: &ClientCore, settle_delay: Duration) -> Result<()> {
    core.request_p2p_repair().await?;
    tokio::time::sleep(settle_delay).await;
    Ok(())
}
