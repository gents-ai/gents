//! Network-membership materializer.
//!
//! This is the signed control-plane analogue of registry discovery. It derives
//! `source="network"` `PeerPairingDesired` rows from:
//! - one admin-signed `AgentNetwork`;
//! - active admin-signed `NetworkMembership` rows;
//! - fresh member-signed `PeerEndpoint` rows.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use defra_agent_protocol::network_token::{EndpointRecord, MembershipRecord, NetworkRecord};
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

use super::templates::{resolve_template, NETWORK_CONTROL_TEMPLATE};

pub const SOURCE_NETWORK: &str = "network";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkEndpointEntry {
    pub peer_id: String,
    pub agent_did: String,
    pub address: String,
}

pub fn derive_network_desired(
    self_did: &str,
    entries: &[NetworkEndpointEntry],
) -> BTreeSet<String> {
    let self_did = self_did.trim();
    entries
        .iter()
        .filter(|entry| entry.agent_did.trim() != self_did)
        .filter(|entry| !entry.peer_id.trim().is_empty())
        .map(|entry| entry.peer_id.clone())
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkTickOutcome {
    pub upserted: BTreeSet<String>,
    pub retracted: BTreeSet<String>,
}

#[async_trait]
pub trait NetworkStore: Send + Sync {
    async fn self_did(&self) -> Result<String>;
    async fn load_materializable_entries(&self) -> Result<Vec<NetworkEndpointEntry>>;
    async fn list_network_owned_peers(&self) -> Result<BTreeSet<String>>;
    async fn list_non_network_owned_peers(&self) -> Result<BTreeSet<String>>;
    async fn upsert_network_desired(&self, entry: &NetworkEndpointEntry) -> Result<()>;
    async fn delete_network_desired(&self, peer_id: &str) -> Result<()>;
}

pub async fn reconcile_network_tick(store: &dyn NetworkStore) -> Result<NetworkTickOutcome> {
    let self_did = store.self_did().await.context("read self DID")?;
    let entries = store
        .load_materializable_entries()
        .await
        .context("load network materialization inputs")?;
    let derived = derive_network_desired(&self_did, &entries);
    let entry_by_peer = entries
        .iter()
        .map(|entry| (entry.peer_id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let blocked = store
        .list_non_network_owned_peers()
        .await
        .context("list non-network desired peers")?;
    let desired = derived
        .difference(&blocked)
        .cloned()
        .collect::<BTreeSet<_>>();
    let existing = store
        .list_network_owned_peers()
        .await
        .context("list network-owned desired peers")?;

    let mut outcome = NetworkTickOutcome::default();
    for peer in desired.difference(&existing) {
        let entry = entry_by_peer
            .get(peer.as_str())
            .copied()
            .with_context(|| format!("derived network peer {peer} missing endpoint entry"))?;
        store
            .upsert_network_desired(entry)
            .await
            .with_context(|| format!("upsert network-owned desired row for {peer}"))?;
        outcome.upserted.insert(peer.clone());
    }
    for peer in existing.difference(&desired) {
        store
            .delete_network_desired(peer)
            .await
            .with_context(|| format!("delete network-owned desired row for {peer}"))?;
        outcome.retracted.insert(peer.clone());
    }
    Ok(outcome)
}

pub async fn run_network_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    if node.p2p_arc().is_none() {
        tracing::debug!("network reconciler idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    }

    let store = GraphqlNetworkStore::new(node.clone(), identity);
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_network(&store).await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => sweep_network(&store).await,
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("network reconciler update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "network reconciler update subscription dropped messages");
                }
                sweep_network(&store).await;
            }
        }
    }
}

async fn sweep_network(store: &dyn NetworkStore) {
    match reconcile_network_tick(store).await {
        Ok(outcome) => {
            if !outcome.upserted.is_empty() || !outcome.retracted.is_empty() {
                tracing::info!(
                    upserted = ?outcome.upserted,
                    retracted = ?outcome.retracted,
                    "network reconcile materialized signed membership pairings"
                );
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "network reconcile tick failed");
        }
    }
}

#[derive(Clone)]
pub struct GraphqlNetworkStore {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
}

impl GraphqlNetworkStore {
    pub fn new(node: Arc<EmbeddedNode>, identity: Arc<dyn AgentIdentity>) -> Self {
        Self { node, identity }
    }

    async fn list_peers_by_source(&self, source: &str) -> Result<BTreeSet<String>> {
        let source = escape_graphql_string(source);
        let query = format!(
            r#"{{
                PeerPairingDesired(filter: {{ source: {{ _eq: "{source}" }} }}) {{
                    peer_id
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query PeerPairingDesired by source")?;
        Ok(rows::<PeerSourceRow>(&response, "PeerPairingDesired")?
            .into_iter()
            .filter_map(|row| {
                let peer_id = row.peer_id.trim().to_string();
                (!peer_id.is_empty()).then_some(peer_id)
            })
            .collect())
    }
}

#[async_trait]
impl NetworkStore for GraphqlNetworkStore {
    async fn self_did(&self) -> Result<String> {
        Ok(self.identity.did().to_string())
    }

    async fn load_materializable_entries(&self) -> Result<Vec<NetworkEndpointEntry>> {
        let query = r#"{
            AgentNetwork {
                network_id
                admin_did
                display_name
                default_template
                created_at
                admin_sig
            }
            NetworkMembership {
                network_id
                member_did
                status
                granted_at
                revoked_at
                admin_sig
            }
            PeerEndpoint {
                did
                node_id
                address
                updated_at
                binding_sig
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query network materialization inputs")?;

        let network_rows = rows::<NetworkRow>(&response, "AgentNetwork")?;
        let network = match network_rows.as_slice() {
            [] => return Ok(Vec::new()),
            [row] => network_record(row)?,
            rows => bail!(
                "expected singleton AgentNetwork for network materialization, found {}",
                rows.len()
            ),
        };
        if !verify_record(
            self.identity.as_ref(),
            &network.admin_did,
            &network.signing_payload(),
            &network.sig,
            "AgentNetwork",
        )
        .await?
        {
            tracing::warn!(
                network_id = %network.network_id,
                admin_did = %network.admin_did,
                "network materializer ignoring AgentNetwork with invalid admin signature"
            );
            return Ok(Vec::new());
        }

        let endpoints = rows::<EndpointRow>(&response, "PeerEndpoint")?
            .into_iter()
            .filter_map(|row| match endpoint_record(&row) {
                Ok(record) => Some(record),
                Err(error) => {
                    tracing::warn!(error = %error, "network materializer skipped malformed PeerEndpoint");
                    None
                }
            })
            .map(|record| (record.did.clone(), record))
            .collect::<BTreeMap<_, _>>();

        let now = Utc::now();
        let mut out = Vec::new();
        for row in rows::<MembershipRow>(&response, "NetworkMembership")? {
            let membership = match membership_record(&row) {
                Ok(record) => record,
                Err(error) => {
                    tracing::warn!(error = %error, "network materializer skipped malformed NetworkMembership");
                    continue;
                }
            };
            if membership.network_id != network.network_id || membership.status.trim() != "active" {
                continue;
            }
            if !verify_record(
                self.identity.as_ref(),
                &network.admin_did,
                &membership.signing_payload(),
                &membership.sig,
                "NetworkMembership",
            )
            .await?
            {
                tracing::warn!(
                    member_did = %membership.member_did,
                    "network materializer skipped membership with invalid admin signature"
                );
                continue;
            }
            let Some(endpoint) = endpoints.get(&membership.member_did) else {
                continue;
            };
            if !endpoint_is_fresh(&endpoint.updated_at, now, super::intervals::stale_after()) {
                continue;
            }
            if !verify_record(
                self.identity.as_ref(),
                &endpoint.did,
                &endpoint.signing_payload(),
                &endpoint.sig,
                "PeerEndpoint",
            )
            .await?
            {
                tracing::warn!(
                    did = %endpoint.did,
                    "network materializer skipped endpoint with invalid member signature"
                );
                continue;
            }
            if endpoint.node_id.trim().is_empty() || endpoint.address.trim().is_empty() {
                continue;
            }
            out.push(NetworkEndpointEntry {
                peer_id: endpoint.node_id.clone(),
                agent_did: endpoint.did.clone(),
                address: endpoint.address.clone(),
            });
        }
        Ok(out)
    }

    async fn list_network_owned_peers(&self) -> Result<BTreeSet<String>> {
        self.list_peers_by_source(SOURCE_NETWORK).await
    }

    async fn list_non_network_owned_peers(&self) -> Result<BTreeSet<String>> {
        let query = r#"{
            PeerPairingDesired {
                peer_id
                source
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query PeerPairingDesired sources")?;
        Ok(rows::<PeerSourceRow>(&response, "PeerPairingDesired")?
            .into_iter()
            .filter(|row| row.source.as_deref().map(str::trim) != Some(SOURCE_NETWORK))
            .filter_map(|row| {
                let peer_id = row.peer_id.trim().to_string();
                (!peer_id.is_empty()).then_some(peer_id)
            })
            .collect())
    }

    async fn upsert_network_desired(&self, entry: &NetworkEndpointEntry) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mutation = upsert_network_desired_mutation(entry, &now)?;
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "upsert network-owned PeerPairingDesired")
    }

    async fn delete_network_desired(&self, peer_id: &str) -> Result<()> {
        let mutation = delete_network_desired_mutation(peer_id);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "delete network-owned PeerPairingDesired")
    }
}

pub fn endpoint_is_fresh(updated_at: &str, now: DateTime<Utc>, stale_after: Duration) -> bool {
    DateTime::parse_from_rfc3339(updated_at.trim())
        .ok()
        .map(|ts| super::discovery::heartbeat_is_fresh(ts.with_timezone(&Utc), now, stale_after))
        .unwrap_or(false)
}

pub fn upsert_network_desired_mutation(entry: &NetworkEndpointEntry, now: &str) -> Result<String> {
    let template = resolve_template(NETWORK_CONTROL_TEMPLATE)
        .context("network-control template is missing from built-in catalog")?;
    let collections = graphql_string_list_literal(template.collections.iter().copied());
    let addresses = graphql_string_list_literal([entry.address.as_str()]);
    let peer_id = escape_graphql_string(&entry.peer_id);
    let agent_did = escape_graphql_string(&entry.agent_did);
    let source = escape_graphql_string(SOURCE_NETWORK);
    let template = escape_graphql_string(template.id);
    let now = escape_graphql_string(now);
    Ok(format!(
        r#"mutation {{
            upsert_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "{source}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: "{agent_did}",
                    source: "{source}",
                    template: "{template}",
                    collections: {collections},
                    replicator_addresses: {addresses},
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{agent_did}",
                    source: "{source}",
                    template: "{template}",
                    collections: {collections},
                    replicator_addresses: {addresses},
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    ))
}

pub fn delete_network_desired_mutation(peer_id: &str) -> String {
    let peer_id = escape_graphql_string(peer_id);
    let source = escape_graphql_string(SOURCE_NETWORK);
    format!(
        r#"mutation {{
            delete_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "{source}" }} }}
            ) {{ _docID }}
        }}"#
    )
}

