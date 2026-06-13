//! Runtime pairing reconcile engine.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;

use super::profiles::expand_p2p_collection_profile_ids;
use super::{
    compute_owned_pairing_diff, DiffOp, EmbeddedRemoteP2pAdmin, PairingActual, PairingApplied,
    PairingDesired, RemoteP2pAdmin,
};

pub const PAIRING_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTickOutcome {
    pub peer_id: String,
    pub ops_applied: Vec<DiffOp>,
    pub desired_read_failed: bool,
}

#[async_trait]
pub trait PairingStateStore: Send + Sync {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>>;

    async fn load_applied(&self, peer_id: &str) -> Result<PairingApplied>;

    async fn save_applied(&self, peer_id: &str, applied: &PairingApplied) -> Result<()>;

    async fn delete_applied(&self, peer_id: &str) -> Result<()>;

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>>;
}

pub async fn reconcile_peer_tick(
    admin: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore,
    peer_id: &str,
) -> Result<PairingTickOutcome> {
    let desired = match store.load_desired(peer_id).await {
        Ok(desired) => desired,
        Err(error) => {
            tracing::warn!(
                peer_id,
                error = %error,
                "pairing desired state read failed; skipping reconcile tick"
            );
            return Ok(PairingTickOutcome {
                peer_id: peer_id.to_string(),
                ops_applied: Vec::new(),
                desired_read_failed: true,
            });
        }
    };
    let desired_state = desired.clone().unwrap_or_default();
    if desired_state.has_wiring() && !desired_state.replicator_addresses.is_empty() {
        let addresses = desired_state
            .replicator_addresses
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        admin
            .connect(&addresses)
            .await
            .context("connect pairing peer")?;
    }
    let mut applied = store.load_applied(peer_id).await?;
    let actual = read_actual(admin).await?;
    let ops = compute_owned_pairing_diff(&desired_state, &actual.state, &applied);
    let mut ops_applied = Vec::new();

    for op in ops {
        apply_op(admin, &op, &desired_state, &actual).await?;
        update_applied_after_success(&mut applied, &op);
        persist_applied(store, peer_id, &applied).await?;
        ops_applied.push(op);
    }

    if desired.is_none() && !applied.is_empty() {
        store.delete_applied(peer_id).await?;
    }

    Ok(PairingTickOutcome {
        peer_id: peer_id.to_string(),
        ops_applied,
        desired_read_failed: false,
    })
}

pub async fn run_pairing_reconciler(
    node: Arc<EmbeddedNode>,
    cancel: CancellationToken,
) -> Result<()> {
    if node.p2p_arc().is_none() {
        tracing::debug!("pairing reconciler idle because embedded node has no P2P transport");
        cancel.cancelled().await;
        return Ok(());
    }

    let admin = EmbeddedRemoteP2pAdmin::new(node.clone());
    let store = GraphqlPairingStateStore::new(node.clone());
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(PAIRING_SWEEP_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_pairings(&admin, &store).await?;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                sweep_pairings(&admin, &store).await?;
            }
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("pairing reconciler update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "pairing reconciler update subscription dropped messages");
                }
                sweep_pairings(&admin, &store).await?;
            }
        }
    }
}

