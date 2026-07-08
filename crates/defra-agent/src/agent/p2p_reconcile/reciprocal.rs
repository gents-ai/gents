use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_agent_protocol::network_token::EndpointRecord;
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

use super::network::{NetworkEndpointEntry, endpoint_is_fresh};
use super::templates::resolve_template;

pub const RECIPROCAL_CONVERSATION_TEMPLATE: &str = "conversation";

/// Select endpoints that can materialize a reciprocal conversation data-plane
/// edge for a previously invited member DID.
///
/// This is intentionally pure: the store layer verifies `PeerEndpoint` records
/// and supplies only signed endpoint entries; the derivation only joins those
/// entries with `ReciprocalConversationIntent.member_did` and defers entries
/// without a dialable peer id/address.
pub fn derive_reciprocal_desired<'a>(
    intent_dids: &BTreeSet<String>,
    endpoints: &'a [NetworkEndpointEntry],
) -> Vec<&'a NetworkEndpointEntry> {
    endpoints
        .iter()
        .filter(|entry| intent_dids.contains(&entry.agent_did))
        .filter(|entry| !entry.peer_id.trim().is_empty())
        .filter(|entry| !entry.address.trim().is_empty())
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReciprocalTickOutcome {
    pub upserted: BTreeSet<String>,
    pub retracted: BTreeSet<String>,
}

#[async_trait]
pub trait ReciprocalStore: Send + Sync {
    async fn load_intent_dids(&self) -> Result<BTreeSet<String>>;
    async fn load_endpoint_for_did(&self, did: &str) -> Result<Option<NetworkEndpointEntry>>;
    async fn upsert_reciprocal_data_plane(
        &self,
        peer_id: &str,
        agent_did: &str,
        address: &str,
    ) -> Result<()>;
    async fn delete_reciprocal_data_plane(&self, peer_id: &str) -> Result<()>;
    async fn list_reciprocal_data_plane_peers(&self) -> Result<BTreeSet<String>>;
}

pub async fn reconcile_reciprocal_tick(
    store: &dyn ReciprocalStore,
    self_did: &str,
) -> Result<ReciprocalTickOutcome> {
    let intent_dids = store
        .load_intent_dids()
        .await
        .context("load reciprocal conversation intents")?;
    let mut endpoints = Vec::new();
    for did in &intent_dids {
        if let Some(endpoint) = store
            .load_endpoint_for_did(did)
            .await
            .with_context(|| format!("load verified PeerEndpoint for reciprocal DID {did}"))?
        {
            endpoints.push(endpoint);
        }
    }

    let derived = derive_reciprocal_desired(&intent_dids, &endpoints);
    let desired = derived
        .iter()
        .map(|entry| entry.peer_id.clone())
        .collect::<BTreeSet<_>>();
    let entry_by_peer = derived
        .iter()
        .map(|entry| (entry.peer_id.as_str(), *entry))
        .collect::<BTreeMap<_, _>>();
    let existing = store
        .list_reciprocal_data_plane_peers()
        .await
        .context("list reciprocal data-plane desired peers")?;

    let mut outcome = ReciprocalTickOutcome::default();
    for peer in desired.difference(&existing) {
        let entry = entry_by_peer
            .get(peer.as_str())
            .copied()
            .with_context(|| format!("derived reciprocal peer {peer} missing endpoint entry"))?;
        store
            .upsert_reciprocal_data_plane(&entry.peer_id, self_did, &entry.address)
            .await
            .with_context(|| format!("upsert reciprocal data-plane desired row for {peer}"))?;
        outcome.upserted.insert(peer.clone());
    }
    for peer in existing.difference(&desired) {
        store
            .delete_reciprocal_data_plane(peer)
            .await
            .with_context(|| format!("delete reciprocal data-plane desired row for {peer}"))?;
        outcome.retracted.insert(peer.clone());
    }
    Ok(outcome)
}

