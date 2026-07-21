//! Self-registration and heartbeat daemon for the `PeerRegistry` collection.
//!
//! Each running node writes (and periodically refreshes) its own row in
//! `PeerRegistry`, keyed by `peer_id`. This makes nodes discoverable to peers
//! that replicate the collection — the foundation of the service-discovery
//! layer described in `docs/superpowers/specs/2026-06-13-peer-registry-service-discovery-design.md` (removed from the tree; see git history).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use defra_node::EmbeddedNode;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;

use super::templates::resolve_template;

/// How often the node refreshes its `updated_at` heartbeat in `PeerRegistry`.
pub const REGISTRY_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Default discovery network id a node self-registers under. A single logical
/// network is the prototype default; multiple networks are not yet a first-class
/// configuration surface (see #490 review L4).
pub const DEFAULT_NETWORK_ID: &str = "default";

/// Environment variable overriding [`DEFAULT_NETWORK_ID`]. A seam so multiple
/// discovery networks can coexist without a code change until network id becomes
/// a first-class config field.
pub const NETWORK_ID_ENV: &str = "GENTS_NETWORK_ID";

/// Resolve the discovery network id from [`NETWORK_ID_ENV`], falling back to
/// [`DEFAULT_NETWORK_ID`].
pub fn resolve_network_id() -> String {
    std::env::var(NETWORK_ID_ENV)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
        .unwrap_or_else(|| DEFAULT_NETWORK_ID.to_string())
}

/// The scope templates a node offers by default when none are explicitly
/// configured: a node advertises that it is willing to replicate a peer's
/// conversation slice (filtered push) and the shared agent-config set. These
/// are the two everyday pairing intents; both resolve in the built-in catalog.
pub const DEFAULT_OFFERED_TEMPLATES: &[&str] = &["conversation", "agent-config"];

/// Filter a set of offered template ids down to those that resolve in the
/// built-in catalog, preserving order and de-duplicating. An unknown id is a
/// node advertising something a peer could not honor, so it is dropped rather
/// than advertised. Falls back to [`DEFAULT_OFFERED_TEMPLATES`] when the result
/// would otherwise be empty, so a node always offers at least the defaults.
pub fn validate_offered_templates<I, S>(offered: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for id in offered {
        let id = id.as_ref().trim();
        if id.is_empty() || resolve_template(id).is_none() {
            continue;
        }
        if seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    }
    if out.is_empty() {
        return DEFAULT_OFFERED_TEMPLATES
            .iter()
            .map(|id| id.to_string())
            .collect();
    }
    out
}

/// The fields this node self-reports into `PeerRegistry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    /// The libp2p peer ID of this node.
    pub peer_id: String,
    /// The agent DID (principal identity) running on this node.
    pub agent_did: String,
    /// Shareable multiaddrs (e.g. `/ip4/.../tcp/.../p2p/<peer_id>`).
    pub addresses: Vec<String>,
    /// Scope templates this node offers (null when empty). A peer materializes a
    /// scoped pairing from one of these (see the discovery reconciler).
    pub templates: Vec<String>,
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
///   all fields — `display_name`, `templates`, `addresses`, `status`, and
///   `updated_at` — because the operator explicitly supplied them.
/// - `Heartbeat` (daemon self-registration tick): the update writes ONLY
///   `status`, `updated_at`, `addresses` (network location can change), and
///   `templates` (the node's offered scope-template set), and deliberately omits
///   `display_name`. This preserves any operator-set `display_name` rather than
///   resetting it to null every 30 seconds, while still keeping the offered
///   templates fresh on every heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertKind {
    /// Full operator-supplied registration — update writes all fields.
    Full,
    /// Daemon heartbeat tick — update writes liveness fields plus the offered
    /// `templates`, preserving the operator-set `display_name`.
    Heartbeat,
}