async fn verify_record(
    identity: &dyn AgentIdentity,
    signer_did: &str,
    payload: &[u8],
    signature: &[u8],
    label: &str,
) -> Result<bool> {
    identity
        .verify(signer_did, payload, signature)
        .await
        .with_context(|| format!("verifying {label} signature for {signer_did}"))
}

fn network_record(row: &NetworkRow) -> Result<NetworkRecord> {
    Ok(NetworkRecord {
        network_id: required(row.network_id.as_deref(), "AgentNetwork.network_id")?,
        admin_did: required(row.admin_did.as_deref(), "AgentNetwork.admin_did")?,
        display_name: required(row.display_name.as_deref(), "AgentNetwork.display_name")?,
        default_template: required(
            row.default_template.as_deref(),
            "AgentNetwork.default_template",
        )?,
        created_at: required(row.created_at.as_deref(), "AgentNetwork.created_at")?,
        sig: decode_sig(required(
            row.admin_sig.as_deref(),
            "AgentNetwork.admin_sig",
        )?)?,
    })
}

fn membership_record(row: &MembershipRow) -> Result<MembershipRecord> {
    Ok(MembershipRecord {
        network_id: required(row.network_id.as_deref(), "NetworkMembership.network_id")?,
        member_did: required(row.member_did.as_deref(), "NetworkMembership.member_did")?,
        status: required(row.status.as_deref(), "NetworkMembership.status")?,
        granted_at: required(row.granted_at.as_deref(), "NetworkMembership.granted_at")?,
        revoked_at: row.revoked_at.clone().unwrap_or_default(),
        sig: decode_sig(required(
            row.admin_sig.as_deref(),
            "NetworkMembership.admin_sig",
        )?)?,
    })
}