pub async fn run_reciprocal_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    if node.p2p_arc().is_none() {
        tracing::debug!("reciprocal reconciler idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    }

    let store = GraphqlReciprocalStore::new(node.clone(), identity.clone());
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_reciprocal(&store, identity.did()).await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => sweep_reciprocal(&store, identity.did()).await,
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("reciprocal reconciler update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "reciprocal reconciler update subscription dropped messages");
                }
                sweep_reciprocal(&store, identity.did()).await;
            }
        }
    }
}

async fn sweep_reciprocal(store: &GraphqlReciprocalStore, self_did: &str) {
    match reconcile_reciprocal_tick(store, self_did).await {
        Ok(outcome) => {
            if !outcome.upserted.is_empty() || !outcome.retracted.is_empty() {
                tracing::info!(
                    upserted = ?outcome.upserted,
                    retracted = ?outcome.retracted,
                    "reconciled reciprocal conversation data-plane desired rows"
                );
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "reciprocal conversation reconcile sweep failed")
        }
    }
}

pub struct GraphqlReciprocalStore {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
}

impl GraphqlReciprocalStore {
    pub fn new(node: Arc<EmbeddedNode>, identity: Arc<dyn AgentIdentity>) -> Self {
        Self { node, identity }
    }
}

#[async_trait]
impl ReciprocalStore for GraphqlReciprocalStore {
    async fn load_intent_dids(&self) -> Result<BTreeSet<String>> {
        let query = r#"{
            ReciprocalConversationIntent {
                member_did
                template
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query ReciprocalConversationIntent")?;
        Ok(
            rows::<IntentRow>(&response, "ReciprocalConversationIntent")?
                .into_iter()
                .filter(|row| {
                    row.template.as_deref().map(str::trim) == Some(RECIPROCAL_CONVERSATION_TEMPLATE)
                })
                .filter_map(|row| row.member_did.map(|did| did.trim().to_string()))
                .filter(|did| !did.is_empty())
                .collect(),
        )
    }

    async fn load_endpoint_for_did(&self, did: &str) -> Result<Option<NetworkEndpointEntry>> {
        let did = escape_graphql_string(did.trim());
        let query = format!(
            r#"{{
                PeerEndpoint(filter: {{ did: {{ _eq: "{did}" }} }}) {{
                    did
                    node_id
                    address
                    updated_at
                    binding_sig
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query PeerEndpoint for reciprocal DID")?;
        for row in rows::<EndpointRow>(&response, "PeerEndpoint")? {
            let record = match endpoint_record(&row) {
                Ok(record) => record,
                Err(error) => {
                    tracing::warn!(error = %error, "reciprocal reconciler skipped malformed PeerEndpoint");
                    continue;
                }
            };
            if !endpoint_is_fresh(
                &record.updated_at,
                Utc::now(),
                super::intervals::stale_after(),
            ) {
                continue;
            }
            if !verify_endpoint(self.identity.as_ref(), &record).await? {
                tracing::warn!(did = %record.did, "reciprocal reconciler skipped endpoint with invalid member signature");
                continue;
            }
            if record.node_id.trim().is_empty() || record.address.trim().is_empty() {
                continue;
            }
            return Ok(Some(NetworkEndpointEntry {
                peer_id: record.node_id,
                agent_did: record.did,
                address: record.address,
            }));
        }
        Ok(None)
    }

    async fn upsert_reciprocal_data_plane(
        &self,
        peer_id: &str,
        agent_did: &str,
        address: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let mutation = upsert_reciprocal_data_plane_mutation(peer_id, agent_did, address, &now)?;
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "upsert reciprocal DataPlanePairingDesired")
    }

    async fn delete_reciprocal_data_plane(&self, peer_id: &str) -> Result<()> {
        let mutation = delete_reciprocal_data_plane_mutation(peer_id);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "delete reciprocal DataPlanePairingDesired")
    }

    async fn list_reciprocal_data_plane_peers(&self) -> Result<BTreeSet<String>> {
        let query = r#"{
            DataPlanePairingDesired {
                peer_id
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query reciprocal DataPlanePairingDesired peers")?;
        Ok(rows::<PeerIdRow>(&response, "DataPlanePairingDesired")?
            .into_iter()
            .filter_map(|row| {
                let peer_id = row.peer_id.trim().to_string();
                (!peer_id.is_empty()).then_some(peer_id)
            })
            .collect())
    }
}

pub fn upsert_reciprocal_data_plane_mutation(
    peer_id: &str,
    agent_did: &str,
    address: &str,
    now: &str,
) -> Result<String> {
    let template = resolve_template(RECIPROCAL_CONVERSATION_TEMPLATE)
        .context("conversation template is missing from built-in catalog")?;
    let collections = graphql_string_list_literal(template.collections.iter().copied());
    let addresses = graphql_string_list_literal([address]);
    let peer_id = escape_graphql_string(peer_id);
    let agent_did = escape_graphql_string(agent_did);
    let template = escape_graphql_string(template.id);
    let now = escape_graphql_string(now);
    Ok(format!(
        r#"mutation {{
            upsert_DataPlanePairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: "{agent_did}",
                    template: "{template}",
                    collections: {collections},
                    replicator_addresses: {addresses},
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{agent_did}",
                    template: "{template}",
                    collections: {collections},
                    replicator_addresses: {addresses},
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    ))
}

pub fn delete_reciprocal_data_plane_mutation(peer_id: &str) -> String {
    let peer_id = escape_graphql_string(peer_id);
    format!(
        r#"mutation {{
            delete_DataPlanePairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{ _docID }}
        }}"#
    )
}