/// Build a GraphQL upsert mutation for `PeerRegistry`.
///
/// - Filters on `peer_id`.
/// - The `add` branch (first registration) always sets every field from the
///   entry, including `display_name`, `templates`, and `registered_at`.
/// - The `update` branch behaviour depends on `kind`:
///   - [`UpsertKind::Full`]: updates all fields (operator register path).
///   - [`UpsertKind::Heartbeat`]: updates `status`, `updated_at`, `addresses`,
///     and `templates` (the offered scope-template set, which the heartbeat
///     re-advertises), leaving operator-set `display_name` intact.
/// - `templates`, `invited_by`, and `display_name` emit `null` when absent to
///   avoid the DefraDB empty-list / nil-column corruption (never `[]`).
pub fn registry_upsert_mutation(entry: &RegistryEntry, now: &str, kind: UpsertKind) -> String {
    let peer_id = escape_graphql_string(&entry.peer_id);
    let agent_did = escape_graphql_string(&entry.agent_did);
    let addresses = graphql_nullable_string_list_literal(&entry.addresses);
    let templates = graphql_nullable_string_list_literal(&entry.templates);
    let display_name = graphql_nullable_string_literal(entry.display_name.as_deref());
    let status = escape_graphql_string(&entry.status);
    let network_id = escape_graphql_string(&entry.network_id);
    let invited_by = graphql_nullable_string_literal(entry.invited_by.as_deref());
    let now = escape_graphql_string(now);

    // The update block differs by kind: Full rewrites every field; Heartbeat
    // writes the liveness fields (status, updated_at, addresses) plus the offered
    // templates, so a node re-advertises its scope offer on every tick while its
    // operator-set display_name is never clobbered by the 30s heartbeat.
    let update_block = match kind {
        UpsertKind::Full => format!(
            r#"update: {{
                    agent_did: "{agent_did}",
                    addresses: {addresses},
                    templates: {templates},
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
                    templates: {templates},
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
                    templates: {templates},
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

    let mut interval = tokio::time::interval(super::intervals::heartbeat_interval());
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

    // `tokio::time::interval` makes its first tick immediately ready. Consume
    // that scheduling tick after the explicit startup write so entering the
    // loop does not issue a redundant second upsert during startup contention.
    interval.tick().await;

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
        // A node advertises the scope templates it is willing to replicate. With
        // no operator override, that is the default offer (conversation +
        // agent-config), validated against the built-in catalog.
        templates: validate_offered_templates(DEFAULT_OFFERED_TEMPLATES.iter().copied()),
        display_name: None,
        status: status.to_string(),
        network_id: network_id.to_string(),
        invited_by: None,
    };

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // Use Heartbeat variant so recurring ticks never overwrite operator-set
    // display_name (the add branch on first registration sets it from the entry,
    // which is empty here — the operator path sets it). The heartbeat still
    // re-advertises the offered templates so the registry offer stays fresh.
    let mutation = registry_upsert_mutation(&entry, &now, UpsertKind::Heartbeat);
    let response = crate::retry::execute_graphql_with_conflict_retry(
        node,
        &mutation,
        "upsert_peer_registry_heartbeat",
    )
    .await;

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
    fn registry_upsert_mutation_escapes_and_emits_null_for_empty_templates() {
        let m = registry_upsert_mutation(
            &RegistryEntry {
                peer_id: r#"p"1"#.into(),
                agent_did: "did:key:a".into(),
                addresses: vec!["/ip4/1/tcp/1".into()],
                templates: vec![],
                display_name: Some("amy".into()),
                status: "online".into(),
                network_id: "default".into(),
                invited_by: None,
            },
            "2026-06-13T00:00:00Z",
            UpsertKind::Full,
        );
        assert!(m.contains(r#"peer_id: { _eq: "p\"1" }"#));
        assert!(m.contains("templates: null"));
        assert!(!m.contains("templates: []"));
        assert!(m.contains(r#"status: "online""#));
    }

    /// The heartbeat-variant `update` block must NOT contain `display_name` (it
    /// would clobber the operator-set value), but MUST re-advertise `templates`
    /// so the offered scope set stays fresh on every tick.
    #[test]
    fn heartbeat_upsert_update_block_omits_display_name_but_keeps_templates() {
        let entry = RegistryEntry {
            peer_id: "peer-hb".into(),
            agent_did: "did:key:hb".into(),
            addresses: vec!["/ip4/1/tcp/9/p2p/peer-hb".into()],
            templates: vec!["conversation".into()],
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
            m.contains(r#"templates: ["conversation"]"#),
            "add branch must still set templates on first registration: {m}"
        );

        // Split the mutation at `update: {` to isolate the update block.
        let update_portion = m
            .split_once("update: {")
            .expect("mutation must contain an update block")
            .1;
        assert!(
            !update_portion.contains("display_name"),
            "heartbeat update block must NOT contain display_name — it would clobber operator-set values: {update_portion}"
        );
        assert!(
            update_portion.contains("templates:"),
            "heartbeat update block must re-advertise templates: {update_portion}"
        );
    }

    /// The full/operator-variant `update` block MUST contain `display_name` and
    /// `templates` — the operator explicitly supplied them via `p2p network register`.
    #[test]
    fn operator_upsert_update_block_includes_display_name_and_templates() {
        let entry = RegistryEntry {
            peer_id: "peer-op".into(),
            agent_did: "did:key:op".into(),
            addresses: vec!["/ip4/1/tcp/9/p2p/peer-op".into()],
            templates: vec!["conversation".into()],
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
            update_portion.contains("templates:"),
            "operator update block must contain templates: {update_portion}"
        );
    }

    #[test]
    fn validate_offered_templates_keeps_known_drops_unknown_and_dedups() {
        let out = validate_offered_templates([
            "conversation",
            "nope",
            "agent-config",
            "conversation",
            "  ",
        ]);
        assert_eq!(
            out,
            vec!["conversation".to_string(), "agent-config".to_string()]
        );
    }

    #[test]
    fn validate_offered_templates_falls_back_to_defaults_when_empty() {
        let out = validate_offered_templates(Vec::<String>::new());
        assert_eq!(
            out,
            DEFAULT_OFFERED_TEMPLATES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        // All defaults must resolve in the catalog.
        for id in &out {
            assert!(resolve_template(id).is_some(), "default {id} must resolve");
        }
    }
}
