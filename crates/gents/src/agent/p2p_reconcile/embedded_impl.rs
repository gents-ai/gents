use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use defra_p2p_adapter::{P2PError, P2PResult, P2pDocumentRequest};
use tokio::time::timeout;

use crate::defra_node::EmbeddedNode;

use super::templates::{
    filter_conditions, live_filter_string_eq, to_replication_filters, FilterPredicate,
    PairingFilters,
};
use super::{
    resolve_embedded_collection_id, resolve_embedded_collection_name, RemoteP2pAdmin,
    RemoteP2pAdminError, RemoteP2pAdminResult, RemoteReplicator,
};

const DEFAULT_EMBEDDED_ADMIN_TIMEOUT: Duration = Duration::from_secs(10);

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
        crate::identity::node_p2p(&self.node).ok_or_else(|| {
            RemoteP2pAdminError::LocalError("embedded node has no P2P transport".into())
        })
    }

    async fn run<T, F>(&self, operation: &'static str, future: F) -> RemoteP2pAdminResult<T>
    where
        F: Future<Output = P2PResult<T>>,
    {
        self.run_within(operation, self.timeout, future).await
    }

    async fn run_within<T, F>(
        &self,
        operation: &'static str,
        within: Duration,
        future: F,
    ) -> RemoteP2pAdminResult<T>
    where
        F: Future<Output = P2PResult<T>>,
    {
        match timeout(within, future).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(map_p2p_error(operation, error)),
            Err(_) => Err(RemoteP2pAdminError::RpcTimeout),
        }
    }

    /// For each of `doc_ids`, whether replicator `peer_id` acknowledged the
    /// heads this node currently holds for it.
    pub async fn documents_acknowledged(
        &self,
        peer_id: &str,
        collection: &str,
        doc_ids: &[String],
    ) -> RemoteP2pAdminResult<Vec<bool>> {
        let p2p = self.p2p()?;
        self.run(
            "replication_acknowledged",
            p2p.replication_acknowledged(peer_id, collection, doc_ids),
        )
        .await
    }

    /// Exact document heads, without requiring unrelated branchable collection heads.
    pub async fn document_heads_acknowledged(
        &self,
        peer_id: &str,
        collection: &str,
        doc_ids: &[String],
    ) -> RemoteP2pAdminResult<Vec<bool>> {
        let p2p = self.p2p()?;
        self.run(
            "document_heads_acknowledged",
            p2p.document_heads_acknowledged(peer_id, collection, doc_ids),
        )
        .await
    }

    /// The `(doc_id, requester_did)` documents of `collection` whose current
    /// heads every replicator carrying them acknowledged: those scoped to the
    /// requester and those carrying the whole collection. A document no
    /// replicator carries acknowledges nothing.
    pub async fn requester_acknowledged(
        &self,
        collection: &str,
        documents: &[(String, String)],
    ) -> RemoteP2pAdminResult<BTreeSet<String>> {
        let collection_id = self
            .resolve_collection_id(collection)
            .await?
            .ok_or_else(|| {
                RemoteP2pAdminError::LocalError(format!(
                    "collection {collection} is not registered"
                ))
            })?;
        let mut peers_by_requester: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut collection_peers = BTreeSet::new();
        for replicator in self.list_replicators().await? {
            let (Some(peer_id), Some(filters)) = (replicator.id, replicator.filters) else {
                continue;
            };
            match filters.get(&collection_id) {
                None if replicator.collections.contains(&collection_id) => {
                    collection_peers.insert(peer_id);
                }
                None => {}
                Some(filter) => {
                    if let Some(requester) = live_filter_string_eq(filter, "requester_did") {
                        peers_by_requester
                            .entry(requester.to_string())
                            .or_default()
                            .insert(peer_id);
                    }
                }
            }
        }

        let mut doc_ids_by_peer: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        let mut routed = BTreeSet::new();
        for (doc_id, requester) in documents {
            for peer_id in peers_by_requester
                .get(requester)
                .into_iter()
                .flatten()
                .chain(&collection_peers)
            {
                doc_ids_by_peer
                    .entry(peer_id)
                    .or_default()
                    .push(doc_id.clone());
                routed.insert(doc_id.clone());
            }
        }
        for (peer_id, doc_ids) in doc_ids_by_peer {
            let acknowledged = self
                .documents_acknowledged(peer_id, collection, &doc_ids)
                .await?;
            for (doc_id, acknowledged) in doc_ids.iter().zip(acknowledged) {
                if !acknowledged {
                    routed.remove(doc_id);
                }
            }
        }
        Ok(routed)
    }

    async fn selected_document_ids(
        &self,
        collection: &str,
        filter: Option<&FilterPredicate>,
    ) -> RemoteP2pAdminResult<Vec<String>> {
        crate::graphql::validate_collection_identifier(collection)
            .map_err(|error| RemoteP2pAdminError::LocalError(error.to_string()))?;
        let filter = match filter {
            None => serde_json::Value::Null,
            Some(filter) => {
                serde_json::Value::Object(filter_conditions(filter).ok_or_else(|| {
                    RemoteP2pAdminError::LocalError(format!(
                        "{collection} replicator filter has no document predicate"
                    ))
                })?)
            }
        };
        let query = format!(
            "query($filter: {collection}FilterArg) {{ {collection}(filter: $filter) {{ _docID }} }}"
        );
        let variables = serde_json::json!({ "filter": filter });
        let response = crate::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            None,
            "p2p_reconcile.replicated_document_ids",
            move |txn| {
                let query = query.clone();
                let variables = variables.clone();
                Box::pin(async move { txn.execute_with_variables(&query, &variables).await })
            },
        )
        .await
        .map_err(|error| {
            RemoteP2pAdminError::LocalError(
                gents_protocol::error_chain::ErrorChain(&error).to_string(),
            )
        })?;
        Ok(response
            .pointer(&format!("/data/{collection}"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("_docID")?.as_str().map(str::to_string))
            .collect())
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
                filters: Some(r.filters),
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
        let defra_filters =
            to_replication_filters(filters).map_err(RemoteP2pAdminError::LocalError)?;
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
        resolve_embedded_collection_id(&self.node, name)
    }

    async fn resolve_collection_name(&self, id: &str) -> RemoteP2pAdminResult<Option<String>> {
        resolve_embedded_collection_name(&self.node, id)
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
        let within = timeout_override.unwrap_or(self.timeout);
        self.run_within(
            "sync_documents",
            within,
            p2p.sync_documents(collection_name, doc_ids.to_vec(), Some(within)),
        )
        .await
    }

    async fn sync_collection_versions(
        &self,
        version_ids: &[String],
        timeout_override: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        self.run_within(
            "sync_collection_versions",
            timeout_override.unwrap_or(self.timeout),
            p2p.sync_collection_versions(version_ids.to_vec()),
        )
        .await
    }

    async fn sync_branchable_collection(
        &self,
        collection_id: &str,
        timeout_override: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let p2p = self.p2p()?;
        self.run_within(
            "sync_branchable_collection",
            timeout_override.unwrap_or(self.timeout),
            p2p.sync_branchable_collection(collection_id),
        )
        .await
    }

    async fn replication_acknowledged(
        &self,
        peer_id: &str,
        collection: &str,
        filter: Option<&FilterPredicate>,
    ) -> RemoteP2pAdminResult<Option<bool>> {
        let doc_ids = self.selected_document_ids(collection, filter).await?;
        if doc_ids.is_empty() {
            return Ok(Some(true));
        }
        let acknowledged = self
            .documents_acknowledged(peer_id, collection, &doc_ids)
            .await?;
        Ok(Some(
            acknowledged.into_iter().all(|acknowledged| acknowledged),
        ))
    }
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

pub(super) async fn push_documents_to_peer(
    node: &Arc<EmbeddedNode>,
    peer_id: &str,
    documents: Vec<P2pDocumentRequest>,
) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let p2p = crate::identity::node_p2p(node)
        .context("embedded node has no P2P transport for hydration delivery")?;
    timeout(
        DEFAULT_EMBEDDED_ADMIN_TIMEOUT,
        p2p.push_documents_to_peer(peer_id, documents),
    )
    .await
    .with_context(|| format!("push hydration documents to peer {peer_id} timed out"))?
    .with_context(|| format!("push hydration documents to peer {peer_id}"))
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
pub(super) mod tests;
