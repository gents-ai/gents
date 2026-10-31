use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use tokio::sync::watch;
use tokio::sync::RwLock;

use gents::runtime::agent::p2p_reconcile::{
    compare_client_replicated_schema, read_client_replicated_schema,
};
use gents::P2pSyncStatusSnapshot;
use gents_protocol::peer_schema::{
    ReplicatedSchema, ReplicatedSchemaSkew, STATUS_REPLICATED_SCHEMA_FIELD,
};
use serde_json::Value;

use super::enrollment::reconcile::EnrollmentAuthorizationGeneration;
use super::{
    p2p_health_materially_changed, ClientPeerStatus, ClientSyncStateSnapshot,
    EnrollmentActivationError, P2PHealth,
};
use crate::client::peer_directory::RemovalCause;
use crate::client::{PeerDirectory, PeerRecord};

type LastErrorPatch = (Option<String>, Option<String>);

/// Replicated-schema compatibility is a live observation between two running
/// nodes, owned here, and deliberately not part of the signed enrollment
/// schema fingerprint: signed offers are re-verified against the current
/// fingerprint on every projection, so folding replicated versions into it
/// would invalidate every membership on any schema-changing release even
/// when both sides upgrade together.
#[derive(Clone)]
pub(in crate::client) struct ClientSyncStateOwner {
    tx: watch::Sender<ClientSyncStateSnapshot>,
    directory: Arc<RwLock<PeerDirectory>>,
    enrollment_observation_generations:
        Arc<StdMutex<BTreeMap<String, EnrollmentAuthorizationGeneration>>>,
    advertised_enrollment_labels: Arc<StdRwLock<BTreeMap<String, String>>>,
    local_replicated_schema: Arc<tokio::sync::OnceCell<ReplicatedSchema>>,
    schema_observation_sequence: Arc<std::sync::atomic::AtomicU64>,
    schema_observation_latest: Arc<std::sync::Mutex<BTreeMap<String, u64>>>,
}

impl ClientSyncStateOwner {
    #[cfg(test)]
    pub(super) async fn for_test(
        records: Vec<PeerRecord>,
        peers: Vec<ClientPeerStatus>,
    ) -> (tempfile::TempDir, Self) {
        let tempdir = tempfile::tempdir().expect("configured peer owner tempdir");
        let mut directory = PeerDirectory::open_writer(tempdir.path().join("peers.json"))
            .await
            .expect("load configured peer owner directory");
        for record in records {
            directory
                .upsert(record)
                .await
                .expect("seed configured peer owner directory");
        }
        let owner = Self::new(P2PHealth::default(), directory, peers);
        (tempdir, owner)
    }

    pub(in crate::client) fn new(
        transport: P2PHealth,
        directory: PeerDirectory,
        peers: Vec<ClientPeerStatus>,
    ) -> Self {
        let mut records = directory.records().to_vec();
        let directory = Arc::new(RwLock::new(directory));
        sort_records(&mut records);
        let mut peers = records
            .iter()
            .map(|record| {
                peers
                    .iter()
                    .find(|status| status.peer_id == record.peer_id)
                    .cloned()
                    .unwrap_or_else(|| conservative_status(record))
            })
            .collect::<Vec<_>>();
        sort_statuses(&mut peers);
        let (tx, _) = watch::channel(ClientSyncStateSnapshot {
            transport,
            database_sync: None,
            database_sync_error: None,
            enrollment_activation_errors: BTreeMap::new(),
            peer_schema_skew: BTreeMap::new(),
            directory: records,
            peers,
        });
        Self {
            tx,
            directory,
            enrollment_observation_generations: Arc::new(StdMutex::new(BTreeMap::new())),
            advertised_enrollment_labels: Arc::new(StdRwLock::new(BTreeMap::new())),
            local_replicated_schema: Arc::new(tokio::sync::OnceCell::new()),
            schema_observation_sequence: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            schema_observation_latest: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
        }
    }

    pub(super) fn retain_enrollment_activation_errors(
        &self,
        current: &BTreeMap<String, EnrollmentAuthorizationGeneration>,
    ) {
        let mut observed = self
            .enrollment_observation_generations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        observed.clone_from(current);
        self.tx.send_if_modified(|state| {
            let before = state.enrollment_activation_errors.len();
            state
                .enrollment_activation_errors
                .retain(|request_id, error| {
                    current.get(request_id).is_some_and(|generation| {
                        error.request_digest == generation.request_digest
                            && error.authorization_sequence == generation.sequence
                            && error.authorization_expires_at == generation.expires_at
                    })
                });
            state.enrollment_activation_errors.len() != before
        });
    }

