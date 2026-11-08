use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::Node as EmbeddedNode;
use gents::request::lifecycle::{
    build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
    RequestSigner, RequestSpec,
};
use gents::runtime::agent::p2p_reconcile::{
    expand_p2p_collection_profile_ids, EmbeddedRemoteP2pAdmin, PairingFilters, RemoteP2pAdmin,
};
use gents::store::config_client::{ConfigAccess, NodePermission};
use gents::store::graphql::escape_graphql_string;
use gents::{KeyIdentity, NodeIdentity, RequestLifecycle, TERMINAL_REDRIVE_CAP};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::support::p2p_waits::wait_for_connected_peer;
use crate::support::{test_p2p_db, TestDb};

/// The owner Node with its enrolled User and Agent; the User submits over the
/// owner's HTTP endpoint with its client key, and `peer` is a second Node
/// that may receive only discovery documents.
struct DeliveryFixture {
    _directory: tempfile::TempDir,
    owner: TestDb,
    node: Arc<EmbeddedNode>,
    node_identity: Arc<dyn NodeIdentity>,
    agent_identity: Arc<dyn NodeIdentity>,
    user: KeyIdentity,
    peer: TestDb,
    requester: ConfigAccess,
    outsider: ConfigAccess,
    agent: ConfigAccess,
}

impl DeliveryFixture {
    async fn new(name: &str) -> Self {
        let directory = tempfile::Builder::new().prefix(name).tempdir().unwrap();
        let owner =
            crate::support::test_p2p_db_without_node(&format!("{name}-owner"), Default::default())
                .await;
        let peer = test_p2p_db(&format!("{name}-peer")).await;
        let fixture = crate::support::managed_agent_fixture(&owner).await;
        let outsider =
            KeyIdentity::load_or_create(directory.path().join("outsider.key"), None).unwrap();
        // Node-level reads and updates are open to the outsider, so document
        // ACP alone is what hides and refuses the User's request.
        let admin = ConfigAccess::local_as(
            owner.node.clone(),
            owner.node_identity.did().parse().unwrap(),
        );
        for permission in [NodePermission::DocumentRead, NodePermission::DocumentUpdate] {
            admin
                .grant_node_permission(&outsider.did().parse().unwrap(), permission)
                .await
                .unwrap();
        }
        // The outsider speaks raw DID-authenticated GraphQL, outside the
        // client-signed owners, so the database's ACP is what refuses it.
        let outsider = ConfigAccess::Graphql(
            gents::store::config_client::GraphqlEndpoint::as_principal(
                owner.client_endpoint(),
                outsider.did(),
            )
            .with_client_signing_capability(outsider.client_signing_capability().unwrap())
            .unwrap(),
        );
        let agent = ConfigAccess::local_managed_agent(
            owner.node.clone(),
            fixture.agent.did().parse().unwrap(),
            fixture.agent.as_ref(),
        )
        .unwrap();
        Self {
            _directory: directory,
            node: owner.node.clone(),
            node_identity: owner.node_identity.clone(),
            agent_identity: fixture.agent.clone(),
            user: fixture.user,
            requester: fixture.access,
            owner,
            peer,
            outsider,
            agent,
        }
    }

    fn user_did(&self) -> &str {
        self.user.did()
    }