async fn sweep_pairings(admin: &dyn RemoteP2pAdmin, store: &dyn PairingStateStore) -> Result<()> {
    for peer_id in store.list_peer_ids().await? {
        match reconcile_peer_tick(admin, store, &peer_id).await {
            Ok(outcome) => {
                if outcome.desired_read_failed {
                    continue;
                }
                if !outcome.ops_applied.is_empty() {
                    tracing::info!(
                        peer_id = %outcome.peer_id,
                        ops = ?outcome.ops_applied,
                        "pairing reconcile applied operations"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    peer_id = %peer_id,
                    error = %error,
                    "pairing reconcile tick failed"
                );
            }
        }
    }
    Ok(())
}

struct ActualSnapshot {
    state: PairingActual,
    replicator_ids_by_addr: BTreeMap<String, String>,
    replicator_collections_by_addr: BTreeMap<String, Vec<String>>,
}

async fn read_actual(admin: &dyn RemoteP2pAdmin) -> Result<ActualSnapshot> {
    let collections = admin
        .list_p2p_collections()
        .await
        .context("list remote P2P collections")?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let remote_replicators = admin
        .list_replicators()
        .await
        .context("list remote P2P replicators")?;
    let replicator_addresses = remote_replicators
        .iter()
        .filter_map(|replicator| replicator.address.clone())
        .collect::<BTreeSet<_>>();
    let replicator_ids_by_addr = remote_replicators
        .iter()
        .filter_map(|replicator| Some((replicator.address.clone()?, replicator.id.clone()?)))
        .collect::<BTreeMap<_, _>>();
    let replicator_collections_by_addr = remote_replicators
        .into_iter()
        .filter_map(|replicator| Some((replicator.address?, replicator.collections)))
        .collect::<BTreeMap<_, _>>();

    Ok(ActualSnapshot {
        state: PairingActual {
            collections,
            replicator_addresses,
        },
        replicator_ids_by_addr,
        replicator_collections_by_addr,
    })
}

async fn apply_op(
    admin: &dyn RemoteP2pAdmin,
    op: &DiffOp,
    desired: &PairingDesired,
    actual: &ActualSnapshot,
) -> Result<()> {
    match op {
        DiffOp::InstallCollection(collection) => admin
            .add_p2p_collections(std::slice::from_ref(collection))
            .await
            .with_context(|| format!("install P2P collection {collection}")),
        DiffOp::TeardownCollection(collection) => admin
            .delete_p2p_collections(std::slice::from_ref(collection))
            .await
            .with_context(|| format!("teardown P2P collection {collection}")),
        DiffOp::InstallReplicator(address) => {
            let addresses = vec![address.clone()];
            let collections = desired.collections.iter().cloned().collect::<Vec<_>>();
            admin
                .add_replicator(&addresses, &collections)
                .await
                .with_context(|| format!("install P2P replicator {address}"))
        }
        DiffOp::TeardownReplicator(address) => {
            let id = actual
                .replicator_ids_by_addr
                .get(address)
                .map(String::as_str)
                .unwrap_or(address.as_str());
            let collections = actual
                .replicator_collections_by_addr
                .get(address)
                .cloned()
                .filter(|collections| !collections.is_empty())
                .unwrap_or_else(|| desired.collections.iter().cloned().collect());
            admin
                .delete_replicator(id, &collections)
                .await
                .with_context(|| format!("teardown P2P replicator {address}"))
        }
    }
}

pub fn update_applied_after_success(applied: &mut PairingApplied, op: &DiffOp) {
    match op {
        DiffOp::InstallCollection(collection) => {
            applied.collections.insert(collection.clone());
        }
        DiffOp::TeardownCollection(collection) => {
            applied.collections.remove(collection);
        }
        DiffOp::InstallReplicator(address) => {
            applied.replicator_addresses.insert(address.clone());
        }
        DiffOp::TeardownReplicator(address) => {
            applied.replicator_addresses.remove(address);
        }
    }
}

async fn persist_applied(
    store: &dyn PairingStateStore,
    peer_id: &str,
    applied: &PairingApplied,
) -> Result<()> {
    if applied.is_empty() {
        store.delete_applied(peer_id).await
    } else {
        store.save_applied(peer_id, applied).await
    }
}

#[derive(Clone)]
pub struct GraphqlPairingStateStore {
    node: Arc<EmbeddedNode>,
}

impl GraphqlPairingStateStore {
    pub fn new(node: Arc<EmbeddedNode>) -> Self {
        Self { node }
    }
}

#[async_trait]
impl PairingStateStore for GraphqlPairingStateStore {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>> {
        let peer_id = escape_graphql_string(peer_id);
        let query = format!(
            r#"{{
                PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    collections
                    replicator_addresses
                    profiles
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query PeerPairingDesired")?;
        first_row::<PairingStateRow>(&response, "PeerPairingDesired")?
            .map(desired_from_pairing_row)
            .transpose()
    }

    async fn load_applied(&self, peer_id: &str) -> Result<PairingApplied> {
        let peer_id = escape_graphql_string(peer_id);
        let query = format!(
            r#"{{
                PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    collections
                    replicator_addresses
                }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query PeerPairingApplied")?;
        Ok(
            first_row::<PairingStateRow>(&response, "PeerPairingApplied")?
                .map(|row| PairingApplied {
                    collections: row.collections.unwrap_or_default().into_iter().collect(),
                    replicator_addresses: row
                        .replicator_addresses
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                })
                .unwrap_or_default(),
        )
    }

    async fn save_applied(&self, peer_id: &str, applied: &PairingApplied) -> Result<()> {
        let peer_id = escape_graphql_string(peer_id);
        let collections = graphql_nullable_string_array(&applied.collections);
        let replicator_addresses = graphql_nullable_string_array(&applied.replicator_addresses);
        let now = escape_graphql_string(
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let mutation = format!(
            r#"mutation {{
                upsert_PeerPairingApplied(
                    filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                    add: {{
                        peer_id: "{peer_id}",
                        collections: {collections},
                        replicator_addresses: {replicator_addresses},
                        created_at: "{now}",
                        updated_at: "{now}"
                    }},
                    update: {{
                        collections: {collections},
                        replicator_addresses: {replicator_addresses},
                        updated_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "upsert PeerPairingApplied")
    }

    async fn delete_applied(&self, peer_id: &str) -> Result<()> {
        let peer_id = escape_graphql_string(peer_id);
        let mutation = format!(
            r#"mutation {{
                delete_PeerPairingApplied(
                    filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}
                ) {{ _docID }}
            }}"#
        );
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "delete PeerPairingApplied")
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        let query = r#"{
            PeerPairingDesired { peer_id }
            PeerPairingApplied { peer_id }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query pairing peer ids")?;
        let mut ids = BTreeSet::new();
        for row in rows::<PeerIdRow>(&response, "PeerPairingDesired")? {
            if !row.peer_id.trim().is_empty() {
                ids.insert(row.peer_id);
            }
        }
        for row in rows::<PeerIdRow>(&response, "PeerPairingApplied")? {
            if !row.peer_id.trim().is_empty() {
                ids.insert(row.peer_id);
            }
        }
        Ok(ids)
    }
}

#[derive(Deserialize)]
struct PairingStateRow {
    collections: Option<Vec<String>>,
    replicator_addresses: Option<Vec<String>>,
    profiles: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct PeerIdRow {
    peer_id: String,
}

fn desired_from_pairing_row(row: PairingStateRow) -> Result<PairingDesired> {
    let explicit_collections = row.collections.unwrap_or_default();
    let profile_ids = row.profiles.unwrap_or_default();
    let collections = if explicit_collections.is_empty() && profile_ids.is_empty() {
        BTreeSet::new()
    } else {
        expand_p2p_collection_profile_ids(
            explicit_collections.iter().map(String::as_str),
            profile_ids.iter().map(String::as_str),
        )?
    };

    Ok(PairingDesired {
        collections,
        replicator_addresses: row
            .replicator_addresses
            .unwrap_or_default()
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
    })
}

fn ensure_no_errors(response: &QueryResponse, label: &str) -> Result<()> {
    if response.has_errors() {
        bail!("{label} failed: {:?}", response.errors);
    }
    Ok(())
}

fn first_row<T>(response: &QueryResponse, field: &str) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    Ok(rows::<T>(response, field)?.into_iter().next())
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

fn graphql_nullable_string_array(values: &BTreeSet<String>) -> String {
    if values.is_empty() {
        return "null".to_string();
    }
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use anyhow::anyhow;

    use super::*;
    use crate::agent::p2p_reconcile::{RemoteP2pAdminResult, RemoteReplicator};

    fn set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    struct MockStore {
        desired: Mutex<Result<Option<PairingDesired>, String>>,
        applied: Mutex<PairingApplied>,
        saved: Mutex<Vec<PairingApplied>>,
        deleted: Mutex<usize>,
    }

    impl Default for MockStore {
        fn default() -> Self {
            Self {
                desired: Mutex::new(Ok(None)),
                applied: Mutex::new(PairingApplied::default()),
                saved: Mutex::new(Vec::new()),
                deleted: Mutex::new(0),
            }
        }
    }

    impl MockStore {
        fn with_desired(desired: Option<PairingDesired>) -> Self {
            Self {
                desired: Mutex::new(Ok(desired)),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl PairingStateStore for MockStore {
        async fn load_desired(&self, _peer_id: &str) -> Result<Option<PairingDesired>> {
            self.desired
                .lock()
                .unwrap()
                .clone()
                .map_err(|message| anyhow!(message))
        }

        async fn load_applied(&self, _peer_id: &str) -> Result<PairingApplied> {
            Ok(self.applied.lock().unwrap().clone())
        }

        async fn save_applied(&self, _peer_id: &str, applied: &PairingApplied) -> Result<()> {
            *self.applied.lock().unwrap() = applied.clone();
            self.saved.lock().unwrap().push(applied.clone());
            Ok(())
        }

        async fn delete_applied(&self, _peer_id: &str) -> Result<()> {
            *self.applied.lock().unwrap() = PairingApplied::default();
            *self.deleted.lock().unwrap() += 1;
            Ok(())
        }

        async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
            Ok(set(&["peer-a"]))
        }
    }

    #[derive(Default)]
    struct MockAdmin {
        collections: Mutex<BTreeSet<String>>,
        replicators: Mutex<BTreeMap<String, RemoteReplicator>>,
        emitted: Mutex<Vec<DiffOp>>,
        connects: Mutex<Vec<Vec<String>>>,
    }

    #[async_trait]
    impl RemoteP2pAdmin for MockAdmin {
        async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }

        async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }

        async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()> {
            self.connects.lock().unwrap().push(addresses.to_vec());
            Ok(())
        }

        async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
            Ok(self.replicators.lock().unwrap().values().cloned().collect())
        }

        async fn add_replicator(
            &self,
            addresses: &[String],
            collections: &[String],
        ) -> RemoteP2pAdminResult<()> {
            for address in addresses {
                self.replicators.lock().unwrap().insert(
                    address.clone(),
                    RemoteReplicator {
                        id: Some(format!("id-{address}")),
                        collections: collections.to_vec(),
                        address: Some(address.clone()),
                    },
                );
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::InstallReplicator(address.clone()));
            }
            Ok(())
        }

        async fn delete_replicator(
            &self,
            id: &str,
            _collections: &[String],
        ) -> RemoteP2pAdminResult<()> {
            let key = self
                .replicators
                .lock()
                .unwrap()
                .iter()
                .find_map(|(address, replicator)| {
                    (replicator.id.as_deref() == Some(id) || address == id).then(|| address.clone())
                });
            if let Some(key) = key {
                self.replicators.lock().unwrap().remove(&key);
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::TeardownReplicator(key));
            }
            Ok(())
        }

        async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(self.collections.lock().unwrap().iter().cloned().collect())
        }

        async fn add_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
            for collection in collections {
                self.collections.lock().unwrap().insert(collection.clone());
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::InstallCollection(collection.clone()));
            }
            Ok(())
        }

        async fn delete_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
            for collection in collections {
                self.collections.lock().unwrap().remove(collection);
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::TeardownCollection(collection.clone()));
            }
            Ok(())
        }

        async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }

        async fn add_p2p_documents(&self, _doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
            Ok(())
        }

        async fn delete_p2p_documents(&self, _doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
            Ok(())
        }

        async fn sync_documents(
            &self,
            _collection_name: &str,
            _doc_ids: &[String],
            _timeout: Option<Duration>,
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }

        async fn sync_collection_versions(
            &self,
            _version_ids: &[String],
            _timeout: Option<Duration>,
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }

        async fn sync_branchable_collection(
            &self,
            _collection_id: &str,
            _timeout: Option<Duration>,
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn read_failure_noops_without_remote_reads() {
        let store = MockStore {
            desired: Mutex::new(Err("boom".into())),
            ..Default::default()
        };
        let admin = MockAdmin::default();

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert!(outcome.desired_read_failed);
        assert!(outcome.ops_applied.is_empty());
        assert!(admin.emitted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn install_updates_applied_after_success() {
        let store = MockStore::with_desired(Some(PairingDesired {
            collections: set(&["c1"]),
            replicator_addresses: set(&["addr1"]),
        }));
        let admin = MockAdmin::default();

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert_eq!(
            outcome.ops_applied,
            vec![
                DiffOp::InstallCollection("c1".into()),
                DiffOp::InstallReplicator("addr1".into())
            ]
        );
        assert_eq!(*admin.connects.lock().unwrap(), vec![vec!["addr1"]]);
        assert_eq!(
            *store.applied.lock().unwrap(),
            PairingApplied {
                collections: set(&["c1"]),
                replicator_addresses: set(&["addr1"]),
            }
        );
    }

    #[tokio::test]
    async fn teardown_is_restricted_to_applied_actual_extras() {
        let store = MockStore::with_desired(Some(PairingDesired::default()));
        *store.applied.lock().unwrap() = PairingApplied {
            collections: set(&["managed"]),
            replicator_addresses: set(&["managed-addr"]),
        };
        let admin = MockAdmin::default();
        *admin.collections.lock().unwrap() = set(&["managed", "manual"]);
        admin.replicators.lock().unwrap().insert(
            "managed-addr".into(),
            RemoteReplicator {
                id: Some("managed-id".into()),
                collections: vec!["managed".into()],
                address: Some("managed-addr".into()),
            },
        );
        admin.replicators.lock().unwrap().insert(
            "manual-addr".into(),
            RemoteReplicator {
                id: Some("manual-id".into()),
                collections: vec!["manual".into()],
                address: Some("manual-addr".into()),
            },
        );

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert_eq!(
            outcome.ops_applied,
            vec![
                DiffOp::TeardownCollection("managed".into()),
                DiffOp::TeardownReplicator("managed-addr".into())
            ]
        );
        assert_eq!(*admin.collections.lock().unwrap(), set(&["manual"]));
        assert!(admin
            .replicators
            .lock()
            .unwrap()
            .contains_key("manual-addr"));
    }

    #[tokio::test]
    async fn desired_absent_tears_down_managed_state_and_deletes_applied_row() {
        let store = MockStore::with_desired(None);
        *store.applied.lock().unwrap() = PairingApplied {
            collections: set(&["c1"]),
            replicator_addresses: set(&["addr1"]),
        };
        let admin = MockAdmin::default();
        *admin.collections.lock().unwrap() = set(&["c1"]);
        admin.replicators.lock().unwrap().insert(
            "addr1".into(),
            RemoteReplicator {
                id: Some("id-addr1".into()),
                collections: vec!["c1".into()],
                address: Some("addr1".into()),
            },
        );

        let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
            .await
            .expect("tick result");

        assert_eq!(
            outcome.ops_applied,
            vec![
                DiffOp::TeardownCollection("c1".into()),
                DiffOp::TeardownReplicator("addr1".into())
            ]
        );
        assert_eq!(*store.deleted.lock().unwrap(), 1);
        assert!(store.applied.lock().unwrap().is_empty());
    }

    #[test]
    fn nullable_graphql_arrays_emit_null_when_empty() {
        assert_eq!(graphql_nullable_string_array(&BTreeSet::new()), "null");
        assert_eq!(
            graphql_nullable_string_array(&set(&["a", "b"])),
            r#"["a", "b"]"#
        );
    }

    #[test]
    fn desired_row_profiles_resolve_to_collections_at_load_boundary() {
        let desired = desired_from_pairing_row(PairingStateRow {
            collections: None,
            replicator_addresses: Some(vec!["addr1".into()]),
            profiles: Some(vec!["chat-requests".into()]),
        })
        .expect("profile resolves");

        assert!(desired.collections.contains("AgentRequest"));
        assert!(desired.collections.contains("AgentResponse"));
        assert_eq!(desired.replicator_addresses, set(&["addr1"]));
    }

    #[test]
    fn desired_row_unknown_profile_is_load_error() {
        let error = desired_from_pairing_row(PairingStateRow {
            collections: None,
            replicator_addresses: Some(vec!["addr1".into()]),
            profiles: Some(vec!["not-a-profile".into()]),
        })
        .unwrap_err();

        assert!(error.to_string().contains("unknown P2P collection profile"));
    }
}