    pub(super) fn finish_enrollment_activation_errors(
        &self,
        generations: &BTreeMap<String, EnrollmentAuthorizationGeneration>,
        errors: &BTreeMap<String, EnrollmentActivationError>,
    ) {
        let current = self
            .enrollment_observation_generations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.tx.send_if_modified(|state| {
            let before = state.enrollment_activation_errors.clone();
            for (request, generation) in generations {
                if current.get(request) != Some(generation) {
                    continue;
                }
                match errors.get(request) {
                    Some(error)
                        if error.request_digest == generation.request_digest
                            && error.authorization_sequence == generation.sequence
                            && error.authorization_expires_at == generation.expires_at =>
                    {
                        state
                            .enrollment_activation_errors
                            .insert(request.clone(), error.clone());
                    }
                    None => {
                        state.enrollment_activation_errors.remove(request);
                    }
                    _ => {}
                }
            }
            before != state.enrollment_activation_errors
        });
    }

    pub(super) fn project_enrollment_activation(
        &self,
        request: &str,
        generation: &EnrollmentAuthorizationGeneration,
        approved: bool,
        route_failure: bool,
    ) -> Option<&'static str> {
        let current = self
            .enrollment_observation_generations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !approved || current.get(request) != Some(generation) {
            return None;
        }
        let snapshot = self.tx.borrow();
        snapshot
            .enrollment_activation_errors
            .get(request)
            .filter(|error| {
                error.request_digest == generation.request_digest
                    && error.authorization_sequence == generation.sequence
                    && error.authorization_expires_at == generation.expires_at
            })
            .map(|error| error.stage.as_str())
            .or_else(|| route_failure.then_some(super::EnrollmentActivationStage::Route.as_str()))
    }

    pub(super) fn remember_enrollment_label(&self, server_peer: &str, label: &str) {
        let label = label.trim();
        if server_peer.trim().is_empty() || label.is_empty() {
            return;
        }
        if let Ok(mut labels) = self.advertised_enrollment_labels.write() {
            labels.insert(server_peer.to_string(), label.to_string());
        }
    }

    pub(super) fn advertised_enrollment_label(&self, server_peer: &str) -> Option<String> {
        self.advertised_enrollment_labels
            .read()
            .ok()?
            .get(server_peer)
            .cloned()
    }

    pub(super) fn snapshot(&self) -> ClientSyncStateSnapshot {
        self.tx.borrow().clone()
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<ClientSyncStateSnapshot> {
        self.tx.subscribe()
    }

    pub(super) fn peer(&self, peer_id: &str) -> Option<ClientPeerStatus> {
        self.tx
            .borrow()
            .peers
            .iter()
            .find(|status| status.peer_id == peer_id)
            .cloned()
    }

    pub(in crate::client) fn records(&self) -> Vec<PeerRecord> {
        self.tx.borrow().directory.clone()
    }

    pub(super) async fn pending_removals(&self) -> Vec<PeerRecord> {
        self.directory.read().await.pending_removals().to_vec()
    }

    pub(super) async fn retired_enrollment_digests(&self) -> BTreeSet<String> {
        self.directory.read().await.retired_enrollment_digests()
    }

    pub(super) async fn prune_retired_enrollments(
        &self,
        authority_server_peers: &BTreeSet<String>,
    ) -> anyhow::Result<()> {
        let mut directory = self.directory.write().await;
        directory
            .prune_retired_enrollments(authority_server_peers)
            .await
    }

    pub(super) async fn clear_ephemeral_pairing_readiness(&self) -> anyhow::Result<()> {
        let mut directory = self.directory.write().await;
        directory.clear_ephemeral_pairing_readiness().await?;
        let records = directory.records().to_vec();
        self.publish_persisted_directory(records);
        Ok(())
    }

    pub(super) async fn has_pending_removal(&self, expected: &PeerRecord) -> bool {
        self.directory.read().await.has_pending_removal(expected)
    }

    pub(super) async fn upsert_local_standard_peer(
        &self,
        label: &str,
        addr: &str,
        node_did: &str,
        graphql: &str,
        node_home: &str,
    ) -> anyhow::Result<PeerRecord> {
        let mut directory = self.directory.write().await;
        let record = directory
            .upsert_local_standard_peer(label, addr, node_did, graphql, node_home)
            .await?;
        let records = directory.records().to_vec();
        self.publish_persisted_directory(records);
        Ok(record)
    }

    pub(super) async fn remember_enrollment_endpoint(
        &self,
        peer_id: &str,
        node_did: &str,
        request_digest: &str,
        expires_at: &str,
        endpoint: &str,
    ) -> anyhow::Result<()> {
        self.directory
            .write()
            .await
            .remember_enrollment_endpoint(peer_id, node_did, request_digest, expires_at, endpoint)
            .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "fields of one enrollment authorization"
    )]
    pub(super) async fn upsert_enrollment_peer(
        &self,
        peer_id: &str,
        label: &str,
        addr: &str,
        node_did: &str,
        network_id: &str,
        request_id: &str,
        request_digest: &str,
        admin_did: &str,
        authorization_sequence: u64,
        authorization_expires_at: &str,
    ) -> anyhow::Result<PeerRecord> {
        let mut directory = self.directory.write().await;
        let record = directory
            .upsert_enrollment_peer(
                peer_id,
                label,
                addr,
                node_did,
                network_id,
                request_id,
                request_digest,
                admin_did,
                authorization_sequence,
                authorization_expires_at,
            )
            .await?;
        self.publish_persisted_directory(directory.records().to_vec());
        Ok(record)
    }

    pub(super) async fn replace_record(
        &self,
        expected: &PeerRecord,
        replacement: PeerRecord,
    ) -> anyhow::Result<Option<PeerRecord>> {
        let mut directory = self.directory.write().await;
        let replaced = directory.replace_if_matches(expected, replacement).await?;
        if replaced.is_some() {
            self.publish_persisted_directory(directory.records().to_vec());
        }
        Ok(replaced)
    }

    pub(super) async fn set_pairing_ready(
        &self,
        expected: &PeerRecord,
        ready: bool,
    ) -> anyhow::Result<Option<PeerRecord>> {
        let mut directory = self.directory.write().await;
        if !directory.records().iter().any(|record| record == expected) {
            return Ok(None);
        }
        let record = directory
            .set_pairing_ready(&expected.peer_id, ready)
            .await?;
        if record.is_some() {
            self.publish_persisted_directory(directory.records().to_vec());
        }
        Ok(record)
    }

    pub(super) async fn queue_removal(
        &self,
        expected: &PeerRecord,
        cause: RemovalCause,
    ) -> anyhow::Result<Option<PeerRecord>> {
        let mut directory = self.directory.write().await;
        let removed = directory.queue_removal(expected, cause).await?;
        let records = directory.records().to_vec();
        self.publish_persisted_directory(records);
        Ok(removed)
    }

    pub(super) async fn complete_removal_if_matches(
        &self,
        expected: &PeerRecord,
    ) -> anyhow::Result<bool> {
        let mut directory = self.directory.write().await;
        let completed = directory.complete_removal_if_matches(expected).await?;
        if completed {
            self.publish_persisted_directory(directory.records().to_vec());
        }
        Ok(completed)
    }

    pub(super) fn replace_database_observation(
        &self,
        transport: P2PHealth,
        database_sync: Option<P2pSyncStatusSnapshot>,
        database_sync_error: Option<String>,
    ) {
        self.tx.send_if_modified(|state| {
            let transport_changed = p2p_health_materially_changed(&state.transport, &transport);
            let previous_health = crate::client::project_sync_health(state);
            state.transport = transport;
            state.database_sync = database_sync;
            state.database_sync_error = database_sync_error;
            transport_changed || previous_health != crate::client::project_sync_health(state)
        });
    }

    /// Capture configured peers plus a sequence number before the endpoint's
    /// `/status` is fetched; a fetch may only be recorded against the
    /// generation of the endpoint it came from, and finishing records only on
    /// peers still in that generation and never behind a newer observation.
    pub(super) fn begin_runtime_schema_observation(
        &self,
        runtime_did: &str,
        endpoint: Option<&str>,
    ) -> Option<RuntimeSchemaObservation> {
        let expected = self
            .records()
            .into_iter()
            .filter(|record| {
                record.node_did == runtime_did && same_endpoint(record.graphql.as_deref(), endpoint)
            })
            .collect::<Vec<_>>();
        let bound_endpoint = expected.first()?.graphql.clone();
        Some(RuntimeSchemaObservation {
            runtime_did: runtime_did.to_string(),
            endpoint: bound_endpoint,
            sequence: self
                .schema_observation_sequence
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            expected,
        })
    }

    pub(super) async fn finish_runtime_schema_observation(
        &self,
        local: &Arc<defra_node::EmbeddedNode>,
        observation: &RuntimeSchemaObservation,
        status: &Value,
    ) -> anyhow::Result<()> {
        let local = self.local_replicated_schema(local).await?;
        self.record_runtime_schema(observation, local, status)
    }

    /// Compare without recording; enrollment uses this before the offer is
    /// authenticated, so an unverified `/status` cannot set or clear skew.
    pub(super) async fn compare_runtime_schema(
        &self,
        local: &Arc<defra_node::EmbeddedNode>,
        runtime_did: &str,
        status: &Value,
    ) -> anyhow::Result<()> {
        let local = self.local_replicated_schema(local).await?;
        compare_runtime_status(local, runtime_did, status)
    }

    async fn local_replicated_schema(
        &self,
        local: &Arc<defra_node::EmbeddedNode>,
    ) -> anyhow::Result<&ReplicatedSchema> {
        self.local_replicated_schema
            .get_or_try_init(|| read_client_replicated_schema(Arc::clone(local)))
            .await
    }

    fn record_runtime_schema(
        &self,
        observation: &RuntimeSchemaObservation,
        local: &ReplicatedSchema,
        status: &Value,
    ) -> anyhow::Result<()> {
        let observed = compare_runtime_status(local, &observation.runtime_did, status);
        let skew = match &observed {
            Ok(()) => None,
            Err(error) => match error.downcast_ref::<ReplicatedSchemaSkew>() {
                Some(skew) => Some(skew.clone()),
                None => return observed,
            },
        };
        let mut latest = self
            .schema_observation_latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.tx.send_if_modified(|state| {
            let mut changed = false;
            for expected in &observation.expected {
                let current = state.directory.iter().any(|record| {
                    record.peer_id == expected.peer_id
                        && observation_generation_matches(expected, record)
                });
                let newest = latest
                    .get(&expected.peer_id)
                    .is_none_or(|sequence| *sequence <= observation.sequence);
                if !current || !newest {
                    continue;
                }
                latest.insert(expected.peer_id.clone(), observation.sequence);
                changed |= match &skew {
                    None => state.peer_schema_skew.remove(&expected.peer_id).is_some(),
                    Some(skew) => {
                        state
                            .peer_schema_skew
                            .insert(expected.peer_id.clone(), skew.clone())
                            .as_ref()
                            != Some(skew)
                    }
                };
            }
            changed
        });
        observed
    }

    #[cfg(test)]
    pub(super) async fn upsert_for_test(&self, record: PeerRecord) -> anyhow::Result<()> {
        let mut directory = self.directory.write().await;
        directory.upsert(record).await?;
        self.publish_persisted_directory(directory.records().to_vec());
        Ok(())
    }

    /// Replace transport observations only for the exact currently persisted
    /// deployment; this path cannot create or resurrect directory state.
    pub(super) fn replace_peer(&self, expected: &PeerRecord, status: ClientPeerStatus) -> bool {
        self.tx.send_if_modified(|state| {
            if !state.directory.iter().any(|record| record == expected)
                || status.peer_id != expected.peer_id
                || status.addr != expected.addr
            {
                return false;
            }
            let Some(existing) = state.peers.iter_mut().find(|existing| {
                existing.peer_id == status.peer_id && existing.addr == status.addr
            }) else {
                return false;
            };
            if *existing == status {
                return false;
            }
            *existing = status;
            true
        })
    }

    fn publish_persisted_directory(&self, mut records: Vec<PeerRecord>) {
        self.publish_persisted_directory_inner(&mut records, None);
    }

    fn publish_persisted_directory_inner(
        &self,
        records: &mut Vec<PeerRecord>,
        last_error_patch: Option<(String, LastErrorPatch)>,
    ) {
        sort_records(records);
        self.tx.send_if_modified(|state| {
            let previous_directory = state.directory.clone();
            let previous_peers = state.peers.clone();
            state.peers = records
                .iter()
                .map(|record| {
                    let mut status = state
                        .directory
                        .iter()
                        .find(|previous| previous.peer_id == record.peer_id)
                        .zip(
                            state
                                .peers
                                .iter()
                                .find(|status| status.peer_id == record.peer_id),
                        )
                        .filter(|(previous, _)| {
                            observation_generation_matches(previous, record)
                                && !(previous.pairing_ready && !record.pairing_ready)
                        })
                        .map(|(_, status)| {
                            let mut status = status.clone();
                            status.label.clone_from(&record.label);
                            status.node_did.clone_from(&record.node_did);
                            status.addr.clone_from(&record.addr);
                            status
                        })
                        .unwrap_or_else(|| conservative_status(record));
                    if let Some((peer_id, (expected, next))) = last_error_patch.as_ref() {
                        if peer_id == &record.peer_id && status.last_error == *expected {
                            status.last_error.clone_from(next);
                        }
                    }
                    status
                })
                .collect();
            let skew_before = state.peer_schema_skew.len();
            state.peer_schema_skew.retain(|peer_id, _| {
                let previous = previous_directory
                    .iter()
                    .find(|record| &record.peer_id == peer_id);
                let next = records.iter().find(|record| &record.peer_id == peer_id);
                previous
                    .zip(next)
                    .is_some_and(|(previous, next)| observation_generation_matches(previous, next))
            });
            state.directory.clone_from(records);
            state.directory != previous_directory
                || state.peers != previous_peers
                || state.peer_schema_skew.len() != skew_before
        });
    }

    pub(super) fn update_peer(
        &self,
        expected: &PeerRecord,
        update: impl FnOnce(&mut ClientPeerStatus),
    ) -> bool {
        self.tx.send_if_modified(|state| {
            if !state.directory.iter().any(|record| record == expected) {
                return false;
            }
            let Some(peer) = state
                .peers
                .iter_mut()
                .find(|peer| peer.peer_id == expected.peer_id && peer.addr == expected.addr)
            else {
                return false;
            };
            let previous = peer.clone();
            update(peer);
            *peer != previous
        })
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeSchemaObservation {
    runtime_did: String,
    endpoint: Option<String>,
    sequence: u64,
    expected: Vec<PeerRecord>,
}

impl RuntimeSchemaObservation {
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }
}

