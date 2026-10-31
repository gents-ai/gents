use super::*;

const TEST_SCHEMA: &str = r#"
    type P2pReconcileThing {
        name: String
    }
"#;

pub(in crate::agent::p2p_reconcile) struct TestNode {
    pub(in crate::agent::p2p_reconcile) node: Arc<EmbeddedNode>,
    _tempdir: tempfile::TempDir,
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

pub(super) async fn test_node() -> TestNode {
    let test = p2p_node().await;
    test.node
        .add_collection(TEST_SCHEMA)
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

pub(in crate::agent::p2p_reconcile) async fn runtime_http_test_node(
    identity: &dyn NodeIdentity,
) -> (TestNode, String) {
    runtime_http_test_node_with_schema(identity, None).await
}

pub(in crate::agent::p2p_reconcile) async fn runtime_http_test_node_with_schema(
    identity: &dyn NodeIdentity,
    initial_schema: Option<&str>,
) -> (TestNode, String) {
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let test = p2p_node_with_options(Some(identity.did()), Some(address)).await;
    if let Some(schema) = initial_schema {
        test.node.add_collection(schema).await.unwrap();
    }
    ensure_runtime_schemas(&test.node).await.unwrap();
    let endpoint = format!("http://{address}/api/v0/graphql");
    let access = crate::store::config_client::ConfigAccess::graphql(&endpoint);
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
    let expires_at =
        (issued + chrono::Duration::minutes(5)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
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

pub(super) fn collection_id(node: &EmbeddedNode, name: &str) -> String {
    node.get_collection(name)
        .expect("collection lookup")
        .expect("collection")
        .collection_id
}

pub(in crate::agent::p2p_reconcile) async fn wait_for_active_peer(admin: &EmbeddedRemoteP2pAdmin) {
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

pub(super) async fn wait_for_value(
    node: &EmbeddedNode,
    collection: &str,
    field: &str,
    expected: &str,
) {
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
