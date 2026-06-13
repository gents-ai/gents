//! Self-registration and heartbeat daemon for the `PeerRegistry` collection.
//!
//! Each running node writes (and periodically refreshes) its own row in
//! `PeerRegistry`, keyed by `peer_id`. This makes nodes discoverable to peers
//! that replicate the collection — the foundation of the service-discovery
//! layer described in `docs/superpowers/specs/2026-06-13-peer-registry-service-discovery-design.md`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use defra_node::EmbeddedNode;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;

/// How often the node refreshes its `updated_at` heartbeat in `PeerRegistry`.
pub const REGISTRY_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// The fields this node self-reports into `PeerRegistry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    /// The libp2p peer ID of this node.
    pub peer_id: String,
    /// The agent DID (principal identity) running on this node.
    pub agent_did: String,
    /// Shareable multiaddrs (e.g. `/ip4/.../tcp/.../p2p/<peer_id>`).
    pub addresses: Vec<String>,
    /// Collection profiles this node offers (null when empty).
    pub profiles: Vec<String>,
    /// Optional human-readable name for this node.
    pub display_name: Option<String>,
    /// Liveness hint: `"online"` or `"offline"`.
    pub status: String,
    /// Which network this node belongs to.
    pub network_id: String,
    /// DID of the member that issued the signed invite admitting this node.
    pub invited_by: Option<String>,
}

/// Build a GraphQL upsert mutation for `PeerRegistry`.
///
/// - Filters on `peer_id`.
/// - `registered_at` is set only on the `add` branch (first registration).
/// - `updated_at` is refreshed on both `add` and `update` (heartbeat).
/// - `profiles`, `invited_by`, and `display_name` emit `null` when absent to
///   avoid the DefraDB empty-list / nil-column corruption (never `[]`).
pub fn registry_upsert_mutation(entry: &RegistryEntry, now: &str) -> String {
    let peer_id = escape_graphql_string(&entry.peer_id);
    let agent_did = escape_graphql_string(&entry.agent_did);
    let addresses = graphql_nullable_string_list_literal(&entry.addresses);
    let profiles = graphql_nullable_string_list_literal(&entry.profiles);
    let display_name = graphql_nullable_string_literal(entry.display_name.as_deref());
    let status = escape_graphql_string(&entry.status);
    let network_id = escape_graphql_string(&entry.network_id);
    let invited_by = graphql_nullable_string_literal(entry.invited_by.as_deref());
    let now = escape_graphql_string(now);

    format!(
        r#"mutation {{
            upsert_PeerRegistry(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: "{agent_did}",
                    addresses: {addresses},
                    profiles: {profiles},
                    display_name: {display_name},
                    status: "{status}",
                    network_id: "{network_id}",
                    invited_by: {invited_by},
                    registered_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{agent_did}",
                    addresses: {addresses},
                    profiles: {profiles},
                    display_name: {display_name},
                    status: "{status}",
                    network_id: "{network_id}",
                    invited_by: {invited_by},
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

/// Background daemon: self-register this node into `PeerRegistry` at startup
/// and refresh the heartbeat every [`REGISTRY_HEARTBEAT_INTERVAL`].
///
/// Mirrors the structure of `run_pairing_reconciler` — if the embedded node
/// has no P2P transport the daemon exits immediately (idle). On cancel it
/// optionally writes an `"offline"` row before returning.
pub async fn run_registry_heartbeat(
    node: Arc<EmbeddedNode>,
    agent_did: String,
    network_id: String,
    cancel: CancellationToken,
) -> Result<()> {
    let Some(p2p) = node.p2p_arc() else {
        tracing::debug!("registry heartbeat idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    };

    let mut interval = tokio::time::interval(REGISTRY_HEARTBEAT_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Perform the initial registration immediately, then heartbeat.
    if let Err(error) =
        tick_registry(&node, &p2p, &agent_did, &network_id, "online").await
    {
        tracing::warn!(
            agent_did = %agent_did,
            network_id = %network_id,
            error = %error,
            "registry heartbeat: initial self-registration failed; will retry"
        );
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                // Best-effort offline write; never block shutdown on a write failure.
                if let Err(error) =
                    tick_registry(&node, &p2p, &agent_did, &network_id, "offline").await
                {
                    tracing::warn!(
                        agent_did = %agent_did,
                        error = %error,
                        "registry heartbeat: offline status write failed during shutdown"
                    );
                }
                return Ok(());
            }
            _ = interval.tick() => {
                if let Err(error) =
                    tick_registry(&node, &p2p, &agent_did, &network_id, "online").await
                {
                    tracing::warn!(
                        agent_did = %agent_did,
                        error = %error,
                        "registry heartbeat: heartbeat tick failed; will retry next interval"
                    );
                }
            }
        }
    }
}

async fn tick_registry(
    node: &EmbeddedNode,
    p2p: &Arc<dyn defra_p2p_adapter::P2POperations>,
    agent_did: &str,
    network_id: &str,
    status: &str,
) -> Result<()> {
    let peer_id = p2p
        .local_peer_id()
        .await
        .map_err(|e| anyhow::anyhow!("local_peer_id: {e}"))?;

    let raw_addresses = p2p
        .listen_addresses()
        .await
        .map_err(|e| anyhow::anyhow!("listen_addresses: {e}"))?;

    let addresses: Vec<String> = raw_addresses
        .into_iter()
        .map(|addr| {
            if addr.starts_with('/') {
                format!("{addr}/p2p/{peer_id}")
            } else {
                addr
            }
        })
        .collect();

    let entry = RegistryEntry {
        peer_id: peer_id.clone(),
        agent_did: agent_did.to_string(),
        addresses,
        profiles: Vec::new(),
        display_name: None,
        status: status.to_string(),
        network_id: network_id.to_string(),
        invited_by: None,
    };

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = registry_upsert_mutation(&entry, &now);
    let response = node.execute(&mutation).await;

    if response.has_errors() {
        bail!(
            "upsert_PeerRegistry failed: {:?}",
            response.errors
        );
    }

    tracing::debug!(
        peer_id = %peer_id,
        agent_did = %agent_did,
        status = %status,
        "registry heartbeat: self-registration written"
    );

    Ok(())
}

fn graphql_nullable_string_list_literal(values: &[String]) -> String {
    if values.is_empty() {
        return "null".to_string();
    }
    format!(
        "[{}]",
        values
            .iter()
            .map(|v| format!(r#""{}""#, escape_graphql_string(v)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn graphql_nullable_string_literal(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| format!(r#""{}""#, escape_graphql_string(v)))
        .unwrap_or_else(|| "null".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_upsert_mutation_escapes_and_emits_null_for_empty_profiles() {
        let m = registry_upsert_mutation(
            &RegistryEntry {
                peer_id: r#"p"1"#.into(),
                agent_did: "did:key:a".into(),
                addresses: vec!["/ip4/1/tcp/1".into()],
                profiles: vec![],
                display_name: Some("amy".into()),
                status: "online".into(),
                network_id: "default".into(),
                invited_by: None,
            },
            "2026-06-13T00:00:00Z",
        );
        assert!(m.contains(r#"peer_id: { _eq: "p\"1" }"#));
        assert!(m.contains("profiles: null"));
        assert!(!m.contains("profiles: []"));
        assert!(m.contains(r#"status: "online""#));
    }
}
