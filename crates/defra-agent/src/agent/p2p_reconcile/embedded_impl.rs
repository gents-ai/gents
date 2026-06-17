//! Embedded-node implementation of the runtime pairing admin seam.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use defra_p2p_adapter::{
    P2PError, P2PResult, P2pDocumentRequest, ReplicationFilter, ReplicationFilters,
};
use tokio::time::timeout;

use crate::defra_node::EmbeddedNode;

use super::templates::PairingFilters;
use super::{RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteReplicator};

const DEFAULT_EMBEDDED_ADMIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Runtime-local admin adapter over an embedded DefraDB node's P2P operations.
#[derive(Clone)]
pub struct EmbeddedRemoteP2pAdmin {
    node: Arc<EmbeddedNode>,
    timeout: Duration,
}

impl EmbeddedRemoteP2pAdmin {
    pub fn new(node: Arc<EmbeddedNode>) -> Self {
        Self {
            node,
            timeout: DEFAULT_EMBEDDED_ADMIN_TIMEOUT,
        }
    }

    pub fn with_timeout(node: Arc<EmbeddedNode>, timeout: Duration) -> Self {
        Self { node, timeout }
    }

    fn p2p(&self) -> RemoteP2pAdminResult<Arc<dyn defra_p2p_adapter::P2POperations>> {
        self.node.p2p_arc().ok_or_else(|| {
            RemoteP2pAdminError::LocalError("embedded node has no P2P transport".into())
        })
    }

    async fn run<T, F>(&self, operation: &'static str, future: F) -> RemoteP2pAdminResult<T>
    where
        F: Future<Output = P2PResult<T>>,
    {
        match timeout(self.timeout, future).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(map_p2p_error(operation, error)),
            Err(_) => Err(RemoteP2pAdminError::RpcTimeout),
        }
    }
}