async fn verify_endpoint(identity: &dyn AgentIdentity, record: &EndpointRecord) -> Result<bool> {
    match identity
        .verify(&record.did, &record.signing_payload(), &record.sig)
        .await
    {
        Ok(true) => Ok(true),
        Ok(false) => Ok(false),
        Err(error) => {
            tracing::warn!(error = %error, did = %record.did, "PeerEndpoint signature verification errored");
            Ok(false)
        }
    }
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
struct IntentRow {
    member_did: Option<String>,
    template: Option<String>,
}

#[derive(Deserialize)]
struct EndpointRow {
    did: Option<String>,
    node_id: Option<String>,
    address: Option<String>,
    updated_at: Option<String>,
    #[serde(default, alias = "binding_sig")]
    binding_sig: Option<String>,
}

#[derive(Deserialize)]
struct PeerIdRow {
    peer_id: String,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn endpoint(did: &str, peer_id: &str, address: &str) -> NetworkEndpointEntry {
        NetworkEndpointEntry {
            peer_id: peer_id.to_string(),
            agent_did: did.to_string(),
            address: address.to_string(),
        }
    }

    #[test]
    fn derive_reciprocal_desired_selects_endpoint_for_intent_did() {
        let intents = BTreeSet::from(["did:key:phone".to_string()]);
        let endpoints = vec![endpoint("did:key:phone", "peer-phone", "/ticket/phone")];

        let desired = derive_reciprocal_desired(&intents, &endpoints);

        assert_eq!(desired.len(), 1);
        assert_eq!(desired[0].peer_id, "peer-phone");
        assert_eq!(desired[0].agent_did, "did:key:phone");
        assert_eq!(desired[0].address, "/ticket/phone");
    }

    #[test]
    fn derive_reciprocal_desired_defers_without_matching_endpoint() {
        let intents = BTreeSet::from(["did:key:phone".to_string()]);
        let endpoints = vec![endpoint("did:key:other", "peer-other", "/ticket/other")];

        let desired = derive_reciprocal_desired(&intents, &endpoints);

        assert!(desired.is_empty());
    }

    #[test]
    fn derive_reciprocal_desired_ignores_blank_peer_id_or_address() {
        let intents = BTreeSet::from(["did:key:phone".to_string()]);
        let endpoints = vec![
            endpoint("did:key:phone", "", "/ticket/phone"),
            endpoint("did:key:phone", "peer-phone", ""),
            endpoint("did:key:phone", "   ", "/ticket/phone"),
            endpoint("did:key:phone", "peer-phone", "   "),
        ];

        let desired = derive_reciprocal_desired(&intents, &endpoints);

        assert!(desired.is_empty());
    }

    #[derive(Default)]
    struct MockReciprocalStore {
        intents: BTreeSet<String>,
        endpoints: BTreeMap<String, NetworkEndpointEntry>,
        existing: BTreeSet<String>,
        upserts: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ReciprocalStore for MockReciprocalStore {
        async fn load_intent_dids(&self) -> Result<BTreeSet<String>> {
            Ok(self.intents.clone())
        }

        async fn load_endpoint_for_did(&self, did: &str) -> Result<Option<NetworkEndpointEntry>> {
            Ok(self.endpoints.get(did).cloned())
        }

        async fn upsert_reciprocal_data_plane(
            &self,
            peer_id: &str,
            agent_did: &str,
            address: &str,
        ) -> Result<()> {
            self.upserts.lock().unwrap().push((
                peer_id.to_string(),
                agent_did.to_string(),
                address.to_string(),
            ));
            Ok(())
        }

        async fn delete_reciprocal_data_plane(&self, peer_id: &str) -> Result<()> {
            self.deletes.lock().unwrap().push(peer_id.to_string());
            Ok(())
        }

        async fn list_reciprocal_data_plane_peers(&self) -> Result<BTreeSet<String>> {
            Ok(self.existing.clone())
        }
    }

    #[tokio::test]
    async fn reconcile_reciprocal_tick_upserts_conversation_data_plane_for_verified_endpoint() {
        let store = MockReciprocalStore {
            intents: BTreeSet::from(["did:key:phone".to_string()]),
            endpoints: BTreeMap::from([(
                "did:key:phone".to_string(),
                endpoint("did:key:phone", "peer-phone", "/ticket/phone"),
            )]),
            ..Default::default()
        };

        let outcome = reconcile_reciprocal_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert_eq!(outcome.upserted, BTreeSet::from(["peer-phone".to_string()]));
        assert!(outcome.retracted.is_empty());
        assert_eq!(
            *store.upserts.lock().unwrap(),
            vec![(
                "peer-phone".to_string(),
                "did:key:server".to_string(),
                "/ticket/phone".to_string()
            )]
        );
        assert!(store.deletes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reconcile_reciprocal_tick_deletes_when_endpoint_disappears() {
        let store = MockReciprocalStore {
            intents: BTreeSet::from(["did:key:phone".to_string()]),
            existing: BTreeSet::from(["peer-phone".to_string()]),
            ..Default::default()
        };

        let outcome = reconcile_reciprocal_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert!(outcome.upserted.is_empty());
        assert_eq!(
            outcome.retracted,
            BTreeSet::from(["peer-phone".to_string()])
        );
        assert_eq!(
            *store.deletes.lock().unwrap(),
            vec!["peer-phone".to_string()]
        );
        assert!(store.upserts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reconcile_reciprocal_tick_does_not_write_without_conversation_intent() {
        let store = MockReciprocalStore {
            endpoints: BTreeMap::from([(
                "did:key:phone".to_string(),
                endpoint("did:key:phone", "peer-phone", "/ticket/phone"),
            )]),
            ..Default::default()
        };

        let outcome = reconcile_reciprocal_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert!(outcome.upserted.is_empty());
        assert!(outcome.retracted.is_empty());
        assert!(store.upserts.lock().unwrap().is_empty());
        assert!(store.deletes.lock().unwrap().is_empty());
    }

    #[test]
    fn reciprocal_data_plane_upsert_uses_conversation_template_and_self_scope() {
        let mutation = upsert_reciprocal_data_plane_mutation(
            "peer-phone",
            "did:key:server",
            "/ticket/phone",
            "2026-07-08T00:00:00Z",
        )
        .unwrap();

        assert!(mutation.contains("upsert_DataPlanePairingDesired"));
        assert!(mutation.contains("peer_id: \"peer-phone\""));
        assert!(mutation.contains("agent_did: \"did:key:server\""));
        assert!(mutation.contains("template: \"conversation\""));
        assert!(mutation.contains("replicator_addresses: [\"/ticket/phone\"]"));
        assert!(mutation.contains("AgentRequest"));
        assert!(mutation.contains("AgentResponse"));
        assert!(!mutation.contains("[]"));
    }
}
