//! Signed `PeerEndpoint` heartbeat.
//!
//! `PeerRegistry` is self-asserted discovery state. The network-membership
//! substrate uses this signed endpoint row instead: the member DID signs its
//! current `(did, node_id, address, updated_at)` binding so peers can materialize
//! network-derived pairings from cryptographically bound reachability.

use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::network_token::EndpointRecord;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

pub async fn run_endpoint_heartbeat(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    let Some(p2p) = node.p2p_arc() else {
        tracing::debug!("PeerEndpoint heartbeat idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    };

    let mut interval = tokio::time::interval(super::intervals::endpoint_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    if let Err(error) = tick_endpoint(&node, &p2p, identity.as_ref()).await {
        tracing::warn!(
            did = %identity.did(),
            error = %error,
            "PeerEndpoint heartbeat: initial signed endpoint publish failed; will retry"
        );
    }

    interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                if let Err(error) = tick_endpoint(&node, &p2p, identity.as_ref()).await {
                    tracing::warn!(
                        did = %identity.did(),
                        error = %error,
                        "PeerEndpoint heartbeat: signed endpoint publish failed; will retry next interval"
                    );
                }
            }
        }
    }
}

async fn tick_endpoint(
    node: &EmbeddedNode,
    p2p: &Arc<dyn defra_p2p_adapter::P2POperations>,
    identity: &dyn AgentIdentity,
) -> Result<()> {
    let peer_id = p2p
        .local_peer_id()
        .await
        .map_err(|e| anyhow::anyhow!("local_peer_id: {e}"))?;
    let address = p2p
        .shareable_address()
        .await
        .map_err(|e| anyhow::anyhow!("shareable_address: {e}"))?
        .context("P2P node reported no shareable address for PeerEndpoint")?;

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut record = EndpointRecord {
        did: identity.did().to_string(),
        node_id: peer_id,
        address,
        updated_at: now,
        sig: Vec::new(),
    };
    record.sig = identity
        .sign(&record.signing_payload())
        .await
        .context("signing PeerEndpoint binding")?;
    let mutation = peer_endpoint_upsert_mutation(&record);
    crate::graphql::graphql_mutation_with_transaction_retry(
        node,
        &mutation,
        "upsert_peer_endpoint_heartbeat",
    )
    .await?;
    tracing::debug!(
        did = %record.did,
        node_id = %record.node_id,
        "PeerEndpoint heartbeat: signed endpoint written"
    );
    Ok(())
}

pub fn peer_endpoint_upsert_mutation(record: &EndpointRecord) -> String {
    let did = escape_graphql_string(&record.did);
    let node_id = escape_graphql_string(&record.node_id);
    let address = escape_graphql_string(&record.address);
    let updated_at = escape_graphql_string(&record.updated_at);
    let binding_sig = escape_graphql_string(&bs58::encode(&record.sig).into_string());
    format!(
        r#"mutation {{
            upsert_PeerEndpoint(
                filter: {{ did: {{ _eq: "{did}" }} }},
                add: {{
                    did: "{did}",
                    node_id: "{node_id}",
                    address: "{address}",
                    updated_at: "{updated_at}",
                    binding_sig: "{binding_sig}"
                }},
                update: {{
                    node_id: "{node_id}",
                    address: "{address}",
                    updated_at: "{updated_at}",
                    binding_sig: "{binding_sig}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_upsert_escapes_and_has_no_empty_lists() {
        let record = EndpointRecord {
            did: r#"did:key:z"member"#.into(),
            node_id: r#"peer"one"#.into(),
            address: r#"/ip4/127.0.0.1/tcp/1/p2p/peer"one"#.into(),
            updated_at: "2026-06-16T00:00:00Z".into(),
            sig: vec![1, 2, 3],
        };
        let mutation = peer_endpoint_upsert_mutation(&record);
        assert!(mutation.contains(r#"did: { _eq: "did:key:z\"member" }"#));
        assert!(mutation.contains(r#"node_id: "peer\"one""#));
        assert!(mutation.contains("binding_sig: \"Ldp\""));
        assert!(!mutation.contains("[]"));
    }
}