    async fn submit(&self, request_id: &str, session_id: &str) -> String {
        let spec = RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.into(),
                node_did: self.node_identity.did().into(),
                requester_did: Some(self.user_did().into()),
                agent_did: self.agent_identity.did().into(),
                session_id: session_id.into(),
                content: "convergence delivery e2e".into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            AgentRequestAdmissionRecord::client(self.user_did()),
        );
        let request =
            build_signed_client_request(&self.requester, spec, RequestSigner::Identity(&self.user))
                .await
                .unwrap();
        submit_client_request(&self.requester, &request)
            .await
            .unwrap();
        self.read(&self.requester, request_id)
            .await
            .unwrap()
            .doc_id
            .unwrap()
    }

    async fn set_state(&self, doc_id: &str, state: &str) {
        // The Agent's watcher completes the client's delegated relationships
        // before the Agent writes the request it now owns.
        let mut watcher =
            gents::DefraWatcher::new(self.node.clone(), self.node_identity.did()).unwrap();
        gents::Watcher::set_agent_identities(&mut watcher, vec![self.agent_identity.clone()])
            .unwrap();
        watcher.try_fetch_request(doc_id).await.unwrap();
        let doc_id = escape_graphql_string(doc_id);
        let state = escape_graphql_string(state);
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let response = self.agent.write("test.delivery.set_state", &format!(
            r#"mutation {{ update_AgentRequest(docID: "{doc_id}", input: {{ lifecycle_state: "{state}", terminalized_at: "{terminalized_at}", terminal_redrive_attempts: 0 }}) {{ _docID }} }}"#,
        )).await.unwrap();
        assert_eq!(
            response["data"]["update_AgentRequest"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    async fn read(&self, access: &ConfigAccess, request_id: &str) -> Option<AgentRequestRow> {
        let request_id = escape_graphql_string(request_id);
        let response = access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{ _docID request_id node_did requester_did agent_did lifecycle_state terminal_redrive_attempts }} }}"#,
        )).await.unwrap();
        response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .first()
            .cloned()
            .map(|row| serde_json::from_value(row).unwrap())
    }

    async fn redrive(&self) -> gents::TerminalRedriveReport {
        RequestLifecycle::redrive_terminal_convergence_with_identities(
            &self.node,
            self.node_identity.did(),
            std::slice::from_ref(&self.agent_identity),
        )
        .await
        .unwrap()
    }

    async fn shutdown(self) {
        self.owner.node.shutdown().await.unwrap();
        self.peer.node.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_requester_observes_owner_terminal_and_bounded_redrive() {
    let fixture = DeliveryFixture::new("convergence-http-owner").await;
    let request_id = "convergence-http-live";
    let doc = fixture.submit(request_id, "convergence-http-session").await;
    let escaped = escape_graphql_string(&doc);
    let commits = fixture.requester.execute(&format!(
        r#"{{ _commits(docID: "{escaped}", filter: {{ fieldName: {{ _eq: "_C" }} }}, order: {{ height: ASC }}, limit: 1) {{ cid }} }}"#,
    )).await.unwrap();
    let genesis = commits["data"]["_commits"][0]["cid"].as_str().unwrap();
    // The genesis block is the User's: read under its document ACP and
    // verified against its detached signature.
    let (block, signature) = fixture
        .node
        .authorized_signed_block_bytes(genesis, Some(fixture.user_did()))
        .await
        .unwrap();
    let block = defra_core::block::Block::from_dag_cbor(&block).unwrap();
    let signature = defra_core::block::Signature::from_dag_cbor(&signature).unwrap();
    assert_eq!(
        db::block::verify::verified_signature_signer_did(&block, &signature).unwrap(),
        fixture.user_did()
    );
    fixture.set_state(&doc, "processing").await;
    let processing = fixture.read(&fixture.requester, request_id).await.unwrap();
    assert_eq!(
        processing.lifecycle_state,
        Some(RequestLifecycleState::Processing)
    );
    assert_eq!(
        processing.node_did.as_deref(),
        Some(fixture.node_identity.did())
    );
    assert_eq!(
        processing.requester_did.as_deref(),
        Some(fixture.user_did())
    );
    assert_ne!(
        processing.node_did.as_deref(),
        processing.requester_did.as_deref()
    );
    assert!(fixture.read(&fixture.outsider, request_id).await.is_none());

    let doc_escaped = escape_graphql_string(&doc);
    let denied = fixture.outsider.write("test.delivery.deny", &format!(
        r#"mutation {{ update_AgentRequest(docID: "{doc_escaped}", input: {{ lifecycle_state: "failed" }}) {{ _docID }} }}"#,
    )).await.unwrap();
    assert!(denied["data"]["update_AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .read(&fixture.requester, request_id)
            .await
            .unwrap()
            .lifecycle_state,
        Some(RequestLifecycleState::Processing),
        "the outsider's update changed nothing"
    );
    fixture.set_state(&doc, "completed").await;
    let terminal = fixture.read(&fixture.requester, request_id).await.unwrap();
    assert_eq!(
        terminal.lifecycle_state,
        Some(RequestLifecycleState::Completed)
    );
    assert_eq!(terminal.doc_id.as_deref(), Some(doc.as_str()));
    assert_eq!(
        terminal.node_did.as_deref(),
        Some(fixture.node_identity.did())
    );
    assert_eq!(terminal.requester_did.as_deref(), Some(fixture.user_did()));

    for attempt in 1..=TERMINAL_REDRIVE_CAP {
        let report = fixture.redrive().await;
        assert_eq!(
            (
                report.scanned,
                report.acknowledged,
                report.reasserted,
                report.failed
            ),
            (1, 0, 1, 0)
        );
        let observed = fixture.read(&fixture.requester, request_id).await.unwrap();
        assert_eq!(
            observed.lifecycle_state,
            Some(RequestLifecycleState::Completed)
        );
        assert_eq!(observed.terminal_redrive_attempts, Some(attempt.into()));
    }
    assert!(fixture.redrive().await.is_noop());
    assert!(fixture.read(&fixture.outsider, request_id).await.is_none());
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_requester_observes_each_terminal_in_a_recovery_wave() {
    const REQUESTS: usize = 13;
    let fixture = DeliveryFixture::new("convergence-http-wave").await;
    let mut requests = Vec::new();
    for index in 0..REQUESTS {
        let request_id = format!("convergence-http-wave-{index:02}");
        let doc = fixture
            .submit(&request_id, &format!("convergence-http-session-{index:02}"))
            .await;
        fixture.set_state(&doc, "completed").await;
        requests.push((request_id, doc));
    }
    let report = fixture.redrive().await;
    assert_eq!(
        (
            report.scanned,
            report.acknowledged,
            report.reasserted,
            report.failed
        ),
        (REQUESTS, 0, REQUESTS, 0)
    );
    for (request_id, doc) in requests {
        let row = fixture.read(&fixture.requester, &request_id).await.unwrap();
        assert_eq!(row.doc_id.as_deref(), Some(doc.as_str()));
        assert_eq!(row.node_did.as_deref(), Some(fixture.node_identity.did()));
        assert_eq!(row.requester_did.as_deref(), Some(fixture.user_did()));
        assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Completed));
        assert_eq!(row.terminal_redrive_attempts, Some(1));
        assert!(fixture.read(&fixture.outsider, &request_id).await.is_none());
    }
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_replicates_while_request_documents_remain_runtime_local() {
    let fixture = DeliveryFixture::new("convergence-discovery").await;
    let request_id = "convergence-private-request";
    let doc = fixture
        .submit(request_id, "convergence-private-session")
        .await;
    fixture.set_state(&doc, "completed").await;
    assert!(expand_p2p_collection_profile_ids(["AgentRequest"], []).is_err());
    assert!(expand_p2p_collection_profile_ids(["AgentSession"], []).is_err());
    assert!(expand_p2p_collection_profile_ids([], ["discovery"]).is_ok());

    let created = ConfigAccess::local_as(fixture.node.clone(), fixture.node_identity.did().parse().unwrap())
        .write("test.discovery.probe", r#"mutation { create_PeerRegistry(input: {peer_id: "convergence-discovery-probe", node_did: "did:key:probe", addresses: ["probe"]}) { _docID } }"#)
        .await.unwrap();
    let public_doc = gents::store::graphql::created_doc_id(&created, "PeerRegistry").unwrap();
    let (_, owner_address) =
        crate::support::enrollment::wait_for_peer_identity(&fixture.node).await;
    let (_, peer_address) =
        crate::support::enrollment::wait_for_peer_identity(&fixture.peer.node).await;
    let owner_admin = EmbeddedRemoteP2pAdmin::new(fixture.node.clone());
    let peer_admin = EmbeddedRemoteP2pAdmin::new(fixture.peer.node.clone());
    peer_admin.connect(&[owner_address]).await.unwrap();
    wait_for_connected_peer(&fixture.peer.node).await;
    assert!(matches!(
        owner_admin
            .add_replicator(
                std::slice::from_ref(&peer_address),
                &["AgentRequest".into()],
                &PairingFilters::default(),
            )
            .await,
        Err(gents::runtime::agent::p2p_reconcile::RemoteP2pAdminError::RuntimeLocalCollection(_))
    ));
    owner_admin
        .add_replicator(
            &[peer_address],
            &["PeerRegistry".into()],
            &PairingFilters::default(),
        )
        .await
        .unwrap();
    fixture
        .peer
        .node
        .p2p()
        .unwrap()
        .sync_documents(
            "PeerRegistry",
            vec![public_doc.clone()],
            Some(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    let public_doc = escape_graphql_string(&public_doc);
    let public = ConfigAccess::local(fixture.peer.node.clone())
        .execute(&format!(
            r#"{{ PeerRegistry(filter: {{ _docID: {{ _eq: "{public_doc}" }} }}) {{ peer_id }} }}"#,
        ))
        .await
        .unwrap();
    assert_eq!(
        public["data"]["PeerRegistry"][0]["peer_id"],
        "convergence-discovery-probe"
    );
    fixture
        .peer
        .node
        .document_acp()
        .register_doc_object(
            &fixture.peer.node_identity.did().parse().unwrap(),
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
            "delivered_requests",
            &doc,
        )
        .await
        .unwrap();
    let escaped_doc = escape_graphql_string(&doc);
    let commits = fixture.requester.execute(&format!(
        r#"{{ _commits(docID: "{escaped_doc}", filter: {{ fieldName: {{ _eq: "_C" }} }}, order: {{ height: ASC }}, limit: 1) {{ cid }} }}"#,
    )).await.unwrap();
    let cid = commits["data"]["_commits"][0]["cid"].as_str().unwrap();
    let pull = fixture
        .peer
        .node
        .p2p()
        .unwrap()
        .sync_documents("AgentRequest", vec![doc], Some(Duration::from_secs(5)))
        .await;
    let peer_rows = ConfigAccess::local_as(
        fixture.peer.node.clone(),
        fixture.peer.node_identity.did().parse().unwrap(),
    )
    .execute("{ AgentRequest { _docID } }")
    .await
    .unwrap();
    assert!(
        fixture
            .peer
            .node
            .authorized_signed_block_bytes(cid, Some(fixture.peer.node_identity.did()))
            .await
            .is_err(),
        "peer obtained a runtime-local request block: {pull:?}"
    );
    assert!(
        peer_rows["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty(),
        "runtime-local request copied to peer: {pull:?}"
    );
    assert_eq!(
        fixture
            .read(&fixture.requester, request_id)
            .await
            .unwrap()
            .lifecycle_state,
        Some(RequestLifecycleState::Completed)
    );
    fixture.shutdown().await;
}
