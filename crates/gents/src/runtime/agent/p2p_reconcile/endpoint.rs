use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::network_token::EndpointRecord;
use tokio_util::sync::CancellationToken;

use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

#[derive(Clone, Debug, PartialEq, Eq)]
struct EndpointBinding {
    peer_id: String,
    address: String,
}

pub async fn run_endpoint_heartbeat(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    endpoint_heartbeat(node, identity, cancel).await
}

async fn endpoint_heartbeat(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    let Some(p2p) = crate::store::identity::node_p2p(&node) else {
        tracing::debug!("PeerEndpoint heartbeat idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    };

    let mut interval = tokio::time::interval(super::intervals::endpoint_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut published = None;

    if let Err(error) = tick_endpoint(&node, &p2p, identity.as_ref(), &mut published).await {
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
                if let Err(error) = tick_endpoint(
                    &node,
                    &p2p,
                    identity.as_ref(),
                    &mut published,
                ).await {
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
    node: &std::sync::Arc<EmbeddedNode>,
    p2p: &Arc<dyn defra_p2p_adapter::P2POperations>,
    identity: &dyn NodeIdentity,
    published: &mut Option<EndpointBinding>,
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
    let binding = EndpointBinding { peer_id, address };
    // Readers take freshness from the authorization lease, not from this
    // row's `updated_at`, so an unchanged binding is never rewritten.
    if published.as_ref() == Some(&binding) {
        return Ok(());
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut record = EndpointRecord {
        did: identity.did().to_string(),
        node_id: binding.peer_id.clone(),
        address: binding.address.clone(),
        updated_at: now,
        sig: Vec::new(),
    };
    record.sig = identity
        .sign(&record.signing_payload())
        .await
        .context("signing PeerEndpoint binding")?;
    let mutation = peer_endpoint_upsert_mutation(&record);
    crate::store::config_client::ConfigAccess::write_local_response(
        node,
        "p2p.upsert_endpoint_heartbeat",
        &mutation,
    )
    .await?;
    *published = Some(binding);
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