fn same_endpoint(configured: Option<&str>, target: Option<&str>) -> bool {
    match (configured, target) {
        (None, None) => true,
        (Some(configured), Some(target)) => {
            match (reqwest::Url::parse(configured), reqwest::Url::parse(target)) {
                (Ok(configured), Ok(target)) => configured == target,
                _ => configured == target,
            }
        }
        _ => false,
    }
}

fn compare_runtime_status(
    local: &ReplicatedSchema,
    runtime_did: &str,
    status: &Value,
) -> anyhow::Result<()> {
    let status_did = status.get("node_did").and_then(Value::as_str);
    anyhow::ensure!(
        status_did == Some(runtime_did),
        "runtime status belongs to {}, not {runtime_did}",
        status_did.unwrap_or("an unidentified runtime")
    );
    let remote = match status.get(STATUS_REPLICATED_SCHEMA_FIELD) {
        Some(Value::Null) => {
            anyhow::bail!("runtime {runtime_did} has not published its replicated schema yet")
        }
        Some(schema) => serde_json::from_value::<ReplicatedSchema>(schema.clone()).ok(),
        None => None,
    };
    compare_client_replicated_schema(local, remote.as_ref()).map_err(anyhow::Error::from)
}

fn observation_generation_matches(previous: &PeerRecord, current: &PeerRecord) -> bool {
    previous.peer_id == current.peer_id
        && previous.addr == current.addr
        && previous.node_did == current.node_did
        && previous.source == current.source
        && previous.pairing_network_id == current.pairing_network_id
        && previous.pairing_template == current.pairing_template
        && previous.enrollment_request_id == current.enrollment_request_id
        && previous.enrollment_request_digest == current.enrollment_request_digest
        && previous.enrollment_admin_did == current.enrollment_admin_did
        && previous.enrollment_authorization_sequence == current.enrollment_authorization_sequence
        && previous.enrollment_authorization_expires_at
            == current.enrollment_authorization_expires_at
        && previous.graphql == current.graphql
}