#[async_trait]
impl RemoteP2pAdmin for EmbeddedRemoteP2pAdmin {
    async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let p2p = self.p2p()?;
        let peer_id = self.run("local_peer_id", p2p.local_peer_id()).await?;
        let addresses = self.run("listen_addresses", p2p.listen_addresses()).await?;
        Ok(addresses
            .into_iter()
            .map(|addr| {
                if addr.starts_with('/') {
                    format!("{addr}/p2p/{peer_id}")
                } else {
                    addr
                }
            })
            .collect())
    }

    async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let p2p = self.p2p()?;
        self.run("connected_peers", p2p.connected_peers()).await
    }

    async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        for addr in addresses {
            self.run("connect_peer", p2p.connect_peer(addr)).await?;
        }
        Ok(())
    }

    async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
        let p2p = self.p2p()?;
        let replicators = self.run("get_replicators", p2p.get_replicators()).await?;
        Ok(replicators
            .into_iter()
            .map(|r| RemoteReplicator {
                id: r.id,
                collections: r.collections,
                address: r.address,
            })
            .collect())
    }

    async fn add_replicator(
        &self,
        addresses: &[String],
        collections: &[String],
        filters: &PairingFilters,
    ) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        let addr = addresses.first().map(String::as_str);
        let defra_filters = to_defra_filters(filters);
        self.run(
            "add_replicator",
            p2p.add_replicator(collections.to_vec(), addr, defra_filters, Vec::new(), None),
        )
        .await
    }

    async fn delete_replicator(
        &self,
        id: &str,
        collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        let addr = (!id.trim().is_empty()).then_some(id);
        self.run(
            "remove_replicator",
            p2p.remove_replicator(collections.to_vec(), addr),
        )
        .await
    }

    async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let p2p = self.p2p()?;
        self.run("get_collections", p2p.get_collections()).await
    }

    async fn resolve_collection_id(&self, name: &str) -> RemoteP2pAdminResult<Option<String>> {
        // The P2P subscription set (`get_collections`) is keyed by collection id,
        // but desired state carries collection names; resolve via the local schema
        // catalog so the reconcile diff compares both sides in id-space.
        match self.node.get_collection(name) {
            Ok(Some(def)) => Ok(Some(def.collection_id)),
            Ok(None) => Ok(None),
            Err(error) => Err(RemoteP2pAdminError::LocalError(format!(
                "resolve_collection_id({name}): {error}"
            ))),
        }
    }

    async fn resolve_collection_name(&self, id: &str) -> RemoteP2pAdminResult<Option<String>> {
        // Walk the local catalog to invert id → name. The subscribe/unsubscribe
        // adapter calls take names, but `PairingApplied` records ids, so teardown
        // of a no-longer-desired collection must recover the name here.
        let names = self.node.list_collections().map_err(|error| {
            RemoteP2pAdminError::LocalError(format!("list_collections for id {id}: {error}"))
        })?;
        for name in names {
            match self.node.get_collection(&name) {
                Ok(Some(def)) if def.collection_id == id => return Ok(Some(def.name)),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        collection_name = %name,
                        %error,
                        "resolve_collection_name failed to fetch a collection definition"
                    );
                }
            }
        }
        Ok(None)
    }

    async fn add_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        self.run("add_collections", p2p.add_collections(collections.to_vec()))
            .await
    }

    async fn delete_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        self.run(
            "remove_collections",
            p2p.remove_collections(collections.to_vec()),
        )
        .await
    }

    async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let p2p = self.p2p()?;
        let documents = self.run("get_documents", p2p.get_documents()).await?;
        Ok(documents.into_iter().map(|d| d.doc_id).collect())
    }

    async fn add_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        let docs = document_requests(doc_ids);
        self.run("add_documents", p2p.add_documents(docs)).await
    }

    async fn delete_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        let docs = document_requests(doc_ids);
        self.run("remove_documents", p2p.remove_documents(docs))
            .await
    }

    async fn sync_documents(
        &self,
        collection_name: &str,
        doc_ids: &[String],
        timeout_override: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        let future = p2p.sync_documents(collection_name, doc_ids.to_vec());
        match timeout(timeout_override.unwrap_or(self.timeout), future).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(map_p2p_error("sync_documents", error)),
            Err(_) => Err(RemoteP2pAdminError::RpcTimeout),
        }
    }

    async fn sync_collection_versions(
        &self,
        version_ids: &[String],
        timeout_override: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        let future = p2p.sync_collection_versions(version_ids.to_vec());
        match timeout(timeout_override.unwrap_or(self.timeout), future).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(map_p2p_error("sync_collection_versions", error)),
            Err(_) => Err(RemoteP2pAdminError::RpcTimeout),
        }
    }

    async fn sync_branchable_collection(
        &self,
        collection_id: &str,
        timeout_override: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        let future = p2p.sync_branchable_collection(collection_id);
        match timeout(timeout_override.unwrap_or(self.timeout), future).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(map_p2p_error("sync_branchable_collection", error)),
            Err(_) => Err(RemoteP2pAdminError::RpcTimeout),
        }
    }
}

/// Translate our `PairingFilters` seam type into defradb's `ReplicationFilters`
/// (per-collection equality predicate), passed straight to the filtered
/// `add_replicator` on the pinned #1033 rev. Filters are translated 1:1 and
/// validated (fail-closed) by defradb — there is no unfiltered fallback. Our
/// predicate values are agent DIDs (strings), so they map to JSON strings.
fn to_defra_filters(filters: &PairingFilters) -> ReplicationFilters {
    filters
        .iter()
        .map(|(collection, predicate)| {
            (
                collection.clone(),
                ReplicationFilter {
                    field: predicate.field.clone(),
                    value: serde_json::Value::String(predicate.value.clone()),
                    // defra-agent's pairing scope is a simple field==value filter;
                    // the rich `Conditions` predicate (added upstream) is unused.
                    conditions: None,
                },
            )
        })
        .collect()
}

fn document_requests(doc_ids: &[String]) -> Vec<P2pDocumentRequest> {
    doc_ids
        .iter()
        .cloned()
        .map(|doc_id| P2pDocumentRequest {
            collection: String::new(),
            doc_id,
        })
        .collect()
}

