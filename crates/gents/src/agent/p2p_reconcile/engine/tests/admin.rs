#[derive(Default)]
struct MockAdmin {
    collections: Mutex<BTreeSet<String>>,
    replicators: Mutex<BTreeMap<String, RemoteReplicator>>,
    emitted: Mutex<Vec<DiffOp>>,
    connects: Mutex<Vec<Vec<String>>>,
    recorded_filters: Mutex<Vec<(Vec<String>, PairingFilters)>>,
    deleted_replicator_collections: Mutex<Vec<Vec<String>>>,
    /// Entries returned by `active_peers` (peer ids or dial addresses, like
    /// the real adapters).
    active: Mutex<Vec<String>>,
    /// `active_peers` fails when set, exercising the degraded-read path.
    fail_active_peers: bool,
    /// Barrier proving supervisor cancellation can drop an in-flight admin
    /// wait instead of waiting for its per-RPC timeout.
    active_peers_started: Option<Arc<tokio::sync::Notify>>,
    active_peers_release: Option<Arc<tokio::sync::Notify>>,
    /// When set, `connect` fails after recording the call, modeling the
    /// redial-timeout that motivated the active-peer gate.
    fail_connect: bool,
    /// Address-specific barrier proving one stale peer's dial does not
    /// head-of-line block a ready peer's sweep.
    blocked_connect_address: Option<String>,
    blocked_connect_started: Option<Arc<tokio::sync::Notify>>,
    blocked_connect_release: Option<Arc<tokio::sync::Notify>>,
    replicator_installed: Option<Arc<tokio::sync::Notify>>,
    /// Upcoming replicator installs to fail, modeling the torn reconnect-replay
    /// window (delete succeeds, reinstall transiently fails).
    fail_add_replicator_attempts: Mutex<usize>,
}

#[async_trait]
impl RemoteP2pAdmin for MockAdmin {
    async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
        if let Some(started) = &self.active_peers_started {
            started.notify_one();
        }
        if let Some(release) = &self.active_peers_release {
            release.notified().await;
        }
        if self.fail_active_peers {
            return Err(RemoteP2pAdminError::RpcError("active_peers down".into()));
        }
        Ok(self.active.lock().unwrap().clone())
    }

    async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()> {
        self.connects.lock().unwrap().push(addresses.to_vec());
        if self
            .blocked_connect_address
            .as_ref()
            .is_some_and(|blocked| addresses.iter().any(|address| address == blocked))
        {
            if let Some(started) = &self.blocked_connect_started {
                started.notify_one();
            }
            if let Some(release) = &self.blocked_connect_release {
                release.notified().await;
            }
        }
        if self.fail_connect {
            return Err(RemoteP2pAdminError::RpcTimeout);
        }
        Ok(())
    }

    async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
        Ok(self.replicators.lock().unwrap().values().cloned().collect())
    }

    async fn add_replicator(
        &self,
        addresses: &[String],
        collections: &[String],
        filters: &PairingFilters,
    ) -> RemoteP2pAdminResult<()> {
        self.recorded_filters
            .lock()
            .unwrap()
            .push((addresses.to_vec(), filters.clone()));
        let mut remaining_failures = self.fail_add_replicator_attempts.lock().unwrap();
        if *remaining_failures > 0 {
            *remaining_failures -= 1;
            return Err(RemoteP2pAdminError::RpcError(
                "transient add_replicator failure".into(),
            ));
        }
        drop(remaining_failures);
        let resolved_filters = mock_live_filters(filters);
        for address in addresses {
            // Transport records the carried collection set in *id* space like
            // the real adapter; `read_actual` reverse-resolves it to names.
            self.replicators.lock().unwrap().insert(
                address.clone(),
                RemoteReplicator {
                    id: Some(format!("id-{address}")),
                    collections: collections.iter().map(|c| mock_collection_id(c)).collect(),
                    address: Some(address.clone()),
                    filters: Some(resolved_filters.clone()),
                },
            );
            self.emitted
                .lock()
                .unwrap()
                .push(DiffOp::InstallReplicator(address.clone()));
            if let Some(installed) = &self.replicator_installed {
                installed.notify_one();
            }
        }
        Ok(())
    }

    async fn delete_replicator(
        &self,
        id: &str,
        collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        self.deleted_replicator_collections
            .lock()
            .unwrap()
            .push(collections.to_vec());
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

    // The subscription set is stored in *id*-space, mirroring the real adapter:
    // `add_p2p_collections` receives names and persists the resolved id;
    // `list_p2p_collections` returns those ids.
    async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
        Ok(self.collections.lock().unwrap().iter().cloned().collect())
    }

    async fn resolve_collection_id(&self, name: &str) -> RemoteP2pAdminResult<Option<String>> {
        Ok(Some(mock_collection_id(name)))
    }

    async fn resolve_collection_name(&self, id: &str) -> RemoteP2pAdminResult<Option<String>> {
        Ok(id
            .strip_prefix("col_")
            .and_then(|rest| rest.strip_suffix("_id"))
            .map(str::to_string))
    }

    async fn add_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
        for collection in collections {
            // Stored token is the resolved id, as the adapter subscribes by id.
            self.collections
                .lock()
                .unwrap()
                .insert(mock_collection_id(collection));
            self.emitted
                .lock()
                .unwrap()
                .push(DiffOp::InstallCollection(collection.clone()));
        }
        Ok(())
    }

    async fn delete_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
        for collection in collections {
            self.collections
                .lock()
                .unwrap()
                .remove(&mock_collection_id(collection));
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
