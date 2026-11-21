use super::*;

const TEST_SCHEMA: &str = r#"
    type P2pReconcileThing {
        name: String
    }
"#;

pub(in crate::p2p_reconcile) struct TestNode {
    pub(in crate::p2p_reconcile) node: Arc<EmbeddedNode>,
    pub(super) _tempdir: tempfile::TempDir,
}

struct NoToolSources;

pub(super) fn no_tool_sources() -> Arc<dyn gents_store::local::ToolSourcePolicy> {
    Arc::new(NoToolSources)
}

impl gents_store::local::ToolSourcePolicy for NoToolSources {
    fn node_operation_sources(
        &self,
        _tools: &serde_json::Value,
        _surfaces: &[serde_json::Value],
    ) -> anyhow::Result<Vec<gents_store::local::ProjectedOperationSource>> {
        Ok(Vec::new())
    }

    fn plugin_refs(
        &self,
        _tools: &serde_json::Value,
    ) -> anyhow::Result<Vec<gents_store::local::ProjectedPluginRef>> {
        Ok(Vec::new())
    }

    fn selected_tools(&self, _node: &str, _context: &serde_json::Value) -> Option<String> {
        None
    }
}

async fn p2p_node_with_identity(node_identity_did: Option<&str>) -> TestNode {
    p2p_node_with_options(node_identity_did, None, None, None).await
}

async fn p2p_node_with_options(
    node_identity_did: Option<&str>,
    http_address: Option<std::net::SocketAddr>,
    signing_key: Option<defra_node::SigningKey>,
    projector: Option<std::sync::Arc<dyn NodeIdentity>>,
) -> TestNode {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let mut builder = EmbeddedNode::builder()
        .data_path(tempdir.path())
        .with_p2p(P2PConfig {
            port: Some(0),
            bind_addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            relay_mode: IrohRelayModeConfig::Disabled,
            discovery: IrohDiscoveryConfig::Disabled,
            allowlist: p2p::iroh::IrohAllowlistConfig::AcceptAll,
            max_concurrent_multipath_paths: None,
            secret_key_path: None,
            load_persisted_collections: false,
            sync: Default::default(),
            peer_key: Default::default(),
            rebroadcast_on_merge: false,
        });
    if let Some(did) = node_identity_did {
        builder = builder.with_node_identity_did(did);
    }
    if let Some(key) = signing_key {
        builder = builder.with_signing_key(key).with_node_acp_enabled();
    }
    if let Some(address) = http_address {
        builder = builder.with_http(defra_node::HttpConfig::with_addr(address));
    }
    if let Some(identity) = projector {
        builder = builder.with_document_projector(
            gents_store::local::document_projector(
                identity,
                no_tool_sources(),
                gents_protocol::schemas::HomePolicyIds::node_store(),
            )
            .expect("document projector"),
        );
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

pub(super) async fn test_node() -> TestNode {
    let test = p2p_node().await;
    test.node
        .add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(TEST_SCHEMA))
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

pub(super) async fn runtime_test_node() -> TestNode {
    let directory = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    runtime_test_node_with_identity(&identity).await
}

pub(super) async fn runtime_test_node_with_identity(identity: &dyn NodeIdentity) -> TestNode {
    let test = p2p_node_with_identity(Some(identity.did())).await;
    ensure_runtime_schemas(&test.node)
        .await
        .expect("runtime schemas");
    test
}

pub(super) async fn runtime_test_node_with_signing_key(
    identity: &dyn NodeIdentity,
    key: Vec<u8>,
) -> TestNode {
    let test = p2p_node_with_options(
        Some(identity.did()),
        None,
        Some(defra_node::SigningKey::Ed25519(key)),
        None,
    )
    .await;
    ensure_runtime_schemas(&test.node)
        .await
        .expect("runtime schemas");
    test
}

pub(super) async fn signed_p2p_node_without_schemas(
    identity: &dyn NodeIdentity,
    key: Vec<u8>,
    address: std::net::SocketAddr,
) -> TestNode {
    p2p_node_with_options(
        Some(identity.did()),
        Some(address),
        Some(defra_node::SigningKey::Ed25519(key)),
        None,
    )
    .await
}

pub(super) async fn signed_p2p_node_with_projector(
    identity: Arc<dyn NodeIdentity>,
    key: Vec<u8>,
    address: std::net::SocketAddr,
) -> TestNode {
    p2p_node_with_options(
        Some(identity.did()),
        Some(address),
        Some(defra_node::SigningKey::Ed25519(key)),
        Some(identity.clone()),
    )
    .await
}

pub(in crate::p2p_reconcile) async fn runtime_http_test_node(
    identity: &dyn NodeIdentity,
) -> (TestNode, String) {
    runtime_http_test_node_with_schema(identity, None).await
}

pub(in crate::p2p_reconcile) async fn runtime_http_test_node_with_schema(
    identity: &dyn NodeIdentity,
    initial_schema: Option<&str>,
) -> (TestNode, String) {
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let test = p2p_node_with_options(Some(identity.did()), Some(address), None, None).await;
    if let Some(schema) = initial_schema {
        test.node
            .add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(schema))
            .await
            .unwrap();
    }
    ensure_runtime_schemas(&test.node).await.unwrap();
    let endpoint = format!("http://{address}/api/v0/graphql");
    let access = gents_store::config_client::ConfigAccess::graphql(&endpoint);
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

pub(in crate::p2p_reconcile) async fn wait_for_peer_info(
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

pub(super) fn collection_id(node: &EmbeddedNode, name: &str) -> String {
    node.get_collection(name)
        .expect("collection lookup")
        .expect("collection")
        .collection_id
}

pub(in crate::p2p_reconcile) async fn wait_for_active_peer(admin: &EmbeddedRemoteP2pAdmin) {
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

pub(super) async fn exec(node: &EmbeddedNode, statement: &str, context: &str) {
    let response = node.execute(statement).await;
    assert!(
        !response.has_errors(),
        "{context} failed: {:?}\n{statement}",
        response.errors
    );
}

pub(super) async fn collection_values(
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
