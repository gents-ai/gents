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
        .map_err(|error| RemoteP2pAdminError::LocalError(format!("{error:#}")))?;
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
pub(super) mod tests {
    use std::collections::BTreeSet;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::stream;
    use p2p::iroh::{IrohDiscoveryConfig, IrohRelayModeConfig};
    use rig::completion::{
        CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
    };
    use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::agent::completion_retry::CompletionRetryProfileFields;
    use crate::agent::daemon::AgentDaemon;
    use crate::agent::p2p_reconcile::templates::{
        Scope, AGENT_TARGET_CALLER_TEMPLATE, AGENT_TARGET_HOST_TEMPLATE,
    };
    use crate::agent::p2p_reconcile::{combine_filters, equality_filter};
    use crate::agent::p2p_reconcile::{
        enrollment_authority_channel, run_enrollment_reconciler, GraphqlEnrollmentStore,
    };
    use crate::agent::p2p_reconcile::{resolve_template, scope_filter};
    use crate::agent::runtime::StartupBarrier;
    use crate::backend_provider::BackendProviderKind;
    use crate::config::{ResolvedAgent, SamplingConfig};
    use crate::defra_node::P2PConfig;
    use crate::ensure_runtime_schemas;
    use crate::graphql::escape_graphql_string;
    use crate::hook::{BackgroundExecutionRegistry, BackgroundToolRegistry, FailurePolicy};
    use crate::identity::{KeyIdentity, NodeIdentity, RuntimeNode};
    use crate::llm::tool::ToolDyn;
    use crate::prompt::LayeredPromptBuilder;
    use crate::tool_surface::AgentToolSurfaceConfig;
    use gents_protocol::enrollment::{
        derive_enrollment_id, encode_offer, enrollment_schema_fingerprint, EnrollmentDecisionKind,
        EnrollmentOfferRecord, EnrollmentRequestRecord, ENROLLMENT_PROTOCOL_VERSION,
    };
    use gents_protocol::network_token::NetworkRecord;

    const TEST_SCHEMA: &str = r#"
        type P2pReconcileThing {
            name: String
        }
    "#;

    pub(in crate::agent::p2p_reconcile) struct TestNode {
        pub(in crate::agent::p2p_reconcile) node: Arc<EmbeddedNode>,
        _tempdir: tempfile::TempDir,
    }

    #[test]
    fn adapter_preserves_conjunctive_filters_as_conditions() {
        let filters = [(
            "AgentRequest".to_string(),
            combine_filters(
                equality_filter("requester_did", "did:key:phone"),
                equality_filter("lifecycle_state", "pending"),
            ),
        )]
        .into_iter()
        .collect();

        let converted = to_replication_filters(&filters).expect("supported filters");
        assert_eq!(
            converted["AgentRequest"].conditions.as_ref(),
            serde_json::json!({
                "_and": [
                    { "requester_did": { "_eq": "did:key:phone" } },
                    { "lifecycle_state": { "_eq": "pending" } }
                ]
            })
            .as_object()
        );
    }

    async fn p2p_node_with_identity(node_identity_did: Option<&str>) -> TestNode {
        p2p_node_with_options(node_identity_did, None).await
    }

    async fn p2p_node_with_options(
        node_identity_did: Option<&str>,
        http_address: Option<std::net::SocketAddr>,
    ) -> TestNode {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut builder = EmbeddedNode::builder()
            .data_path(tempdir.path())
            .with_p2p(P2PConfig {
                port: 0,
                bind_addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                relay_mode: IrohRelayModeConfig::Disabled,
                discovery: IrohDiscoveryConfig::Disabled,
                allowlist: p2p::iroh::IrohAllowlistConfig::AcceptAll,
                max_concurrent_multipath_paths: None,
                secret_key_path: None,
                load_persisted_collections: false,
                max_concurrent_dag_fetches: p2p::sync::DEFAULT_MAX_CONCURRENT_DAG_FETCHES,
                max_concurrent_push_tasks: p2p::sync::DEFAULT_MAX_CONCURRENT_PUSH_TASKS,
                rate_limit_burst: p2p::sync::DEFAULT_RATE_LIMIT_BURST,
                rate_limit_rate: p2p::sync::DEFAULT_RATE_LIMIT_RATE,
                max_doc_sync_request_doc_ids: p2p::sync::DEFAULT_MAX_DOC_SYNC_REQUEST_DOC_IDS,
                max_pending_dags: p2p::sync::DEFAULT_MAX_PENDING_DAGS,
                rebroadcast_on_merge: false,
            });
        if let Some(did) = node_identity_did {
            builder = builder.with_node_identity_did(did);
        }
        if let Some(address) = http_address {
            builder = builder.with_http(defra_node::HttpConfig::with_addr(address));
        }
        let node = Arc::new(builder.build().await.expect("embedded p2p node"));
        TestNode {
            node,
            _tempdir: tempdir,
        }
    }

    async fn p2p_node() -> TestNode {
        p2p_node_with_identity(None).await
    }

    async fn test_node() -> TestNode {
        let test = p2p_node().await;
        test.node
            .add_schema(TEST_SCHEMA)
            .await
            .expect("test schema");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if test
                    .node
                    .get_collection("P2pReconcileThing")
                    .expect("collection lookup")
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("committed schema reaches the collection observation");
        test
    }