fn endpoint_record(row: &EndpointRow) -> Result<EndpointRecord> {
    Ok(EndpointRecord {
        did: required(row.did.as_deref(), "PeerEndpoint.did")?,
        node_id: required(row.node_id.as_deref(), "PeerEndpoint.node_id")?,
        address: required(row.address.as_deref(), "PeerEndpoint.address")?,
        updated_at: required(row.updated_at.as_deref(), "PeerEndpoint.updated_at")?,
        sig: decode_sig(required(
            row.binding_sig.as_deref(),
            "PeerEndpoint.binding_sig",
        )?)?,
    })
}

fn required(value: Option<&str>, field: &str) -> Result<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .with_context(|| format!("{field} is missing"))
}

fn decode_sig(sig: String) -> Result<Vec<u8>> {
    bs58::decode(sig)
        .into_vec()
        .context("decoding base58 signature")
}

fn graphql_string_list_literal<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let items = values
        .into_iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>();
    if items.is_empty() {
        "null".to_string()
    } else {
        format!("[{}]", items.join(", "))
    }
}

fn ensure_no_errors(response: &QueryResponse, label: &str) -> Result<()> {
    if response.has_errors() {
        bail!("{label} failed: {:?}", response.errors);
    }
    Ok(())
}

fn rows<T>(response: &QueryResponse, field: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(value) = response.data.as_ref().and_then(|data| data.get(field)) else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value.clone()).with_context(|| format!("decode {field} rows"))
}

