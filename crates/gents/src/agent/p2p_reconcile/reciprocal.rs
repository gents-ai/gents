use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName};
use gents_protocol::network_token::EndpointRecord;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

use super::graphql_helpers::{ensure_no_errors, first_row, graphql_string_list_literal, rows};
use super::network::{endpoint_is_fresh, NetworkEndpointEntry};
use super::templates::resolve_template;

pub const RECIPROCAL_CONVERSATION_TEMPLATE: &str = "conversation";
pub const SOURCE_RECIPROCAL: &str = "reciprocal";

/// Select endpoints that can materialize a reciprocal conversation data-plane
/// edge for a previously invited member DID.
///
/// This is intentionally pure: the store layer verifies `PeerEndpoint` records
/// and supplies only signed endpoint entries; the derivation only joins those
/// entries with `ReciprocalConversationIntent.member_did` and defers entries
/// without a dialable peer id/address.
pub fn derive_reciprocal_desired<'a>(
    intent_dids: &BTreeSet<String>,
    revoked_member_dids: &BTreeSet<String>,
    endpoints: &'a [NetworkEndpointEntry],
) -> Vec<&'a NetworkEndpointEntry> {
    endpoints
        .iter()
        .filter(|entry| intent_dids.contains(&entry.agent_did))
        .filter(|entry| !revoked_member_dids.contains(&entry.agent_did))
        .filter(|entry| !entry.peer_id.trim().is_empty())
        .filter(|entry| !entry.address.trim().is_empty())
        .collect()
}

/// Reciprocal-owned `DataPlanePairingDesired` state the tick reconciles. The
/// engine always materializes from the live signed endpoint, but the row must
/// still converge to the derived value (Lean `settled` is full-row equality,
/// not peer-id membership) — a drifted address would otherwise warn on every
/// engine sweep forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReciprocalRowState {
    pub address: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReciprocalTickOutcome {
    pub upserted: BTreeSet<String>,
    pub refreshed: BTreeSet<String>,
    pub retracted: BTreeSet<String>,
}

#[async_trait]
pub trait ReciprocalStore: Send + Sync {
    async fn load_intent_dids(&self) -> Result<BTreeSet<String>>;
    async fn load_revoked_member_dids(&self) -> Result<BTreeSet<String>>;
    async fn load_endpoint_for_did(&self, did: &str) -> Result<Option<NetworkEndpointEntry>>;
    async fn upsert_reciprocal_data_plane(
        &self,
        peer_id: &str,
        agent_did: &str,
        address: &str,
    ) -> Result<()>;
    async fn delete_reciprocal_data_plane(&self, peer_id: &str) -> Result<()>;
    async fn list_reciprocal_data_plane_rows(&self)
        -> Result<BTreeMap<String, ReciprocalRowState>>;
    /// Peers whose `DataPlanePairingDesired` row is owned by another source
    /// (operator/manual/legacy-null). Mirrors the network reconciler's blocked
    /// set: these peers are excluded from the desired set up front so the tick
    /// neither re-attempts a guarded upsert every sweep nor reports it as
    /// upserted.
    async fn list_non_reciprocal_data_plane_peers(&self) -> Result<BTreeSet<String>>;
}