    #[tokio::test]
    async fn collection_name_resolution_accepts_names_and_ids() {
        let test = test_node().await;
        let definition = test
            .node
            .get_collection("P2pReconcileThing")
            .expect("collection lookup")
            .expect("observed fixture schema");
        let admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&test.node));

        assert_eq!(
            admin
                .resolve_collection_name("P2pReconcileThing")
                .await
                .expect("resolve name token"),
            Some("P2pReconcileThing".to_string())
        );
        assert_eq!(
            admin
                .resolve_collection_name(&definition.collection_id)
                .await
                .expect("resolve id token"),
            Some("P2pReconcileThing".to_string())
        );
    }

    async fn runtime_test_node() -> TestNode {
        let directory = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
        runtime_test_node_with_identity(&identity).await
    }

    async fn runtime_test_node_with_identity(identity: &dyn NodeIdentity) -> TestNode {
        let test = p2p_node_with_identity(Some(identity.did())).await;
        ensure_runtime_schemas(&test.node)
            .await
            .expect("runtime schemas");
        test
    }

    pub(in crate::agent::p2p_reconcile) async fn runtime_http_test_node(
        identity: &dyn NodeIdentity,
    ) -> (TestNode, String) {
        let address = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let test = p2p_node_with_options(Some(identity.did()), Some(address)).await;
        ensure_runtime_schemas(&test.node).await.unwrap();
        let endpoint = format!("http://{address}/api/v0/graphql");
        let access = crate::config_client::ConfigAccess::graphql(&endpoint);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if access.execute("{ AgentRequest { _docID } }").await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("native HTTP endpoint becomes ready");
        (test, endpoint)
    }

    fn bs58_sig(signature: &[u8]) -> String {
        bs58::encode(signature).into_string()
    }

    pub(in crate::agent::p2p_reconcile) async fn authorize_enrollment_peer(
        node: Arc<EmbeddedNode>,
        admin_identity: Arc<dyn NodeIdentity>,
        member_identity: Arc<dyn NodeIdentity>,
        member_peer: &str,
        member_ticket: &str,
    ) -> String {
        let issued = chrono::Utc::now();
        let issued_at = issued.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let expires_at = (issued + chrono::Duration::minutes(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let existing_network = GraphqlEnrollmentStore::new(node.clone(), admin_identity.clone())
            .load_projection()
            .await
            .expect("load enrollment fixture network")
            .network_id;
        let network_id = existing_network
            .clone()
            .unwrap_or_else(|| format!("network-{}", uuid::Uuid::new_v4()));
        if existing_network.is_none() {
            let mut network = NetworkRecord {
                network_id: network_id.clone(),
                admin_did: admin_identity.did().to_string(),
                display_name: "final admission test".to_string(),
                default_template: "conversation".to_string(),
                created_at: issued_at.clone(),
                sig: Vec::new(),
            };
            network.sig = admin_identity
                .sign(&network.signing_payload())
                .await
                .expect("sign test network");
            let response = node
                .execute(&format!(
                    r#"mutation {{ create_Network(input: {{
                    network_id: "{}", admin_did: "{}", display_name: "{}",
                    default_template: "{}", created_at: "{}", admin_sig: "{}"
                }}) {{ _docID }} }}"#,
                    escape_graphql_string(&network.network_id),
                    escape_graphql_string(&network.admin_did),
                    escape_graphql_string(&network.display_name),
                    escape_graphql_string(&network.default_template),
                    escape_graphql_string(&network.created_at),
                    escape_graphql_string(&bs58_sig(&network.sig)),
                ))
                .await;
            assert!(
                !response.has_errors(),
                "create signed Network: {:?}",
                response.errors
            );
        }

        let p2p = node.p2p().expect("admin P2P enabled");
        let server_peer = p2p.local_peer_id().await.expect("admin peer id");
        let server_ticket = p2p
            .shareable_address()
            .await
            .expect("admin ticket lookup")
            .expect("admin shareable ticket");
        let challenge = format!("challenge-{}", uuid::Uuid::new_v4());
        let offer_id = format!(
            "offer-{}",
            derive_enrollment_id(
                "gents-enrollment-offer-v1",
                &[
                    &network_id,
                    admin_identity.did(),
                    &server_peer,
                    &challenge,
                    &issued_at,
                ],
            )
        );
        let mut offer = EnrollmentOfferRecord {
            version: ENROLLMENT_PROTOCOL_VERSION,
            offer_id,
            challenge,
            network_id: network_id.clone(),
            admin_did: admin_identity.did().to_string(),
            server_peer,
            server_ticket,
            owner_agent: admin_identity.did().to_string(),
            profile: "client".to_string(),
            schema_fingerprint: enrollment_schema_fingerprint(),
            issued_at: issued_at.clone(),
            expires_at: expires_at.clone(),
            admin_sig: Vec::new(),
        };
        offer.admin_sig = admin_identity
            .sign(&offer.signing_payload())
            .await
            .expect("sign enrollment offer");
        let offer_token = encode_offer(&offer).expect("encode enrollment offer");
        let client_nonce = format!("nonce-{}", uuid::Uuid::new_v4());
        let request_id = format!(
            "enroll-{}",
            derive_enrollment_id(
                "gents-enrollment-request-id-v1",
                &[
                    &offer.offer_id,
                    member_identity.did(),
                    member_peer,
                    &client_nonce,
                ],
            )
        );
        let mut request = EnrollmentRequestRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            request_id,
            request_digest: String::new(),
            offer_id: offer.offer_id.clone(),
            offer_token,
            challenge: offer.challenge.clone(),
            network_id,
            admin_did: admin_identity.did().to_string(),
            server_peer: offer.server_peer.clone(),
            candidate_did: member_identity.did().to_string(),
            candidate_peer: member_peer.to_string(),
            candidate_ticket: member_ticket.to_string(),
            owner_agent: offer.owner_agent.clone(),
            profile: offer.profile.clone(),
            client_nonce,
            issued_at,
            expires_at,
            candidate_sig: Vec::new(),
        };
        request.request_digest = request.computed_digest();
        request.candidate_sig = member_identity
            .sign(&request.signing_payload())
            .await
            .expect("sign enrollment request");
        request
            .validate_against_offer(&offer)
            .expect("validate enrollment request");

        let response = node
            .execute(&format!(
                r#"mutation {{ create_NetworkEnrollmentRequest(input: {{
                    protocol_version: {}, request_id: "{}", request_digest: "{}",
                    offer_id: "{}", offer_token: "{}", challenge: "{}",
                    network_id: "{}", admin_did: "{}", server_peer: "{}",
                    candidate_did: "{}", candidate_peer: "{}", candidate_ticket: "{}",
                    owner_agent: "{}", profile: "{}", client_nonce: "{}",
                    issued_at: "{}", expires_at: "{}", candidate_sig: "{}"
                }}) {{ _docID }} }}"#,
                request.protocol_version,
                escape_graphql_string(&request.request_id),
                escape_graphql_string(&request.request_digest),
                escape_graphql_string(&request.offer_id),
                escape_graphql_string(&request.offer_token),
                escape_graphql_string(&request.challenge),
                escape_graphql_string(&request.network_id),
                escape_graphql_string(&request.admin_did),
                escape_graphql_string(&request.server_peer),
                escape_graphql_string(&request.candidate_did),
                escape_graphql_string(&request.candidate_peer),
                escape_graphql_string(&request.candidate_ticket),
                escape_graphql_string(&request.owner_agent),
                escape_graphql_string(&request.profile),
                escape_graphql_string(&request.client_nonce),
                escape_graphql_string(&request.issued_at),
                escape_graphql_string(&request.expires_at),
                escape_graphql_string(&bs58_sig(&request.candidate_sig)),
            ))
            .await;
        assert!(
            !response.has_errors(),
            "create signed enrollment request: {:?}",
            response.errors
        );
        GraphqlEnrollmentStore::new(node, admin_identity)
            .decide_request(&request.request_id, EnrollmentDecisionKind::Approved)
            .await
            .expect("approve live enrollment request");
        request.request_id
    }

    #[derive(Clone)]
    struct CountingReplyModel(Arc<AtomicUsize>);

    #[allow(refining_impl_trait)]
    impl CompletionModel for CountingReplyModel {
        type Response = ();
        type StreamingResponse = ();
        type Client = ();

        fn make(_: &Self::Client, _: impl Into<String>) -> Self {
            Self(Arc::new(AtomicUsize::new(0)))
        }

        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse<()>, CompletionError> {
            Err(CompletionError::ProviderError("unused".into()))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::iter(vec![
                Ok(RawStreamingChoice::Message("admitted reply".to_string())),
                Ok(RawStreamingChoice::FinalResponse(())),
            ]));
            Ok(StreamingCompletionResponse::stream(inner))
        }
    }

    fn test_agent(identity: Arc<dyn NodeIdentity>) -> Arc<ResolvedAgent> {
        let principal = Arc::new(RuntimeNode {
            node_did: identity.did().to_string(),
            identity,
            default_agent_id: "behavior-1".to_string(),
            display_name: None,
            enabled: true,
        });
        Arc::new(ResolvedAgent {
            agent_id: "behavior-1".to_string(),
            node: principal,
            backend_id: Some("backend-behavior-1".to_string()),
            backend_provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
            backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
            backend_auth: crate::document_config::BackendAuth::Unauthenticated,
            model_name: "scripted".to_string(),
            service_tier: None,
            resolved_reasoning_efforts: None,
            context_window: 8_192,
            max_output_tokens: 1_024,
            max_turns: 2,
            max_turns_provenance: crate::config::MaxTurnsProvenance::Default,
            system_prompt: "system".to_string(),
            tools: AgentToolSurfaceConfig::meta_only(),
            compaction: None,
            compaction_inference: None,
            max_total_tokens: None,
            stream_batch_ms: 0,
            stream_liveness_timeout: Duration::from_secs(5),
            deadline_duration: Duration::from_secs(30),
            provider_idle_timeout: Duration::from_secs(
                crate::config::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
            ),
            completion_retry: CompletionRetryProfileFields::default(),
            sampling: SamplingConfig::default(),
            skills: Vec::new(),
        })
    }

    fn behavior_daemon(
        node: Arc<EmbeddedNode>,
        behavior: Arc<ResolvedAgent>,
        authority: super::super::EnrollmentAuthorityHandle,
        calls: Arc<AtomicUsize>,
    ) -> AgentDaemon<CountingReplyModel> {
        let prompt_builder = LayeredPromptBuilder::for_agent(
            &behavior.system_prompt,
            &behavior.agent_id,
            &[],
            false,
            &[],
        );
        let runtime_status = crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            behavior.node_did().to_string(),
        );
        AgentDaemon::new(
            node.clone(),
            behavior.clone(),
            None,
            Arc::new(CountingReplyModel(calls)),
            prompt_builder.preamble().to_string(),
            Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            FailurePolicy::default(),
            None,
            BackgroundToolRegistry::default(),
            BackgroundExecutionRegistry::default(),
            Arc::new(StartupBarrier::ready_for_test()),
            runtime_status,
            1,
            crate::request_admission::AgentRequestAdmissionVerifier::new(
                node,
                behavior.node_identity().clone(),
                authority,
            ),
        )
        .expect("valid canonical daemon fixture")
    }

    async fn seed_peer_session_message(
        node: &Arc<EmbeddedNode>,
        lifecycle: &crate::lifecycle::RequestLifecycle,
        writer: &crate::streaming::DefraStreamWriter,
        host_did: &str,
        tool_call_id: &str,
        prompt: &str,
        turn: usize,
    ) -> String {
        use crate::tool_call_lifecycle::{AwaitMode, ToolCallLifecycle};
        use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
        writer
            .start_provider_attempt(
                &lifecycle.request().doc_id,
                turn,
                0,
                format!("inference.{}", turn + 1).parse().unwrap(),
            )
            .await;
        let message = Message::Assistant {
            id: Some(format!("remote-provider-{turn}")),
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: tool_call_id.into(),
                call_id: Some(tool_call_id.into()),
                function: ToolFunction::new(
                    crate::toolset::AGENT_NEW_TOOL_NAME.into(),
                    serde_json::json!({ "agent": "remote-target", "prompt": prompt }),
                ),
                signature: None,
                additional_params: None,
            })],
        };
        let publication = writer
            .publish_native_turn(lifecycle, turn, 0, &message)
            .await
            .expect("accept remote agent_new intent");
        let accepted = publication.accepted_tools.into_iter().next().unwrap();
        let mut row = ToolCallLifecycle::from_accepted(
            node.clone(),
            lifecycle.request().node_did.clone(),
            lifecycle.request().requester_did.clone(),
            accepted,
            lifecycle.claimed_deadline_at().unwrap(),
            AwaitMode::Background,
        )
        .expect("adopt accepted agent_new");
        let cause = crate::lifecycle::SessionMessageCause {
            caller_node_did: lifecycle.request().node_did.clone(),
            caller_request_id: lifecycle.request().request_id.clone(),
            caller_request_doc_id: lifecycle.request().doc_id.clone(),
            caller_hop: lifecycle.request().request_hop,
            tool_call_id: row.tool_call_id().to_owned(),
            tool_call_doc_id: row.doc_id().unwrap().to_owned(),
            correlation: None,
        };
        let target = crate::lifecycle::SessionMessageTarget {
            node_did: host_did.to_owned(),
            agent_id: "behavior-1".to_owned(),
            session_id: uuid::Uuid::new_v4().to_string(),
        };
        let plan = crate::session_message::plan(
            node,
            &cause,
            &target,
            crate::session_message::RenderedBody {
                content: prompt.to_owned(),
                goal: None,
            },
            None,
            false,
        )
        .await
        .expect("plan peer session message")
        .expect("peer session message admitted by the caller");
        crate::session_message::commit(node, &cause, &mut row, plan, false)
            .await
            .expect("materialize the caller-signed Peer request")
            .request_id
    }

    pub(in crate::agent::p2p_reconcile) async fn wait_for_peer_info(
        admin: &EmbeddedRemoteP2pAdmin,
    ) -> Vec<String> {
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

    fn collection_id(node: &EmbeddedNode, name: &str) -> String {
        node.get_collection(name)
            .expect("collection lookup")
            .expect("collection")
            .collection_id
    }

    pub(in crate::agent::p2p_reconcile) async fn wait_for_active_peer(
        admin: &EmbeddedRemoteP2pAdmin,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let peers = admin.active_peers().await.expect("active peers");
            if !peers.is_empty() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("node never reported an active P2P peer");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn exec(node: &EmbeddedNode, statement: &str, context: &str) {
        let response = node.execute(statement).await;
        assert!(
            !response.has_errors(),
            "{context} failed: {:?}\n{statement}",
            response.errors
        );
    }

    async fn seed_agent_request(node: &EmbeddedNode, request_id: &str, requester_did: &str) {
        let session_id = format!("{request_id}-session");
        seed_agent_request_for_session(node, request_id, &session_id, Some(requester_did)).await;
    }

    async fn seed_agent_request_for_session(
        node: &EmbeddedNode,
        request_id: &str,
        session_id: &str,
        requester_did: Option<&str>,
    ) {
        let request_id = escape_graphql_string(request_id);
        let node_did = "did:key:host";
        let requester_did_field = crate::session::requester_did_create_field(requester_did);
        let session_id = escape_graphql_string(session_id);
        let agent_id = escape_graphql_string(&format!("{node_did}:default"));
        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    purpose: "normal",
                    node_did: "{node_did}",
                    {requester_did_field}
                    agent_id: "{agent_id}",
                    session_id: "{session_id}",
                    retry_parent_request: "",
                    retry_root_request: "{request_id}",
                    superseded_by_request: "",
                    content: "issue 604 filtered replay",
                    lifecycle_state: "processing",
                    backend_id: "",
                    execution_origin: "interactive",
                    failure_reason: "",
                    created_at: "2026-07-06T00:00:00Z",
                    deadline: "2026-07-06T01:00:00Z",
                    retry_count: 0,
                    max_retries: 3,
                    request_hop: 0
                }}) {{ _docID }}
            }}"#
        );
        exec(node, &mutation, "seed AgentRequest").await;
    }

    async fn seed_agent_tool_call(node: &EmbeddedNode, tool_call_id: &str, requester_did: &str) {
        let tool_call_id = escape_graphql_string(tool_call_id);
        let requester_did = escape_graphql_string(requester_did);
        let tool_call_key = escape_graphql_string(&format!("issue-604-session:{tool_call_id}"));
        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    request_id: "parent-match",
                    session_id: "issue-604-session",
                    node_did: "did:key:coord",
                    requester_did: "{requester_did}",
                    message_sequence: 1,
                    tool_name: "agent_new",
                    tool_call_id: "{tool_call_id}",
                    lifecycle_state: "running",
                    started_at: "2026-07-06T00:00:00Z",
                    await_mode: "background"
                }}) {{ _docID }}
            }}"#
        );
        exec(node, &mutation, "seed AgentToolCall").await;
    }

    async fn seed_agent_target_return_artifacts(
        node: &Arc<EmbeddedNode>,
        suffix: &str,
        requester_did: Option<&str>,
    ) -> (String, String, String, u32) {
        let request_id = format!("return-{suffix}-response");
        let session_id = format!("return-{suffix}-session");
        let node_did = "did:key:host";
        let agent_id = "did:key:host:default";

        seed_agent_request_for_session(node, &request_id, &session_id, requester_did).await;

        crate::session::ensure_session_with_agent_id_and_requester_did(
            node,
            &session_id,
            node_did,
            agent_id,
            requester_did,
        )
        .await
        .expect("create routed AgentSession");
        let response = node.execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "pending" }}) {{ {} }} }}"#,
            escape_graphql_string(&request_id),
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        let request =
            crate::watcher::agent_request_from_mutation_response(&response, "update_AgentRequest")
                .expect("decode routed request")
                .expect("routed request");
        let mut lifecycle = crate::lifecycle::RequestLifecycle::new_with_node_did(
            Arc::clone(node),
            "default",
            node_did,
            request,
            60,
        );
        assert_eq!(
            lifecycle.claim().await.expect("claim routed request"),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        let writer = crate::streaming::DefraStreamWriter::new(
            Arc::clone(node),
            node_did,
            Duration::from_secs(60),
        );
        lifecycle
            .begin_owned_execution(&writer)
            .await
            .expect("begin routed execution");
        writer
            .start_provider_attempt(
                &lifecycle.request().doc_id,
                0,
                0,
                gents_protocol::rendered_request::CaptureScope {
                    kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                    seq: 0,
                },
            )
            .await;
        let published = writer
            .publish_native_turn(
                &lifecycle,
                0,
                0,
                &gents_protocol::message::Message::assistant("child result"),
            )
            .await
            .expect("publish routed child result turn");
        lifecycle
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: published.message_doc_id.clone(),
                },
                None,
            )
            .await
            .expect("complete routed request");
        (
            request_id,
            session_id,
            published.message_doc_id,
            published.sequence,
        )
    }

    async fn collection_values(
        node: &EmbeddedNode,
        collection: &str,
        field: &str,
    ) -> BTreeSet<String> {
        let query = format!("{{ {collection} {{ {field} }} }}");
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "query {collection}.{field} failed: {:?}",
            response.errors
        );
        response
            .data
            .as_ref()
            .and_then(|data| data.get(collection))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get(field).and_then(serde_json::Value::as_str))
            .map(ToOwned::to_owned)
            .collect()
    }

    async fn wait_for_value(node: &EmbeddedNode, collection: &str, field: &str, expected: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let last = collection_values(node, collection, field).await;
            if last.contains(expected) {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for {collection}.{field}={expected}; last={last:?}");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn load_admission_request_by_id(
        node: &EmbeddedNode,
        request_id: &str,
    ) -> crate::watcher::AgentRequest {
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
                escape_graphql_string(request_id),
            ))
            .await;
        let doc_id = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(serde_json::Value::as_str)
            .expect("child doc id");
        crate::request_admission::load_request_for_admission_test(node, doc_id)
            .await
            .expect("load child")
    }

    #[tokio::test]
    async fn embedded_collections_round_trip() {
        let test = test_node().await;
        let node = Arc::clone(&test.node);
        let admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&node));
        let collection_name = "P2pReconcileThing".to_string();

        admin
            .add_p2p_collections(std::slice::from_ref(&collection_name))
            .await
            .expect("add collection");

        let collections = admin
            .list_p2p_collections()
            .await
            .expect("list collections");
        assert_eq!(collections, vec![collection_name]);

        node.shutdown().await;
    }

    /// Every builtin template's filter fields must be `@immutable` in the
    /// target collection's schema, or `add_replicator` rejects the install at
    /// pairing time.
    #[tokio::test]
    async fn embedded_all_builtin_template_filters_pass_replicator_validation() {
        let local_test = runtime_test_node().await;
        let remote_test = runtime_test_node().await;
        let local = Arc::clone(&local_test.node);
        let remote = Arc::clone(&remote_test.node);
        let local_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&local));
        let remote_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&remote));
        let remote_addresses = wait_for_peer_info(&remote_admin).await;
        local_admin
            .connect(&remote_addresses)
            .await
            .expect("connect remote");
        wait_for_active_peer(&local_admin).await;
        wait_for_active_peer(&remote_admin).await;

        for template in super::super::templates::builtin_templates() {
            if template.collections.is_empty() {
                continue;
            }
            let filters = match &template.scope {
                Scope::ClientRoute => super::super::policy::resolve_template_filters(
                    template,
                    super::super::policy::PairingDirection::ClientToRuntime,
                    "did:key:z6MkPeerForFilterValidation",
                    "did:key:z6MkSelfForFilterValidation",
                ),
                _ => super::super::templates::scope_filter(
                    &template.scope,
                    template.collections,
                    "did:key:z6MkPeerForFilterValidation",
                    "did:key:z6MkSelfForFilterValidation",
                ),
            };
            match &template.scope {
                Scope::Unscoped => assert!(
                    filters.is_empty(),
                    "unscoped template '{}' unexpectedly has filters",
                    template.id
                ),
                Scope::PerCollection(rules) => {
                    let filter_collections =
                        filters.keys().map(String::as_str).collect::<BTreeSet<_>>();
                    let scoped_collections = rules
                        .iter()
                        .map(|rule| rule.collection)
                        .collect::<BTreeSet<_>>();
                    assert_eq!(
                        filter_collections, scoped_collections,
                        "per-collection template '{}' must filter every scoped collection",
                        template.id
                    );
                }
                Scope::PeerDid { .. } => {}
                Scope::ClientRoute => assert!(!filters.is_empty()),
            }
            local_admin
                .add_replicator(
                    &remote_addresses,
                    &template
                        .collections
                        .iter()
                        .map(|c| (*c).to_string())
                        .collect::<Vec<_>>(),
                    &filters,
                )
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "template '{}' filter set rejected by add_replicator: {error:#}",
                        template.id
                    )
                });
        }

        local.shutdown().await;
        remote.shutdown().await;
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
        let expected_collection_id = collection_id(&local, "P2pReconcileThing");

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

    #[tokio::test]
    async fn embedded_filtered_replicator_replays_existing_matching_documents() {
        let sender_test = runtime_test_node().await;
        let receiver_test = runtime_test_node().await;
        let sender = Arc::clone(&sender_test.node);
        let receiver = Arc::clone(&receiver_test.node);
        let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&sender));
        let receiver_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&receiver));
        let sender_addresses = wait_for_peer_info(&sender_admin).await;
        let receiver_addresses = wait_for_peer_info(&receiver_admin).await;

        seed_agent_request(&sender, "child-match", "did:key:coord").await;
        seed_agent_request(&sender, "child-other", "did:key:other").await;
        seed_agent_tool_call(&sender, "bridge-match", "did:key:host").await;
        seed_agent_tool_call(&sender, "bridge-other", "did:key:other").await;

        sender_admin
            .connect(&receiver_addresses)
            .await
            .expect("connect sender to receiver");
        wait_for_active_peer(&sender_admin).await;
        wait_for_active_peer(&receiver_admin).await;

        let collections = vec!["AgentRequest".to_string(), "AgentToolCall".to_string()];
        receiver_admin
            .add_replicator(&sender_addresses, &collections, &PairingFilters::default())
            .await
            .expect("authorize sender as receiver-side replicator");
        assert!(
            sender_admin
                .list_p2p_collections()
                .await
                .expect("list sender P2P subscriptions")
                .is_empty(),
            "filtered Push sender must not subscribe to whole collections"
        );
        assert!(
            receiver_admin
                .list_p2p_collections()
                .await
                .expect("list receiver P2P subscriptions")
                .is_empty(),
            "filtered Push receiver must not subscribe to whole collections"
        );

        // Give any independently configured route a chance to replay before
        // installing the sender filter.
        tokio::time::sleep(Duration::from_millis(250)).await;
        let mut filters = PairingFilters::new();
        filters.insert(
            "AgentRequest".to_string(),
            equality_filter("requester_did", "did:key:coord"),
        );
        filters.insert(
            "AgentToolCall".to_string(),
            equality_filter("requester_did", "did:key:host"),
        );
        sender_admin
            .add_replicator(&receiver_addresses, &collections, &filters)
            .await
            .expect("install filtered sender to receiver replicator");

        wait_for_value(&receiver, "AgentRequest", "request_id", "child-match").await;
        wait_for_value(&receiver, "AgentToolCall", "tool_call_id", "bridge-match").await;

        let request_ids = collection_values(&receiver, "AgentRequest", "request_id").await;
        let tool_call_ids = collection_values(&receiver, "AgentToolCall", "tool_call_id").await;
        assert_eq!(
            request_ids,
            BTreeSet::from(["child-match".to_string()]),
            "filtered request replay should not leak non-matching rows"
        );
        assert_eq!(
            tool_call_ids,
            BTreeSet::from(["bridge-match".to_string()]),
            "filtered tool-call replay should not leak non-matching rows"
        );

        sender.shutdown().await;
        receiver.shutdown().await;
    }

    #[tokio::test]
    async fn explicit_file_image_push_keeps_unselected_images_and_collection_interest_local() {
        let keys = tempfile::tempdir().unwrap();
        let sender_identity =
            KeyIdentity::load_or_create(keys.path().join("sender.key"), None).unwrap();
        let receiver_identity =
            KeyIdentity::load_or_create(keys.path().join("receiver.key"), None).unwrap();
        assert_ne!(sender_identity.did(), receiver_identity.did());
        let (sender_test, sender_http) = runtime_http_test_node(&sender_identity).await;
        let (receiver_test, receiver_http) = runtime_http_test_node(&receiver_identity).await;
        let sender = &sender_test.node;
        let receiver = &receiver_test.node;
        let sender_admin = EmbeddedRemoteP2pAdmin::new(sender.clone());
        let receiver_admin = EmbeddedRemoteP2pAdmin::new(receiver.clone());
        let sender_addresses = wait_for_peer_info(&sender_admin).await;
        let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
        sender_admin.connect(&receiver_addresses).await.unwrap();
        wait_for_active_peer(&sender_admin).await;
        wait_for_active_peer(&receiver_admin).await;
        receiver_admin
            .add_replicator(
                &sender_addresses,
                &["FileImageHydrationRequest".into()],
                &PairingFilters::default(),
            )
            .await
            .unwrap();
        let mut selected = String::new();
        for tool in ["selected-tool", "unselected-tool"] {
            let images = serde_json::json!({
                "version": 1, "tool_call_doc_id": tool, "changes": [{
                    "root": "/recorded", "path": "edited.txt", "before": {"kind": "missing"},
                    "after": {"kind": "unavailable", "reason": "tooLarge"}
                }]
            });
            let variables = serde_json::json!({"input": {
                "tool_call_doc_id": tool, "request_doc_id": "request",
                "node_did": sender_identity.did(), "requester_did": receiver_identity.did(),
                "session_id": "session", "file_changes": images,
            }});
            let response = crate::config_client::ConfigAccess::transact_local_write(
                sender, None, "test.file_images.seed", move |txn| {
                    let variables = variables.clone();
                    Box::pin(async move { txn.execute_with_variables(
                        "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }",
                        &variables,
                    ).await })
                },
            ).await.unwrap();
            if tool == "selected-tool" {
                selected = crate::graphql::created_doc_id(&response, "ToolFileChange").unwrap();
            }
        }
        let receiver_access = crate::config_client::ConfigAccess::local(receiver.clone());
        assert!(receiver_access
            .execute("{ ToolFileChange { _docID } }")
            .await
            .unwrap()["data"]["ToolFileChange"]
            .as_array()
            .unwrap()
            .is_empty());
        let peer_id = receiver.p2p().unwrap().local_peer_id().await.unwrap();
        push_documents_to_peer(
            sender,
            &peer_id,
            vec![P2pDocumentRequest {
                collection: "ToolFileChange".into(),
                doc_id: selected.clone(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(
            sender_admin
                .document_heads_acknowledged(
                    &peer_id,
                    "ToolFileChange",
                    std::slice::from_ref(&selected)
                )
                .await
                .unwrap(),
            [false]
        );
        sender_admin
            .add_replicator(
                &receiver_addresses,
                &["FileImageHydrationRequest".into()],
                &PairingFilters::default(),
            )
            .await
            .unwrap();
        push_documents_to_peer(
            sender,
            &peer_id,
            vec![P2pDocumentRequest {
                collection: "ToolFileChange".into(),
                doc_id: selected.clone(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(
            sender_admin
                .document_heads_acknowledged(
                    &peer_id,
                    "ToolFileChange",
                    std::slice::from_ref(&selected)
                )
                .await
                .unwrap(),
            [true]
        );
        let mut demand = gents_protocol::file_image_hydration::FileImageDemand {
            admission: gents_protocol::session_hydration::HydrationRequest {
                request_key: String::new(),
                peer_id: peer_id.clone(),
                requester_did: receiver_identity.did().into(),
                node_did: sender_identity.did().into(),
                session_id: "session".into(),
            },
            request_doc_id: "request".into(),
            tool_call_doc_id: "selected-tool".into(),
        };
        demand.admission.request_key = demand.derive_request_key();
        let mut receipt = gents_protocol::file_image_hydration::FileImageHydrationReceipt {
            version: gents_protocol::file_image_hydration::FILE_IMAGE_RECEIPT_VERSION,
            demand: demand.clone(),
            selection: gents_protocol::file_image_hydration::FileImageSelection::Selected {
                document: selected.clone(),
            },
            detail: String::new(),
            processed_at: "2026-10-20T00:00:00Z".into(),
            signer_did: sender_identity.did().into(),
            signature: Vec::new(),
        };
        receipt.signature = sender_identity
            .sign(&receipt.signing_payload().unwrap())
            .await
            .unwrap();
        let receipt = crate::tool_call_lifecycle::VerifiedFileImageReceipt::verify(
            receipt,
            &demand,
            &receiver_identity,
        )
        .await
        .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !receipt.grant_local_read(receiver.clone()).await.unwrap() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "image creator registration did not arrive"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        wait_for_value(
            receiver,
            "ToolFileChange",
            "tool_call_doc_id",
            "selected-tool",
        )
        .await;
        let result = receiver_access
            .execute("{ ToolFileChange { _docID tool_call_doc_id file_changes } }")
            .await
            .unwrap();
        let rows = result["data"]["ToolFileChange"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["_docID"], selected);
        assert_eq!(rows[0]["file_changes"]["tool_call_doc_id"], "selected-tool");
        let outsider_identity =
            KeyIdentity::load_or_create(keys.path().join("outsider.key"), None).unwrap();
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        for case in snapshot["stored_file_image_access_cases"]
            .as_array()
            .unwrap()
        {
            let (owner, endpoint) = match case["owner"].as_str().unwrap() {
                "node" => (sender, sender_http.as_str()),
                "client" => (receiver, receiver_http.as_str()),
                role => panic!("unknown modeled storage owner: {role}"),
            };
            let actor = case["actor"].as_str().map(|actor| match actor {
                "node" => sender_identity.did(),
                "client" => receiver_identity.did(),
                "outsider" => outsider_identity.did(),
                role => panic!("unknown modeled actor: {role}"),
            });
            let visible = if let Some(actor) = actor {
                let did = actor.parse().unwrap();
                let result = crate::config_client::ConfigAccess::transact_local_write(
                    owner,
                    Some(did),
                    "test.file_images.storage_access",
                    |txn| {
                        Box::pin(async move { txn.execute("{ ToolFileChange { _docID } }").await })
                    },
                )
                .await
                .unwrap();
                result["data"]["ToolFileChange"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["_docID"] == selected)
            } else {
                let collection = owner.get_collection("ToolFileChange").unwrap().unwrap();
                owner
                    .document_acp()
                    .check_doc_access(
                        &defra_node::AcpIdentity::Anonymous,
                        defra_node::DocumentPermission::Read,
                        gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
                        gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE,
                        &collection.collection_id,
                    )
                    .await
                    .unwrap()
            };
            assert_eq!(visible, case["expected"].as_bool().unwrap(), "{case}");
            let http = match actor {
                Some(did) => crate::config_client::ConfigAccess::graphql_as(endpoint, did),
                None => crate::config_client::ConfigAccess::graphql(endpoint),
            };
            let response = http
                .execute("{ ToolFileChange { _docID file_changes } }")
                .await
                .unwrap();
            let http_visible = response["data"]["ToolFileChange"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["_docID"] == selected);
            assert_eq!(
                http_visible,
                case["expected"].as_bool().unwrap(),
                "HTTP: {case}"
            );
        }
        let outsider_test = runtime_test_node_with_identity(&outsider_identity).await;
        let outsider = &outsider_test.node;
        let outsider_admin = EmbeddedRemoteP2pAdmin::new(outsider.clone());
        outsider_admin.connect(&receiver_addresses).await.unwrap();
        wait_for_active_peer(&outsider_admin).await;
        let pull = outsider
            .p2p()
            .unwrap()
            .sync_documents(
                "ToolFileChange",
                vec![selected.clone()],
                Some(Duration::from_secs(5)),
            )
            .await;
        let copied = crate::config_client::ConfigAccess::local(outsider.clone())
            .execute("{ ToolFileChange { _docID } }")
            .await
            .unwrap();
        assert!(
            copied["data"]["ToolFileChange"]
                .as_array()
                .unwrap()
                .is_empty(),
            "an unrelated transport peer fetched a retained image from its recipient: {pull:?}"
        );
        assert!(sender_admin
            .list_p2p_collections()
            .await
            .unwrap()
            .is_empty());
        assert!(receiver_admin
            .list_p2p_collections()
            .await
            .unwrap()
            .is_empty());
        sender.shutdown().await;
        receiver.shutdown().await;
        outsider.shutdown().await;
    }

    #[tokio::test]
    async fn peer_request_final_admission_uses_the_requester_signature_and_fresh_authority() {
        let identity_temp = tempfile::tempdir().expect("identity tempdir");
        let coordinator_identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(identity_temp.path().join("coordinator.key"), None)
                .expect("coordinator identity"),
        );
        let host_identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(identity_temp.path().join("host.key"), None)
                .expect("host identity"),
        );
        let coordinator_test = runtime_test_node_with_identity(coordinator_identity.as_ref()).await;
        let host_test = runtime_test_node_with_identity(host_identity.as_ref()).await;
        let coordinator = Arc::clone(&coordinator_test.node);
        let host = Arc::clone(&host_test.node);
        let coordinator_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&coordinator));
        let host_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&host));
        let coordinator_addresses = wait_for_peer_info(&coordinator_admin).await;
        let host_addresses = wait_for_peer_info(&host_admin).await;

        let host_did = host_identity.did().to_string();
        let coordinator_did = coordinator_identity.did().to_string();
        let parent_request_id = "remote-parent-1";

        let parent_response = coordinator
            .execute(&format!(
                r#"mutation {{ create_AgentRequest(input: {{
                    request_id: "{parent_request_id}", node_did: "{coordinator_did}",
                    purpose: "normal",
                    requester_did: "{coordinator_did}", agent_id: "parent-behavior",
                    session_id: "remote-session", retry_root_request: "{parent_request_id}",
                    content: "remote parent", lifecycle_state: "pending",
                    execution_origin: "interactive", created_at: "2026-08-30T00:00:00Z",
                    retry_count: 0, max_retries: 3, request_hop: 0
                }}) {{ _docID }} }}"#,
            ))
            .await;
        assert!(
            !parent_response.has_errors(),
            "seed parent: {:?}",
            parent_response.errors
        );
        let _parent_doc_id = parent_response
            .data
            .as_ref()
            .and_then(|data| {
                data.get("create_AgentRequest")
                    .or_else(|| data.get("add_AgentRequest"))
            })
            .and_then(|value| {
                value.get("_docID").or_else(|| {
                    value
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("_docID"))
                })
            })
            .and_then(serde_json::Value::as_str)
            .expect("parent doc id")
            .to_string();
        let parent_request =
            crate::request_binding::load_agent_request(&coordinator, parent_request_id)
                .await
                .unwrap()
                .unwrap();
        let mut parent_lifecycle = crate::lifecycle::RequestLifecycle::new_with_node_did(
            coordinator.clone(),
            "parent-behavior",
            &coordinator_did,
            parent_request,
            120,
        );
        assert_eq!(
            parent_lifecycle.claim().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        let parent_writer = crate::streaming::DefraStreamWriter::new(
            coordinator.clone(),
            &coordinator_did,
            Duration::ZERO,
        );
        parent_lifecycle
            .begin_owned_execution(&parent_writer)
            .await
            .unwrap();
        let fresh_request_id = seed_peer_session_message(
            &coordinator,
            &parent_lifecycle,
            &parent_writer,
            &host_did,
            "message-1",
            "delegated work",
            0,
        )
        .await;
        let revoked_request_id = seed_peer_session_message(
            &coordinator,
            &parent_lifecycle,
            &parent_writer,
            &host_did,
            "message-2",
            "delegated work after revocation",
            1,
        )
        .await;

        // The receiving node's own agent configures the request; the Peer
        // branch reads only the requester's ACP authority.
        let tools: crate::document_config::Tools = serde_json::from_value(serde_json::json!({
            "tools_id":"host-tools","node_did":host_did
        }))
        .unwrap();
        let context: crate::document_config::AgentContext =
            serde_json::from_value(serde_json::json!({
                "context_id":"host-context","node_did":host_did,"tools_id":"host-tools"
            }))
            .unwrap();
        let behavior: crate::document_config::Agent = serde_json::from_value(serde_json::json!({
            "agent_id":"behavior-1","node_did":host_did,"context_id":"host-context",
            "inference_profile_id":"host-profile"
        }))
        .unwrap();
        for (collection, document) in [
            ("Tools", serde_json::to_value(tools).unwrap()),
            ("AgentContext", serde_json::to_value(context).unwrap()),
            ("Agent", serde_json::to_value(behavior).unwrap()),
        ] {
            let input = gents_protocol::graphql::graphql_input_literal(&document).unwrap();
            exec(
                &host,
                &format!("mutation {{create_{collection}(input:{input}){{_docID}}}}"),
                "seed target context policy",
            )
            .await;
        }

        let template =
            resolve_template(AGENT_TARGET_CALLER_TEMPLATE).expect("agent-target-caller template");
        let collections = template
            .collections
            .iter()
            .map(|collection| (*collection).to_string())
            .collect::<Vec<_>>();
        coordinator_admin
            .connect(&host_addresses)
            .await
            .expect("connect host");
        wait_for_active_peer(&coordinator_admin).await;
        wait_for_active_peer(&host_admin).await;
        let coordinator_p2p = coordinator.p2p().expect("coordinator P2P enabled");
        let coordinator_peer = coordinator_p2p
            .local_peer_id()
            .await
            .expect("coordinator peer id");
        let coordinator_ticket = coordinator_p2p
            .shareable_address()
            .await
            .expect("coordinator ticket lookup")
            .expect("coordinator shareable ticket");
        let enrollment_request_id = authorize_enrollment_peer(
            host.clone(),
            host_identity.clone(),
            coordinator_identity.clone(),
            &coordinator_peer,
            &coordinator_ticket,
        )
        .await;
        let (authority_owner, authority) = enrollment_authority_channel();
        let authority_cancel = CancellationToken::new();
        let authority_task = tokio::spawn(run_enrollment_reconciler(
            host.clone(),
            host_identity.clone(),
            authority_owner,
            authority_cancel.clone(),
        ));
        host_admin
            .add_replicator(
                &coordinator_addresses,
                &collections,
                &PairingFilters::default(),
            )
            .await
            .expect("authorize coordinator route");
        let filters = scope_filter(
            &template.scope,
            template.collections,
            &host_did,
            &coordinator_did,
        );
        coordinator_admin
            .add_replicator(&host_addresses, &collections, &filters)
            .await
            .expect("install addressed-request coordinator route");
        wait_for_value(&host, "AgentRequest", "request_id", &fresh_request_id).await;
        wait_for_value(&host, "AgentRequest", "request_id", &revoked_request_id).await;
        assert_eq!(
            collection_values(&host, "AgentRequest", "request_id").await,
            BTreeSet::from([fresh_request_id.clone(), revoked_request_id.clone()]),
            "only requests addressed to the host replicate; the caller's own request stays home"
        );
        assert!(
            collection_values(&host, "AgentToolCall", "tool_call_id")
                .await
                .is_empty(),
            "the caller's tool rows never replicate to the host"
        );

        let behavior = test_agent(host_identity.clone());
        let fresh_calls = Arc::new(AtomicUsize::new(0));
        let mut fresh_daemon = behavior_daemon(
            host.clone(),
            behavior.clone(),
            authority.clone(),
            fresh_calls.clone(),
        );
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let fresh = load_admission_request_by_id(&host, &fresh_request_id).await;
        fresh_daemon
            .process_request(fresh, shutdown_rx.clone())
            .await
            .unwrap();
        let fresh_state = host
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ lifecycle_state failure_reason error_message terminal_output }} }}"#,
                escape_graphql_string(&fresh_request_id),
            ))
            .await;
        assert!(
            fresh_calls.load(Ordering::SeqCst) > 0,
            "fresh Peer request did not reach the provider: {fresh_state:?}"
        );

        GraphqlEnrollmentStore::new(host.clone(), host_identity.clone())
            .revoke_request(&enrollment_request_id)
            .await
            .expect("durably revoke signed enrollment generation");
        let revoked_calls = Arc::new(AtomicUsize::new(0));
        let mut revoked_daemon =
            behavior_daemon(host.clone(), behavior, authority, revoked_calls.clone());
        let revoked = load_admission_request_by_id(&host, &revoked_request_id).await;
        revoked_daemon
            .process_request(revoked, shutdown_rx)
            .await
            .unwrap();
        assert_eq!(
            revoked_calls.load(Ordering::SeqCst),
            0,
            "revoked pending Peer request reached the provider"
        );
        let rejected = host
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    lifecycle_state claimed_at
                }} }}"#,
                escape_graphql_string(&revoked_request_id),
            ))
            .await;
        let rejected = rejected
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .expect("terminal denied child");
        assert_eq!(
            rejected
                .get("lifecycle_state")
                .and_then(serde_json::Value::as_str),
            Some("failed")
        );
        assert!(rejected
            .get("claimed_at")
            .is_none_or(serde_json::Value::is_null));

        authority_cancel.cancel();
        authority_task
            .await
            .expect("join enrollment authority")
            .expect("enrollment authority shutdown");
        coordinator.shutdown().await;
        host.shutdown().await;
    }

    #[tokio::test]
    async fn embedded_agent_target_host_replays_only_return_projection() {
        let sender_test = runtime_test_node().await;
        let receiver_test = runtime_test_node().await;
        let sender = Arc::clone(&sender_test.node);
        let receiver = Arc::clone(&receiver_test.node);
        let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&sender));
        let receiver_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&receiver));
        let sender_addresses = wait_for_peer_info(&sender_admin).await;
        let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
        let (match_request_id, match_session_id, match_header_doc_id, match_sequence) =
            seed_agent_target_return_artifacts(&sender, "match", Some("did:key:coord")).await;
        let (unrelated_request_id, _unrelated_session_id, _unrelated_header, _unrelated_sequence) =
            seed_agent_target_return_artifacts(&sender, "unrelated", None).await;
        let (foreign_request_id, _foreign_session_id, _foreign_header, _foreign_sequence) =
            seed_agent_target_return_artifacts(&sender, "foreign", Some("did:key:other")).await;
        let template =
            resolve_template(AGENT_TARGET_HOST_TEMPLATE).expect("agent-target-host template");
        let collections = template
            .collections
            .iter()
            .map(|collection| (*collection).to_string())
            .collect::<Vec<_>>();
        sender_admin
            .connect(&receiver_addresses)
            .await
            .expect("connect sender to receiver");
        wait_for_active_peer(&sender_admin).await;
        wait_for_active_peer(&receiver_admin).await;
        receiver_admin
            .add_replicator(&sender_addresses, &collections, &PairingFilters::default())
            .await
            .expect("authorize only the agent-target return projection");
        let filters = scope_filter(
            &template.scope,
            template.collections,
            "did:key:coord",
            "did:key:host",
        );
        sender_admin
            .add_replicator(&receiver_addresses, &collections, &filters)
            .await
            .expect("install requester-scoped return replicator");
        wait_for_value(&receiver, "AgentRequest", "request_id", &match_request_id).await;
        wait_for_value(&receiver, "AgentMessage", "_docID", &match_header_doc_id).await;
        assert_eq!(
            collection_values(&receiver, "AgentRequest", "request_id").await,
            BTreeSet::from([match_request_id.clone()]),
            "only the requester-routed return request must replicate"
        );
        let sender_terminal = sender . execute (& format ! (r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID lifecycle_state terminal_output }} }}"# , escape_graphql_string (& match_request_id) ,)) . await ;
        let sender_terminal_row = &sender_terminal.data.as_ref().unwrap()["AgentRequest"][0];
        let match_request_doc_id = sender_terminal_row["_docID"]
            .as_str()
            .expect("sender request doc id")
            .to_string();
        assert_eq!(sender_terminal_row["lifecycle_state"], "completed");
        assert_eq!(
            sender_terminal_row["terminal_output"],
            serde_json::to_value(gents_protocol::output::TerminalOutput::Message {
                message_doc_id: match_header_doc_id.clone(),
            })
            .unwrap()
        );
        let receiver_message_rows = crate :: session :: canonical_rows :: decode_transcript_message_row (& receiver . execute (& format ! (r#"{{ AgentMessage(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {} }} }}"# , escape_graphql_string (& match_header_doc_id) , crate :: session :: canonical_rows :: AGENT_MESSAGE_FIELDS ,)) . await . data . as_ref () . and_then (| data | data . get ("AgentMessage")) . and_then (| rows | rows . as_array ()) . and_then (| rows | rows . first ()) . cloned () . expect ("replicated canonical AgentMessage") ,) . expect ("decode replicated canonical AgentMessage") ;
        assert_eq!(
            receiver_message_rows.doc_id, match_header_doc_id,
            "AgentMessage replication must preserve the physical header identity"
        );
        assert_eq!(receiver_message_rows.message.session_id, match_session_id);
        assert_eq!(receiver_message_rows.message.sequence, match_sequence);
        wait_for_value(
            &receiver,
            "AgentOutputSegment",
            "request_doc_id",
            &match_request_doc_id,
        )
        .await;
        assert_eq!(
            collection_values(&receiver, "AgentOutputSegment", "request_doc_id").await,
            BTreeSet::from([match_request_doc_id.clone()]),
            "only the routed return projection's segments must replicate"
        );
        let agent_response_rows = receiver.execute("{ AgentResponse { response_key } }").await;
        assert!(
            agent_response_rows.has_errors(),
            "AgentResponse is a retired collection: receiver queries must reject it"
        );
        let mut projected = None;
        for _ in 0..120 {
            match crate::session::load_canonical_message_from_node(
                &receiver,
                &match_header_doc_id,
                "did:key:host",
                Some("did:key:coord"),
            )
            .await
            {
                Ok((_, message)) => {
                    projected = Some(message);
                    break;
                }
                Err(error)
                    if error
                        .downcast_ref::<gents_protocol::output::ReconstructionError>()
                        .is_some_and(|error| error.is_incomplete()) =>
                {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                Err(error) => {
                    panic!("reconstructing projected child result failed: {error:#}");
                }
            }
        }
        assert_eq!(
            projected.expect("projected child result reconstructed"),
            gents_protocol::message::Message::assistant("child result"),
            "receiver must project the exact child result"
        );
        assert!(
            !collection_values(&receiver, "AgentRequest", "request_id")
                .await
                .contains(&unrelated_request_id),
            "unrouted host-local artifact leaked to the receiver"
        );
        assert!(
            !collection_values(&receiver, "AgentRequest", "request_id")
                .await
                .contains(&foreign_request_id),
            "foreign-requester artifact leaked to the receiver"
        );
        wait_for_value(&receiver, "AgentSession", "session_id", &match_session_id).await;
        assert_eq!(
            collection_values(&receiver, "AgentSession", "session_id").await,
            BTreeSet::from([match_session_id.clone()]),
            "only the requester's caused session crosses the return leg"
        );
        sender.shutdown().await;
        receiver.shutdown().await;
    }

    #[tokio::test]
    async fn applied_pairing_load_finds_the_twin_a_merge_left_out_of_the_unique_index() {
        use crate::agent::p2p_reconcile::{GraphqlPairingStateStore, PairingStateStore};

        for clear in [false, true] {
            let identity_temp = tempfile::tempdir().expect("identity tempdir");
            let identity: Arc<dyn NodeIdentity> = Arc::new(
                KeyIdentity::load_or_create(identity_temp.path().join("node.key"), None)
                    .expect("identity"),
            );
            let sender_test = runtime_test_node().await;
            let receiver_test = runtime_test_node().await;
            let sender = Arc::clone(&sender_test.node);
            let receiver = Arc::clone(&receiver_test.node);
            let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&sender));
            let receiver_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&receiver));
            let sender_addresses = wait_for_peer_info(&sender_admin).await;
            let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
            for (node, address) in [(&sender, "a"), (&receiver, "b")] {
                exec(
                node,
                &format!(
                    r#"mutation {{ create_PeerPairingApplied(input: {{ peer_id: "peer-x", replicator_addresses: ["{address}"] }}) {{ _docID }} }}"#
                ),
                "seed applied pairing",
            )
            .await;
            }
            let mut conflicts = receiver.subscribe(&[events::EventName::UniqueIndexConflict]);
            let collections = vec!["PeerPairingApplied".to_string()];
            sender_admin.connect(&receiver_addresses).await.unwrap();
            wait_for_active_peer(&sender_admin).await;
            receiver_admin
                .add_replicator(&sender_addresses, &collections, &PairingFilters::default())
                .await
                .unwrap();
            sender_admin
                .add_replicator(
                    &receiver_addresses,
                    &collections,
                    &PairingFilters::default(),
                )
                .await
                .unwrap();
            let message = tokio::time::timeout(Duration::from_secs(30), conflicts.recv())
                .await
                .expect("unique-index conflict")
                .unwrap();
            let conflict = message.as_unique_index_conflict().unwrap().clone();

            let store =
                GraphqlPairingStateStore::for_explicit_desired(Arc::clone(&receiver), identity);
            let mut loaded = store.load_applied("peer-x").await.unwrap();
            assert_eq!(
                loaded.duplicate_doc_ids,
                vec![conflict.unindexed_doc_id.clone()]
            );
            exec(
            &receiver,
            r#"mutation { create_PeerPairingApplied(input: { peer_id: "peer-y", replicator_addresses: ["other"] }) { _docID } }"#,
            "seed another applied peer",
        ).await;
            let peers = BTreeSet::from([
                "peer-x".to_owned(),
                "peer-y".to_owned(),
                "missing".to_owned(),
            ]);
            let batch = store.load_applied_many(&peers).await.unwrap();
            assert_eq!(batch.len(), 2);
            assert_eq!(batch["peer-x"], loaded);
            assert!(batch["peer-y"].duplicate_doc_ids.is_empty());
            assert_eq!(
                batch["peer-y"].state.replicator_addresses,
                BTreeSet::from(["other".to_owned()])
            );
            exec(
            &receiver,
            r#"mutation { delete_PeerPairingApplied(filter: { peer_id: { _eq: "peer-y" } }) { _docID } }"#,
            "remove other applied peer",
        ).await;
            if clear {
                loaded.state = Default::default();
            }
            store.persist_applied("peer-x", &loaded).await.unwrap();
            let fresh = store.load_applied_many(&peers).await.unwrap();
            assert_eq!(fresh.len(), usize::from(!clear));
            if !clear {
                assert!(fresh["peer-x"].duplicate_doc_ids.is_empty());
            }
            let remaining = receiver
                .execute(r#"{ PeerPairingApplied { _docID } }"#)
                .await
                .data
                .unwrap();
            assert_eq!(
                remaining["PeerPairingApplied"],
                if clear {
                    serde_json::json!([])
                } else {
                    serde_json::json!([{ "_docID": conflict.winner_doc_id }])
                }
            );

            sender.shutdown().await;
            receiver.shutdown().await;
        }
    }
}