#[derive(Deserialize)]
struct NetworkRow {
    network_id: Option<String>,
    admin_did: Option<String>,
    display_name: Option<String>,
    default_template: Option<String>,
    created_at: Option<String>,
    admin_sig: Option<String>,
}

#[derive(Deserialize)]
struct MembershipRow {
    network_id: Option<String>,
    member_did: Option<String>,
    status: Option<String>,
    granted_at: Option<String>,
    revoked_at: Option<String>,
    admin_sig: Option<String>,
}

#[derive(Deserialize)]
struct EndpointRow {
    did: Option<String>,
    node_id: Option<String>,
    address: Option<String>,
    updated_at: Option<String>,
    binding_sig: Option<String>,
}

#[derive(Deserialize)]
struct PeerSourceRow {
    peer_id: String,
    #[serde(default)]
    source: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_network_desired_skips_self() {
        let entries = vec![
            NetworkEndpointEntry {
                peer_id: "peer-self".into(),
                agent_did: "did:key:self".into(),
                address: "/ip4/1/tcp/1/p2p/peer-self".into(),
            },
            NetworkEndpointEntry {
                peer_id: "peer-a".into(),
                agent_did: "did:key:a".into(),
                address: "/ip4/1/tcp/2/p2p/peer-a".into(),
            },
        ];
        let desired = derive_network_desired("did:key:self", &entries);
        assert!(!desired.contains("peer-self"));
        assert!(desired.contains("peer-a"));
    }

    #[test]
    fn endpoint_freshness_matches_registry_window() {
        let now = DateTime::parse_from_rfc3339("2026-06-16T00:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(endpoint_is_fresh(
            "2026-06-16T00:00:30Z",
            now,
            Duration::from_secs(90)
        ));
        assert!(!endpoint_is_fresh(
            "2026-06-15T23:00:00Z",
            now,
            Duration::from_secs(90)
        ));
    }

    #[test]
    fn network_desired_upsert_uses_network_source_and_control_template() {
        let entry = NetworkEndpointEntry {
            peer_id: r#"peer"a"#.into(),
            agent_did: r#"did:key:z"a"#.into(),
            address: r#"/ip4/127.0.0.1/tcp/1/p2p/peer"a"#.into(),
        };
        let mutation = upsert_network_desired_mutation(&entry, "2026-06-16T00:00:00Z").unwrap();
        assert!(mutation.contains(r#"peer_id: { _eq: "peer\"a" }"#));
        assert!(mutation.contains(r#"source: "network""#));
        assert!(mutation.contains(r#"template: "network-control""#));
        assert!(mutation.contains("AgentNetwork"));
        assert!(!mutation.contains("AgentBehavior"));
        assert!(!mutation.contains("[]"));
    }
}