fn map_p2p_error(operation: &'static str, error: P2PError) -> RemoteP2pAdminError {
    match error {
        P2PError::NotFound(message) => RemoteP2pAdminError::RemoteNotFound(message),
        P2PError::Transport(message) | P2PError::Internal(message) => {
            RemoteP2pAdminError::RpcError(format!("{operation}: {message}"))
        }
        P2PError::InvalidInput(message) | P2PError::Unsupported(message) => {
            RemoteP2pAdminError::LocalError(format!("{operation}: {message}"))
        }
        _ => RemoteP2pAdminError::RpcError(format!("{operation}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use p2p::iroh::{IrohDiscoveryConfig, IrohRelayModeConfig};

    use super::*;
    use crate::defra_node::P2PConfig;

    const TEST_SCHEMA: &str = r#"
        type P2pReconcileThing {
            name: String
        }
    "#;

    struct TestNode {
        node: Arc<EmbeddedNode>,
        _tempdir: tempfile::TempDir,
    }

    async fn test_node() -> TestNode {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path())
                .with_p2p(P2PConfig {
                    port: 0,
                    bind_addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                    relay_mode: IrohRelayModeConfig::Disabled,
                    discovery: IrohDiscoveryConfig::Disabled,
                    secret_key_path: None,
                    load_persisted_collections: false,
                    max_concurrent_dag_fetches: p2p::sync::DEFAULT_MAX_CONCURRENT_DAG_FETCHES,
                    max_concurrent_push_tasks: p2p::sync::DEFAULT_MAX_CONCURRENT_PUSH_TASKS,
                    rate_limit_burst: p2p::sync::DEFAULT_RATE_LIMIT_BURST,
                    rate_limit_rate: p2p::sync::DEFAULT_RATE_LIMIT_RATE,
                    max_doc_sync_request_doc_ids: p2p::sync::DEFAULT_MAX_DOC_SYNC_REQUEST_DOC_IDS,
                })
                .build()
                .await
                .expect("embedded p2p node"),
        );
        node.add_schema(TEST_SCHEMA).await.expect("test schema");
        TestNode {
            node,
            _tempdir: tempdir,
        }
    }

    async fn wait_for_peer_info(admin: &EmbeddedRemoteP2pAdmin) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let addresses = admin.peer_info().await.expect("peer info");
            if !addresses.is_empty() {
                return addresses;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("node never exposed a P2P address");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn collection_id(node: &EmbeddedNode) -> String {
        node.get_collection("P2pReconcileThing")
            .expect("collection lookup")
            .expect("collection")
            .collection_id
    }

    #[tokio::test]
    async fn embedded_collections_round_trip() {
        let test = test_node().await;
        let node = Arc::clone(&test.node);
        let admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&node));
        let collection_name = "P2pReconcileThing".to_string();
        let expected_collection_id = collection_id(&node);

        admin
            .add_p2p_collections(std::slice::from_ref(&collection_name))
            .await
            .expect("add collection");

        let collections = admin
            .list_p2p_collections()
            .await
            .expect("list collections");
        assert!(collections.contains(&expected_collection_id));

        node.shutdown().await;
    }

    #[tokio::test]
    async fn embedded_replicators_round_trip() {
        let local_test = test_node().await;
        let remote_test = test_node().await;
        let local = Arc::clone(&local_test.node);
        let remote = Arc::clone(&remote_test.node);
        let local_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&local));
        let remote_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&remote));
        let remote_addresses = wait_for_peer_info(&remote_admin).await;
        let collection_name = "P2pReconcileThing".to_string();
        let expected_collection_id = collection_id(&local);

        local_admin
            .connect(&remote_addresses)
            .await
            .expect("connect remote");
        local_admin
            .add_replicator(
                &remote_addresses,
                std::slice::from_ref(&collection_name),
                &PairingFilters::default(),
            )
            .await
            .expect("add replicator");

        let replicators = local_admin
            .list_replicators()
            .await
            .expect("list replicators");
        assert!(
            replicators
                .iter()
                .any(|r| r.collections.contains(&expected_collection_id)),
            "replicators={replicators:?}"
        );

        local.shutdown().await;
        remote.shutdown().await;
    }
}