pub async fn reconcile_reciprocal_tick(
    store: &dyn ReciprocalStore,
    self_did: &str,
) -> Result<ReciprocalTickOutcome> {
    let intent_dids = store
        .load_intent_dids()
        .await
        .context("load reciprocal conversation intents")?;
    let revoked_member_dids = store
        .load_revoked_member_dids()
        .await
        .context("load verified revoked network memberships")?;
    let mut endpoints = Vec::new();
    for did in &intent_dids {
        if revoked_member_dids.contains(did) {
            continue;
        }
        if let Some(endpoint) = store
            .load_endpoint_for_did(did)
            .await
            .with_context(|| format!("load verified PeerEndpoint for reciprocal DID {did}"))?
        {
            endpoints.push(endpoint);
        }
    }

    let blocked = store
        .list_non_reciprocal_data_plane_peers()
        .await
        .context("list non-reciprocal data-plane desired peers")?;
    let desired = derive_reciprocal_desired(&intent_dids, &revoked_member_dids, &endpoints)
        .into_iter()
        .filter(|entry| !blocked.contains(&entry.peer_id))
        .map(|entry| (entry.peer_id.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let existing = store
        .list_reciprocal_data_plane_rows()
        .await
        .context("list reciprocal data-plane desired rows")?;

    let mut outcome = ReciprocalTickOutcome::default();
    for (peer, entry) in &desired {
        match existing.get(peer) {
            Some(row) if row.address == entry.address => {}
            Some(_) => {
                store
                    .upsert_reciprocal_data_plane(&entry.peer_id, self_did, &entry.address)
                    .await
                    .with_context(|| {
                        format!("refresh reciprocal data-plane desired row for {peer}")
                    })?;
                outcome.refreshed.insert(peer.clone());
            }
            None => {
                store
                    .upsert_reciprocal_data_plane(&entry.peer_id, self_did, &entry.address)
                    .await
                    .with_context(|| {
                        format!("upsert reciprocal data-plane desired row for {peer}")
                    })?;
                outcome.upserted.insert(peer.clone());
            }
        }
    }
    for peer in existing.keys() {
        if !desired.contains_key(peer) {
            store
                .delete_reciprocal_data_plane(peer)
                .await
                .with_context(|| format!("delete reciprocal data-plane desired row for {peer}"))?;
            outcome.retracted.insert(peer.clone());
        }
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
            if !outcome.upserted.is_empty()
                || !outcome.refreshed.is_empty()
                || !outcome.retracted.is_empty()
            {
                tracing::info!(
                    upserted = ?outcome.upserted,
                    refreshed = ?outcome.refreshed,
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

    pub async fn load_materializable_entries(&self) -> Result<Vec<NetworkEndpointEntry>> {
        let intent_dids = <Self as ReciprocalStore>::load_intent_dids(self).await?;
        let revoked_member_dids = <Self as ReciprocalStore>::load_revoked_member_dids(self).await?;
        let mut endpoints = Vec::new();
        for did in intent_dids {
            if revoked_member_dids.contains(&did) {
                continue;
            }
            if let Some(endpoint) =
                <Self as ReciprocalStore>::load_endpoint_for_did(self, &did).await?
            {
                endpoints.push(endpoint);
            }
        }
        Ok(endpoints)
    }

    async fn existing_data_plane_ownership(&self, peer_id: &str) -> Result<DataPlaneOwnership> {
        let peer_id = escape_graphql_string(peer_id);
        let query = format!(
            r#"{{
                DataPlanePairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}, limit: 1) {{
                    source
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query DataPlanePairingDesired ownership")?;
        let Some(row) = first_row::<DataPlaneSourceRow>(&response, "DataPlanePairingDesired")?
        else {
            return Ok(DataPlaneOwnership::Absent);
        };
        Ok(data_plane_ownership_for_existing_row(row.source.as_deref()))
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

    async fn load_revoked_member_dids(&self) -> Result<BTreeSet<String>> {
        super::network::GraphqlNetworkStore::new(self.node.clone(), self.identity.clone())
            .load_revoked_member_dids()
            .await
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
                super::intervals::reciprocal_stale_after(),
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
        if should_skip_reciprocal_upsert(self.existing_data_plane_ownership(peer_id).await?) {
            tracing::warn!(
                peer_id = %peer_id,
                "skipping reciprocal DataPlanePairingDesired upsert because a non-reciprocal row already owns this peer"
            );
            return Ok(());
        }

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

    async fn list_reciprocal_data_plane_rows(
        &self,
    ) -> Result<BTreeMap<String, ReciprocalRowState>> {
        let query = r#"{
            DataPlanePairingDesired(filter: { source: { _eq: "reciprocal" } }) {
                peer_id
                replicator_addresses
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query reciprocal DataPlanePairingDesired rows")?;
        Ok(rows::<DataPlaneRow>(&response, "DataPlanePairingDesired")?
            .into_iter()
            .filter_map(|row| {
                let peer_id = row.peer_id.trim().to_string();
                if peer_id.is_empty() {
                    return None;
                }
                let address = row
                    .replicator_addresses
                    .unwrap_or_default()
                    .into_iter()
                    .map(|value| value.trim().to_string())
                    .find(|value| !value.is_empty())
                    .unwrap_or_default();
                Some((peer_id, ReciprocalRowState { address }))
            })
            .collect())
    }

    async fn list_non_reciprocal_data_plane_peers(&self) -> Result<BTreeSet<String>> {
        let query = r#"{
            DataPlanePairingDesired {
                peer_id
                source
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(
            &response,
            "query non-reciprocal DataPlanePairingDesired peers",
        )?;
        Ok(
            rows::<DataPlaneOwnershipRow>(&response, "DataPlanePairingDesired")?
                .into_iter()
                .filter(|row| !data_plane_source_is_reciprocal(row.source.as_deref()))
                .filter_map(|row| {
                    let peer_id = row.peer_id.trim().to_string();
                    (!peer_id.is_empty()).then_some(peer_id)
                })
                .collect(),
        )
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
    let source = escape_graphql_string(SOURCE_RECIPROCAL);
    let now = escape_graphql_string(now);
    Ok(format!(
        r#"mutation {{
            upsert_DataPlanePairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "{source}" }} }},
                add: {{
                    peer_id: "{peer_id}",
                    agent_did: "{agent_did}",
                    template: "{template}",
                    source: "{source}",
                    collections: {collections},
                    replicator_addresses: {addresses},
                    created_at: "{now}",
                    updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{agent_did}",
                    template: "{template}",
                    source: "{source}",
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
            delete_DataPlanePairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "reciprocal" }} }}) {{ _docID }}
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
            // Best-effort swallow: a transient verifier failure skips this row now
            // and retries on the next sweep instead of halting reconciliation.
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

fn data_plane_source_is_reciprocal(source: Option<&str>) -> bool {
    source.map(str::trim) == Some(SOURCE_RECIPROCAL)
}

fn data_plane_ownership_for_existing_row(source: Option<&str>) -> DataPlaneOwnership {
    if data_plane_source_is_reciprocal(source) {
        DataPlaneOwnership::Reciprocal
    } else {
        DataPlaneOwnership::NonReciprocal
    }
}

fn should_skip_reciprocal_upsert(existing: DataPlaneOwnership) -> bool {
    matches!(existing, DataPlaneOwnership::NonReciprocal)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataPlaneOwnership {
    Absent,
    Reciprocal,
    NonReciprocal,
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
    #[serde(default)]
    binding_sig: Option<String>,
}

#[derive(Deserialize)]
struct DataPlaneRow {
    peer_id: String,
    #[serde(default)]
    replicator_addresses: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct DataPlaneOwnershipRow {
    peer_id: String,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Deserialize)]
struct DataPlaneSourceRow {
    #[serde(default)]
    source: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gents_protocol::network_token::{MembershipRecord, NetworkRecord};

    use super::*;
    use crate::identity::KeyIdentity;

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

        let desired = derive_reciprocal_desired(&intents, &BTreeSet::new(), &endpoints);

        assert_eq!(desired.len(), 1);
        assert_eq!(desired[0].peer_id, "peer-phone");
        assert_eq!(desired[0].agent_did, "did:key:phone");
        assert_eq!(desired[0].address, "/ticket/phone");
    }

    #[test]
    fn derive_reciprocal_desired_defers_without_matching_endpoint() {
        let intents = BTreeSet::from(["did:key:phone".to_string()]);
        let endpoints = vec![endpoint("did:key:other", "peer-other", "/ticket/other")];

        let desired = derive_reciprocal_desired(&intents, &BTreeSet::new(), &endpoints);

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

        let desired = derive_reciprocal_desired(&intents, &BTreeSet::new(), &endpoints);

        assert!(desired.is_empty());
    }

    #[derive(Default)]
    struct MockReciprocalStore {
        intents: BTreeSet<String>,
        revoked_members: BTreeSet<String>,
        endpoints: BTreeMap<String, NetworkEndpointEntry>,
        existing: BTreeMap<String, ReciprocalRowState>,
        blocked: BTreeSet<String>,
        upserts: Mutex<Vec<(String, String, String)>>,
        deletes: Mutex<Vec<String>>,
    }

    fn existing_row(peer_id: &str, address: &str) -> (String, ReciprocalRowState) {
        (
            peer_id.to_string(),
            ReciprocalRowState {
                address: address.to_string(),
            },
        )
    }

    #[async_trait]
    impl ReciprocalStore for MockReciprocalStore {
        async fn load_intent_dids(&self) -> Result<BTreeSet<String>> {
            Ok(self.intents.clone())
        }

        async fn load_revoked_member_dids(&self) -> Result<BTreeSet<String>> {
            Ok(self.revoked_members.clone())
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

        async fn list_reciprocal_data_plane_rows(
            &self,
        ) -> Result<BTreeMap<String, ReciprocalRowState>> {
            Ok(self.existing.clone())
        }

        async fn list_non_reciprocal_data_plane_peers(&self) -> Result<BTreeSet<String>> {
            Ok(self.blocked.clone())
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
            existing: BTreeMap::from([existing_row("peer-phone", "/ticket/phone")]),
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
    async fn reconcile_reciprocal_tick_refreshes_row_when_endpoint_address_drifts() {
        let store = MockReciprocalStore {
            intents: BTreeSet::from(["did:key:phone".to_string()]),
            endpoints: BTreeMap::from([(
                "did:key:phone".to_string(),
                endpoint("did:key:phone", "peer-phone", "/ticket/phone-new"),
            )]),
            existing: BTreeMap::from([existing_row("peer-phone", "/ticket/phone-old")]),
            ..Default::default()
        };

        let outcome = reconcile_reciprocal_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert!(outcome.upserted.is_empty());
        assert!(outcome.retracted.is_empty());
        assert_eq!(
            outcome.refreshed,
            BTreeSet::from(["peer-phone".to_string()])
        );
        assert_eq!(
            *store.upserts.lock().unwrap(),
            vec![(
                "peer-phone".to_string(),
                "did:key:server".to_string(),
                "/ticket/phone-new".to_string()
            )]
        );
        assert!(store.deletes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reconcile_reciprocal_tick_is_quiescent_when_row_matches_endpoint() {
        let store = MockReciprocalStore {
            intents: BTreeSet::from(["did:key:phone".to_string()]),
            endpoints: BTreeMap::from([(
                "did:key:phone".to_string(),
                endpoint("did:key:phone", "peer-phone", "/ticket/phone"),
            )]),
            existing: BTreeMap::from([existing_row("peer-phone", "/ticket/phone")]),
            ..Default::default()
        };

        let outcome = reconcile_reciprocal_tick(&store, "did:key:server")
            .await
            .unwrap();

        // Quiescence matters: the reconciler sweeps on every Update event, so a
        // settled state must produce zero writes or each sweep would trigger the
        // next one.
        assert_eq!(outcome, ReciprocalTickOutcome::default());
        assert!(store.upserts.lock().unwrap().is_empty());
        assert!(store.deletes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reconcile_reciprocal_tick_skips_peers_owned_by_other_sources() {
        let store = MockReciprocalStore {
            intents: BTreeSet::from(["did:key:phone".to_string()]),
            endpoints: BTreeMap::from([(
                "did:key:phone".to_string(),
                endpoint("did:key:phone", "peer-phone", "/ticket/phone"),
            )]),
            blocked: BTreeSet::from(["peer-phone".to_string()]),
            ..Default::default()
        };

        let outcome = reconcile_reciprocal_tick(&store, "did:key:server")
            .await
            .unwrap();

        assert_eq!(outcome, ReciprocalTickOutcome::default());
        assert!(store.upserts.lock().unwrap().is_empty());
        assert!(store.deletes.lock().unwrap().is_empty());
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
    fn reciprocal_data_plane_source_guard_preserves_operator_rows() {
        assert_eq!(
            data_plane_ownership_for_existing_row(Some("reciprocal")),
            DataPlaneOwnership::Reciprocal
        );
        assert_eq!(
            data_plane_ownership_for_existing_row(Some(" reciprocal ")),
            DataPlaneOwnership::Reciprocal
        );
        assert_eq!(
            data_plane_ownership_for_existing_row(None),
            DataPlaneOwnership::NonReciprocal
        );
        assert_eq!(
            data_plane_ownership_for_existing_row(Some("")),
            DataPlaneOwnership::NonReciprocal
        );
        assert_eq!(
            data_plane_ownership_for_existing_row(Some("operator")),
            DataPlaneOwnership::NonReciprocal
        );

        assert!(!should_skip_reciprocal_upsert(DataPlaneOwnership::Absent));
        assert!(!should_skip_reciprocal_upsert(
            DataPlaneOwnership::Reciprocal
        ));
        assert!(
            should_skip_reciprocal_upsert(DataPlaneOwnership::NonReciprocal),
            "existing source=null/operator DataPlanePairingDesired rows must survive reciprocal intents"
        );
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
        assert!(mutation.contains(
            "filter: { peer_id: { _eq: \"peer-phone\" }, source: { _eq: \"reciprocal\" } }"
        ));
        assert!(mutation.contains("peer_id: \"peer-phone\""));
        assert!(mutation.contains("agent_did: \"did:key:server\""));
        assert!(mutation.contains("template: \"conversation\""));
        assert!(mutation.contains("source: \"reciprocal\""));
        assert!(mutation.contains("replicator_addresses: [\"/ticket/phone\"]"));
        assert!(mutation.contains("AgentRequest"));
        assert!(mutation.contains("AgentResponse"));
        assert!(!mutation.contains("[]"));
    }

    #[tokio::test]
    async fn graphql_tick_retracts_for_signed_revoked_membership() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let identity = Arc::new(KeyIdentity::load_or_create(
            tempdir.path().join("admin.key"),
            None,
        )?);
        let now = "2026-07-14T00:00:00Z";
        let member_did = "did:key:revoked-member";

        let mut network = NetworkRecord {
            network_id: "network-a".to_string(),
            admin_did: identity.did().to_string(),
            display_name: "Network A".to_string(),
            default_template: "network-control".to_string(),
            created_at: now.to_string(),
            sig: Vec::new(),
        };
        network.sig = identity.sign(&network.signing_payload()).await?;
        let mut membership = MembershipRecord {
            network_id: network.network_id.clone(),
            member_did: member_did.to_string(),
            status: "revoked".to_string(),
            granted_at: now.to_string(),
            revoked_at: now.to_string(),
            sig: Vec::new(),
        };
        membership.sig = identity.sign(&membership.signing_payload()).await?;
        let network_sig = bs58::encode(&network.sig).into_string();
        let membership_sig = bs58::encode(&membership.sig).into_string();

        let seed = format!(
            r#"mutation {{
                create_AgentNetwork(input: {{
                    network_id: "network-a",
                    admin_did: "{admin_did}",
                    display_name: "Network A",
                    default_template: "network-control",
                    created_at: "{now}",
                    admin_sig: "{network_sig}"
                }}) {{ _docID }}
                create_NetworkMembership(input: {{
                    membership_key: "network-a:{member_did}",
                    network_id: "network-a",
                    member_did: "{member_did}",
                    status: "revoked",
                    granted_at: "{now}",
                    revoked_at: "{now}",
                    admin_sig: "{membership_sig}"
                }}) {{ _docID }}
                create_ReciprocalConversationIntent(input: {{
                    member_did: "{member_did}",
                    template: "conversation",
                    created_at: "{now}",
                    updated_at: "{now}"
                }}) {{ _docID }}
                create_DataPlanePairingDesired(input: {{
                    peer_id: "peer-revoked",
                    agent_did: "{admin_did}",
                    collections: ["AgentRequest"],
                    replicator_addresses: ["/ticket/revoked"],
                    template: "conversation",
                    source: "reciprocal",
                    created_at: "{now}",
                    updated_at: "{now}"
                }}) {{ _docID }}
            }}"#,
            admin_did = escape_graphql_string(identity.did()),
            network_sig = escape_graphql_string(&network_sig),
            membership_sig = escape_graphql_string(&membership_sig),
        );
        let response = node.execute(&seed).await;
        ensure_no_errors(&response, "seed reciprocal revocation regression")?;

        let store = GraphqlReciprocalStore::new(node.clone(), identity);
        let outcome = reconcile_reciprocal_tick(&store, "did:key:server").await?;

        assert_eq!(
            outcome.retracted,
            BTreeSet::from(["peer-revoked".to_string()])
        );
        let response = node
            .execute(
                r#"{
                    DataPlanePairingDesired(filter: { peer_id: { _eq: "peer-revoked" } }) {
                        peer_id
                    }
                }"#,
            )
            .await;
        ensure_no_errors(&response, "query reciprocal row after revocation")?;
        assert!(rows::<DataPlaneRow>(&response, "DataPlanePairingDesired")?.is_empty());
        Ok(())
    }
}
