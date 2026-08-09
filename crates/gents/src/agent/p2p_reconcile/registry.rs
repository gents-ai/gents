use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use defra_node::EmbeddedNode;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;

use super::graphql_helpers::{graphql_nullable_string_literal, graphql_string_list_literal};
use super::templates::resolve_template;

pub const REGISTRY_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

pub const DEFAULT_NETWORK_ID: &str = "default";

pub const NETWORK_ID_ENV: &str = "GENTS_NETWORK_ID";

pub fn resolve_network_id() -> String {
    std::env::var(NETWORK_ID_ENV)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
        .unwrap_or_else(|| DEFAULT_NETWORK_ID.to_string())
}

pub const DEFAULT_OFFERED_TEMPLATES: &[&str] = &["conversation", "agent-config"];

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    pub peer_id: String,
    pub agent_did: String,
    pub addresses: Vec<String>,
    pub templates: Vec<String>,
    pub display_name: Option<String>,
    pub status: String,
    pub network_id: String,
    pub invited_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertKind {
    Full,
    Heartbeat,
}

pub fn registry_upsert_mutation(entry: &RegistryEntry, now: &str, kind: UpsertKind) -> String {
    let peer_id = escape_graphql_string(&entry.peer_id);
    let agent_did = escape_graphql_string(&entry.agent_did);
    let addresses = graphql_string_list_literal(entry.addresses.iter().map(String::as_str));
    let templates = graphql_string_list_literal(entry.templates.iter().map(String::as_str));
    let display_name = graphql_nullable_string_literal(entry.display_name.as_deref());
    let status = escape_graphql_string(&entry.status);
    let network_id = escape_graphql_string(&entry.network_id);
    let invited_by = graphql_nullable_string_literal(entry.invited_by.as_deref());
    let now = escape_graphql_string(now);

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

    if let Err(error) = tick_registry(&node, &p2p, &agent_did, &network_id, "online").await {
        tracing::warn!(
            agent_did = %agent_did,
            network_id = %network_id,
            error = %error,
            "registry heartbeat: initial self-registration failed; will retry"
        );
    }

    interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
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
        templates: validate_offered_templates(DEFAULT_OFFERED_TEMPLATES.iter().copied()),
        display_name: None,
        status: status.to_string(),
        network_id: network_id.to_string(),
        invited_by: None,
    };

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = registry_upsert_mutation(&entry, &now, UpsertKind::Heartbeat);
    crate::graphql::graphql_with_transaction_retry(
        node,
        &mutation,
        "upsert_peer_registry_heartbeat",
    )
    .await?;

    tracing::debug!(
        peer_id = %peer_id,
        agent_did = %agent_did,
        status = %status,
        "registry heartbeat: self-registration written"
    );

    Ok(())
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
