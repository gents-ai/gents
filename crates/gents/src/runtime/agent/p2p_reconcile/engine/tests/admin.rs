#[derive(Default)]
struct MockAdmin {
    collections: Mutex<BTreeSet<String>>,
    replicators: Mutex<BTreeMap<String, RemoteReplicator>>,
    emitted: Mutex<Vec<DiffOp>>,
    connects: Mutex<Vec<Vec<String>>>,
    recorded_filters: Mutex<Vec<(Vec<String>, PairingFilters)>>,
    deleted_replicator_collections: Mutex<Vec<Vec<String>>>,
    delete_replicator_not_found: Mutex<bool>,
    active: Mutex<Vec<String>>,
    fail_active_peers: bool,
    active_peers_started: Option<Arc<tokio::sync::Notify>>,
    active_peers_release: Option<Arc<tokio::sync::Notify>>,
    fail_connect: bool,
    blocked_connect_address: Option<String>,
    blocked_connect_started: Option<Arc<tokio::sync::Notify>>,
    blocked_connect_release: Option<Arc<tokio::sync::Notify>>,
    replicator_installed: Option<Arc<tokio::sync::Notify>>,
    fail_add_replicator_attempts: Mutex<usize>,
    /// Peer ids whose replicator acknowledged every routed document; the
    /// queried `(peer_id, collection)` pairs are recorded.
    acknowledged_peers: BTreeSet<String>,
    acknowledgement_queries: Mutex<Vec<(String, String)>>,
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
        if std::mem::take(&mut *self.delete_replicator_not_found.lock().unwrap()) {
            return Err(RemoteP2pAdminError::RemoteNotFound(id.to_string()));
        }
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

    async fn replication_acknowledged(
        &self,
        peer_id: &str,
        collection: &str,
        _filter: Option<&crate::runtime::agent::p2p_reconcile::FilterPredicate>,
    ) -> RemoteP2pAdminResult<Option<bool>> {
        self.acknowledgement_queries
            .lock()
            .unwrap()
            .push((peer_id.to_string(), collection.to_string()));
        Ok(Some(self.acknowledged_peers.contains(peer_id)))
    }
}
