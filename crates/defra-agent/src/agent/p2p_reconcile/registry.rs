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

/// Controls which fields the `update` branch of [`registry_upsert_mutation`]
/// writes.
///
/// - `Full` (operator register, `p2p network register`): the update includes
///   all fields — `display_name`, `profiles`, `addresses`, `status`, and
///   `updated_at` — because the operator explicitly supplied them.
/// - `Heartbeat` (daemon self-registration tick): the update writes ONLY
///   `status`, `updated_at`, and `addresses` (network location can change),
///   and deliberately omits `display_name` and `profiles`. This preserves any
///   value an operator previously set via `p2p network register` rather than
///   resetting it to null every 30 seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertKind {
    /// Full operator-supplied registration — update writes all fields.
    Full,
    /// Daemon heartbeat tick — update writes only liveness fields, preserving
    /// operator-set `display_name` and `profiles`.
    Heartbeat,
}

/// Build a GraphQL upsert mutation for `PeerRegistry`.
///
/// - Filters on `peer_id`.
/// - The `add` branch (first registration) always sets every field from the
///   entry, including `display_name`, `profiles`, and `registered_at`.
/// - The `update` branch behaviour depends on `kind`:
///   - [`UpsertKind::Full`]: updates all fields (operator register path).
///   - [`UpsertKind::Heartbeat`]: updates only `status`, `updated_at`, and
///     `addresses`, leaving operator-set `display_name`/`profiles` intact.
/// - `profiles`, `invited_by`, and `display_name` emit `null` when absent to
///   avoid the DefraDB empty-list / nil-column corruption (never `[]`).
pub fn registry_upsert_mutation(entry: &RegistryEntry, now: &str, kind: UpsertKind) -> String {
    let peer_id = escape_graphql_string(&entry.peer_id);
    let agent_did = escape_graphql_string(&entry.agent_did);
    let addresses = graphql_nullable_string_list_literal(&entry.addresses);
    let profiles = graphql_nullable_string_list_literal(&entry.profiles);
    let display_name = graphql_nullable_string_literal(entry.display_name.as_deref());
    let status = escape_graphql_string(&entry.status);
    let network_id = escape_graphql_string(&entry.network_id);
    let invited_by = graphql_nullable_string_literal(entry.invited_by.as_deref());
    let now = escape_graphql_string(now);

    // The update block differs by kind: Full rewrites every field; Heartbeat
    // writes only the liveness fields (status, updated_at, addresses) so that
    // operator-set display_name and profiles are never clobbered by the 30s tick.
    let update_block = match kind {
        UpsertKind::Full => format!(
            r#"update: {{
                    agent_did: "{agent_did}",
                    addresses: {addresses},
                    profiles: {profiles},
                    display_name: {display_name},
                    status: "{status}",
                    network_id: "{network_id}",
                    invited_by: {invited_by},
                    updated_at: "{now}"
                }}"#
        ),
        UpsertKind::Heartbeat => format!(
            r#"update: {{
                    addresses: {addresses},
                    status: "{status}",
                    updated_at: "{now}"
                }}"#
        ),
    };

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
                {update_block}
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
    if let Err(error) = tick_registry(&node, &p2p, &agent_did, &network_id, "online").await {
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
    // Use Heartbeat variant so recurring ticks never overwrite operator-set
    // display_name or profiles (the add branch on first registration still sets
    // them from the entry, which is empty here — the operator path sets them).
    let mutation = registry_upsert_mutation(&entry, &now, UpsertKind::Heartbeat);
    let response = node.execute(&mutation).await;

    if response.has_errors() {
        bail!("upsert_PeerRegistry failed: {:?}", response.errors);
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
            UpsertKind::Full,
        );
        assert!(m.contains(r#"peer_id: { _eq: "p\"1" }"#));
        assert!(m.contains("profiles: null"));
        assert!(!m.contains("profiles: []"));
        assert!(m.contains(r#"status: "online""#));
    }

    /// The heartbeat-variant `update` block must NOT contain `display_name` or
    /// `profiles` — those fields must be absent so the operator-set values are
    /// never overwritten by the 30-second heartbeat tick.
    #[test]
    fn heartbeat_upsert_update_block_omits_display_name_and_profiles() {
        let entry = RegistryEntry {
            peer_id: "peer-hb".into(),
            agent_did: "did:key:hb".into(),
            addresses: vec!["/ip4/1/tcp/9/p2p/peer-hb".into()],
            profiles: vec!["chat-requests".into()],
            display_name: Some("should-not-appear-in-update".into()),
            status: "online".into(),
            network_id: "default".into(),
            invited_by: None,
        };
        let m = registry_upsert_mutation(&entry, "2026-06-13T01:00:00Z", UpsertKind::Heartbeat);

        // The `add` branch (first-registration) IS allowed to set everything.
        assert!(
            m.contains(r#"display_name: "should-not-appear-in-update""#),
            "add branch must still set display_name on first registration: {m}"
        );
        assert!(
            m.contains(r#"profiles: ["chat-requests"]"#),
            "add branch must still set profiles on first registration: {m}"
        );

        // Split the mutation at `update: {` to isolate the update block, then
        // confirm display_name and profiles do NOT appear in the update portion.
        let update_portion = m
            .split_once("update: {")
            .expect("mutation must contain an update block")
            .1;
        assert!(
            !update_portion.contains("display_name"),
            "heartbeat update block must NOT contain display_name — it would clobber operator-set values: {update_portion}"
        );
        assert!(
            !update_portion.contains("profiles"),
            "heartbeat update block must NOT contain profiles — it would clobber operator-set values: {update_portion}"
        );
    }

    /// The full/operator-variant `update` block MUST contain `display_name` and
    /// `profiles` — the operator explicitly supplied them via `p2p network register`.
    #[test]
    fn operator_upsert_update_block_includes_display_name_and_profiles() {
        let entry = RegistryEntry {
            peer_id: "peer-op".into(),
            agent_did: "did:key:op".into(),
            addresses: vec!["/ip4/1/tcp/9/p2p/peer-op".into()],
            profiles: vec!["chat-requests".into()],
            display_name: Some("my-node".into()),
            status: "online".into(),
            network_id: "default".into(),
            invited_by: None,
        };
        let m = registry_upsert_mutation(&entry, "2026-06-13T01:00:00Z", UpsertKind::Full);

        let update_portion = m
            .split_once("update: {")
            .expect("mutation must contain an update block")
            .1;
        assert!(
            update_portion.contains(r#"display_name: "my-node""#),
            "operator update block must contain display_name: {update_portion}"
        );
        assert!(
            update_portion.contains("profiles:"),
            "operator update block must contain profiles: {update_portion}"
        );
    }
}