fn sort_records(records: &mut [PeerRecord]) {
    records.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.peer_id.cmp(&right.peer_id))
    });
}

fn sort_statuses(statuses: &mut [ClientPeerStatus]) {
    statuses.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.peer_id.cmp(&right.peer_id))
    });
}

pub(super) fn conservative_status(record: &PeerRecord) -> ClientPeerStatus {
    ClientPeerStatus {
        peer_id: record.peer_id.clone(),
        label: record.label.clone(),
        node_did: record.node_did.clone(),
        addr: record.addr.clone(),
        dial_succeeded: false,
        last_error: None,
        pairing: Vec::new(),
        routes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn generated_activation_traces_drive_the_sync_owner() {
        use super::super::EnrollmentActivationStage;
        use super::*;
        let output = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.EnrollmentActivation:olean",
            "Proofs/Conformance/EnrollmentActivation.lean",
        )
        .unwrap();
        let cases: Vec<Value> = serde_json::from_str(&output).unwrap();
        assert!(!cases.is_empty());
        let (_temp, owner) = ClientSyncStateOwner::for_test(Vec::new(), Vec::new()).await;
        let authority = |value: &Value| -> Option<(String, EnrollmentAuthorizationGeneration)> {
            (!value.is_null()).then(|| {
                (
                    value["request"].as_u64().unwrap().to_string(),
                    EnrollmentAuthorizationGeneration {
                        request_digest: value["digest"].as_u64().unwrap().to_string(),
                        sequence: value["generation"].as_u64().unwrap(),
                        expires_at: value["expiry"].as_u64().unwrap().to_string(),
                    },
                )
            })
        };
        let stage = |value: &Value| -> Option<EnrollmentActivationStage> {
            value.as_str().map(|name| match name {
                "authentication" => EnrollmentActivationStage::Authentication,
                "route" => EnrollmentActivationStage::Route,
                other => panic!("unexpected stage {other}"),
            })
        };
        let map = |entry: Option<(String, EnrollmentAuthorizationGeneration)>| {
            entry.into_iter().collect::<BTreeMap<_, _>>()
        };
        let errors = |generation: &BTreeMap<String, EnrollmentAuthorizationGeneration>,
                      stage: Option<EnrollmentActivationStage>| {
            generation
                .iter()
                .filter_map(|(request, generation)| {
                    stage.map(|stage| {
                        (
                            request.clone(),
                            EnrollmentActivationError {
                                request_digest: generation.request_digest.clone(),
                                authorization_sequence: generation.sequence,
                                authorization_expires_at: generation.expires_at.clone(),
                                stage,
                            },
                        )
                    })
                })
                .collect::<BTreeMap<_, _>>()
        };
        for case in cases {
            let (requested_id, requested_generation) = authority(&case["requested"]).unwrap();
            let requested = BTreeMap::from([(requested_id.clone(), requested_generation)]);
            owner.retain_enrollment_activation_errors(&BTreeMap::new());
            let initial = map(authority(&case["initial"]));
            owner.retain_enrollment_activation_errors(&initial);
            owner.finish_enrollment_activation_errors(
                &initial,
                &errors(&initial, stage(&case["prior"])),
            );
            let selected = map(authority(&case["selected"]));
            owner.retain_enrollment_activation_errors(&selected);
            owner.finish_enrollment_activation_errors(
                &requested,
                &errors(&requested, stage(&case["result"])),
            );
            let snapshot = owner.snapshot();
            let stored = selected
                .keys()
                .next()
                .and_then(|request| snapshot.enrollment_activation_errors.get(request))
                .map(|error| error.stage.as_str());
            assert_eq!(stored, case["stored"].as_str(), "{case}");
            let projected = owner.project_enrollment_activation(
                &requested_id,
                &requested[&requested_id],
                case["approved"].as_bool().unwrap(),
                case["routeObserved"].as_bool().unwrap(),
            );
            assert_eq!(projected, case["projected"].as_str(), "{case}");
        }
    }

    #[test]
    fn enrollment_observation_generation_includes_the_entire_authorization() {
        use super::*;
        let mut record = PeerRecord::new("peer", "127.0.0.1:1", "did:key:node");
        record.enrollment_request_id = Some("request".into());
        record.enrollment_request_digest = Some("digest".into());
        record.enrollment_admin_did = Some("did:key:admin".into());
        record.enrollment_authorization_sequence = Some(1);
        record.enrollment_authorization_expires_at = Some("expiry".into());
        assert!(observation_generation_matches(&record, &record));
        let mut changed = record.clone();
        changed.enrollment_authorization_expires_at = Some("new-expiry".into());
        assert!(!observation_generation_matches(&record, &changed));
        changed = record.clone();
        changed.enrollment_request_id = Some("new-request".into());
        assert!(!observation_generation_matches(&record, &changed));
        changed = record.clone();
        changed.enrollment_admin_did = Some("did:key:other".into());
        assert!(!observation_generation_matches(&record, &changed));
    }

    #[tokio::test]
    async fn enrollment_activation_errors_notify_without_peers_and_expire_with_authority() {
        use super::*;
        let (_temp, owner) = ClientSyncStateOwner::for_test(Vec::new(), Vec::new()).await;
        let mut updates = owner.subscribe();
        let error = EnrollmentActivationError {
            request_digest: "digest-one".into(),
            authorization_sequence: 1,
            authorization_expires_at: "2030-01-01T00:00:00Z".into(),
            stage: super::super::EnrollmentActivationStage::Authentication,
        };
        let current = BTreeMap::from([(
            "request".into(),
            EnrollmentAuthorizationGeneration {
                request_digest: error.request_digest.clone(),
                sequence: 1,
                expires_at: error.authorization_expires_at.clone(),
            },
        )]);
        owner.retain_enrollment_activation_errors(&current);
        owner.finish_enrollment_activation_errors(
            &current,
            &BTreeMap::from([("request".into(), error.clone())]),
        );
        assert!(updates.has_changed().unwrap());
        assert!(updates.borrow_and_update().directory.is_empty());
        owner.retain_enrollment_activation_errors(&current);
        assert!(!updates.has_changed().unwrap());
        let mut newer = current.clone();
        newer.get_mut("request").unwrap().sequence = 2;
        owner.retain_enrollment_activation_errors(&newer);
        assert!(updates.has_changed().unwrap());
        assert!(updates
            .borrow_and_update()
            .enrollment_activation_errors
            .is_empty());
        owner.finish_enrollment_activation_errors(
            &current,
            &BTreeMap::from([("request".into(), error)]),
        );
        assert!(!updates.has_changed().unwrap());
        owner.retain_enrollment_activation_errors(&BTreeMap::new());
        assert!(!updates.has_changed().unwrap());
        assert!(updates.borrow().enrollment_activation_errors.is_empty());
    }

    #[test]
    fn configured_peer_mutation_callers_cannot_bypass_the_owner() {
        fn without_trailing_test_module(source: &str) -> &str {
            [
                "\n#[cfg(test)]\nmod tests {",
                "\n#[cfg(test)]\nmod pairing_reconcile_tests {",
            ]
            .into_iter()
            .filter_map(|marker| source.find(marker))
            .min()
            .map_or(source, |index| &source[..index])
        }

        for (name, source) in [
            ("core", include_str!("../core.rs")),
            ("writes", include_str!("writes.rs")),
            (
                "writes/config_delete",
                include_str!("writes/config_delete.rs"),
            ),
            ("writes/config_save", include_str!("writes/config_save.rs")),
            ("writes/hydration", include_str!("writes/hydration.rs")),
            (
                "writes/hydration/documents",
                include_str!("writes/hydration/documents.rs"),
            ),
            ("writes/manual_runs", include_str!("writes/manual_runs.rs")),
            ("writes/operator", include_str!("writes/operator.rs")),
            (
                "writes/pending_queue",
                include_str!("writes/pending_queue.rs"),
            ),
            (
                "writes/pending_submissions",
                include_str!("writes/pending_submissions.rs"),
            ),
            (
                "writes/request_patch",
                include_str!("writes/request_patch.rs"),
            ),
            ("writes/requests", include_str!("writes/requests.rs")),
            ("writes/sessions", include_str!("writes/sessions.rs")),
            ("route_manager", include_str!("route_manager.rs")),
            ("supervisor", include_str!("supervisor.rs")),
            ("observer", include_str!("../observe.rs")),
        ] {
            let production = without_trailing_test_module(source);
            assert!(
                !production.contains(".peer_directory"),
                "{name} retained direct ClientCore peer-directory access"
            );
            assert!(
                !production.contains("PeerDirectory::"),
                "{name} retained a direct PeerDirectory mutation seam"
            );
        }
    }
}
