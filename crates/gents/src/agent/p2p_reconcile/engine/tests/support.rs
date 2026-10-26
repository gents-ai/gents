fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

const TEST_TRANSPORT_PEER_ID: &str =
    "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
const TEST_TRANSPORT_ADDRESS_A: &str =
    "127.0.0.1:56091/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
const TEST_TRANSPORT_ADDRESS_B: &str =
    "127.0.0.1:56092/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";

fn one_filter(collection: &str, field: &str, value: &str) -> PairingFilters {
    let mut filters = PairingFilters::new();
    filters.insert(
        collection.to_string(),
        crate::agent::p2p_reconcile::equality_filter(field, value),
    );
    filters
}

fn merge_desired(
    base: Option<PairingDesired>,
    data_plane: Option<PairingDesired>,
) -> Option<PairingDesired> {
    merge_layered_desired("did:key:local", "did:key:peer", base, data_plane)
}

/// The mock mirrors the real adapter's name→collection-id distinctness; a
/// name-echo (id == name) would hide the id-space mismatch the engine must
/// reconcile. The prefix guarantees id != name.
fn mock_collection_id(name: &str) -> String {
    format!("col_{name}_id")
}

fn mock_live_filters(filters: &PairingFilters) -> defra_p2p_adapter::ReplicationFilters {
    to_replication_filters(filters)
        .expect("mock filters are representable")
        .into_iter()
        .map(|(collection, filter)| (mock_collection_id(&collection), filter))
        .collect()
}

struct MockStore {
    desired: Mutex<Result<Option<PairingDesired>, String>>,
    applied: Mutex<PairingApplied>,
    saved: Mutex<Vec<PairingApplied>>,
    deleted: Mutex<usize>,
    list_peer_ids_failures: Mutex<usize>,
    list_peer_ids_calls: Mutex<usize>,
    list_peer_ids_retry_started: Option<Arc<tokio::sync::Notify>>,
    list_peer_ids_retry_release: Option<Arc<tokio::sync::Notify>>,
    persist_applied_completed: Option<Arc<tokio::sync::Notify>>,
}

impl Default for MockStore {
    fn default() -> Self {
        Self {
            desired: Mutex::new(Ok(None)),
            applied: Mutex::new(PairingApplied::default()),
            saved: Mutex::new(Vec::new()),
            deleted: Mutex::new(0),
            list_peer_ids_failures: Mutex::new(0),
            list_peer_ids_calls: Mutex::new(0),
            list_peer_ids_retry_started: None,
            list_peer_ids_retry_release: None,
            persist_applied_completed: None,
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

    async fn load_applied(&self, _peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(LoadedPairingApplied {
            state: self.applied.lock().unwrap().clone(),
            ..Default::default()
        })
    }

    async fn persist_applied(&self, _peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        *self.applied.lock().unwrap() = applied.state.clone();
        if applied.state.is_empty() {
            *self.deleted.lock().unwrap() += 1;
        } else {
            self.saved.lock().unwrap().push(applied.state.clone());
            if let Some(completed) = &self.persist_applied_completed {
                completed.notify_one();
            }
        }
        Ok(())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        *self.list_peer_ids_calls.lock().unwrap() += 1;
        let should_fail = {
            let mut failures = self.list_peer_ids_failures.lock().unwrap();
            if *failures == 0 {
                false
            } else {
                *failures -= 1;
                true
            }
        };
        if should_fail {
            anyhow::bail!("transient list_peer_ids failure");
        }
        if let Some(started) = &self.list_peer_ids_retry_started {
            started.notify_one();
        }
        if let Some(release) = &self.list_peer_ids_retry_release {
            release.notified().await;
        }
        Ok(set(&["peer-a"]))
    }
}

struct MultiPeerStore {
    desired: BTreeMap<String, PairingDesired>,
    applied: Mutex<BTreeMap<String, PairingApplied>>,
}

struct EnrollmentFenceStore {
    desired: PairingDesired,
    generation: EnrollmentRouteGeneration,
    applied: Mutex<PairingApplied>,
    authority_checks: Mutex<std::collections::VecDeque<Result<bool, String>>>,
}

#[async_trait]
impl PairingStateStore for EnrollmentFenceStore {
    async fn load_desired(&self, _peer_id: &str) -> Result<Option<PairingDesired>> {
        Ok(Some(self.desired.clone()))
    }

    async fn load_desired_with_authority(&self, _peer_id: &str) -> Result<LoadedPairingDesired> {
        Ok(LoadedPairingDesired {
            state: Some(self.desired.clone()),
            enrollment_generation: Some(self.generation.clone()),
        })
    }

    async fn enrollment_generation_is_current(
        &self,
        _generation: &EnrollmentRouteGeneration,
    ) -> Result<bool> {
        self.authority_checks
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(false))
            .map_err(|error| TransportFailure(error.to_string()))
    }

    async fn load_applied(&self, _peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(LoadedPairingApplied {
            state: self.applied.lock().unwrap().clone(),
            ..Default::default()
        })
    }

    async fn persist_applied(&self, _peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        *self.applied.lock().unwrap() = applied.state.clone();
        Ok(())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        Ok(set(&["peer-a"]))
    }
}

#[async_trait]
impl PairingStateStore for MultiPeerStore {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>> {
        Ok(self.desired.get(peer_id).cloned())
    }

    async fn load_applied(&self, peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(LoadedPairingApplied {
            state: self
                .applied
                .lock()
                .unwrap()
                .get(peer_id)
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        })
    }

    async fn persist_applied(&self, peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        let mut states = self.applied.lock().unwrap();
        if applied.state.is_empty() {
            states.remove(peer_id);
        } else {
            states.insert(peer_id.to_string(), applied.state.clone());
        }
        Ok(())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        Ok(self.desired.keys().cloned().collect())
    }
}

fn enrolled_live_route_store(
    authority_checks: impl IntoIterator<Item = Result<bool, String>>,
) -> EnrollmentFenceStore {
    EnrollmentFenceStore {
        desired: PairingDesired {
            replicator_addresses: set(&["addr1"]),
            replicator_collections: set(&["AgentRequest"]),
            ..Default::default()
        },
        generation: EnrollmentRouteGeneration {
            member_did: "did:key:member".into(),
            member_peer: "peer-a".into(),
            member_ticket: "addr1".into(),
            request_digest: "digest-1".into(),
            authorization_sequence: 1,
            authorization_expires_at: "2099-09-29T00:00:00Z".into(),
        },
        applied: Mutex::new(PairingApplied {
            replicator_addresses: set(&["addr1"]),
            ..Default::default()
        }),
        authority_checks: Mutex::new(authority_checks.into_iter().collect()),
    }
}

fn admin_with_live_enrolled_route() -> MockAdmin {
    let admin = MockAdmin::default();
    admin.active.lock().unwrap().push("peer-a".into());
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("route-1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(Default::default()),
        },
    );
    admin
}
