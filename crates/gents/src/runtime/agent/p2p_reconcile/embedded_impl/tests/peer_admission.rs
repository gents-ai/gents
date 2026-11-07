use super::fixtures::{
    authorize_enrollment_peer, collection_values, runtime_test_node_with_identity,
    wait_for_active_peer, wait_for_peer_info,
};
use super::seeding::{seed_agent_request, seed_agent_tool_call};
use super::*;

/// Enrollment authorizes a peer for discovery and enrollment only. An approved
/// member still cannot route addressed requests or tool rows over P2P: the
/// agent-target-caller scope does not resolve, and both sides refuse request
/// collections before the transport.
#[tokio::test]
async fn approved_enrollment_does_not_open_peer_request_replication() {
    assert!(
        resolve_template(AGENT_TARGET_CALLER_TEMPLATE).is_none(),
        "the agent-target-caller template must not resolve to a replication scope"
    );
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
    let host_did = host_identity.did().to_owned();
    let coordinator_did = coordinator_identity.did().to_owned();

    seed_agent_request(&coordinator, "addressed-request", &host_did).await;
    seed_agent_tool_call(&coordinator, "addressed-tool", &host_did).await;
    coordinator_admin
        .connect(&host_addresses)
        .await
        .expect("connect host");
    wait_for_active_peer(&coordinator_admin).await;
    wait_for_active_peer(&host_admin).await;

    // Enrollment publication requires the host's own Node, owned by its
    // runtime identity.
    crate::test_support::ensure_test_node(&host, &host_did).await;
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
    authorize_enrollment_peer(
        host.clone(),
        host_identity.clone(),
        coordinator_identity.clone(),
        &coordinator_peer,
        &coordinator_ticket,
    )
    .await;

    let collections = vec!["AgentRequest".to_owned(), "AgentToolCall".to_owned()];
    let mut filters = PairingFilters::new();
    filters.insert(
        "AgentRequest".to_owned(),
        equality_filter("node_did", &host_did),
    );
    filters.insert(
        "AgentToolCall".to_owned(),
        equality_filter("requester_did", &coordinator_did),
    );
    for (admin, addresses) in [
        (&host_admin, &coordinator_addresses),
        (&coordinator_admin, &host_addresses),
    ] {
        let refused = admin
            .add_replicator(addresses, &collections, &filters)
            .await;
        assert!(
            matches!(
                refused,
                Err(RemoteP2pAdminError::RuntimeLocalCollection(ref name)) if name == "AgentRequest"
            ),
            "{refused:?}"
        );
        assert!(admin
            .list_p2p_collections()
            .await
            .expect("list P2P subscriptions")
            .is_empty());
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        collection_values(&host, "AgentRequest", "request_id")
            .await
            .is_empty(),
        "an addressed request must not replicate to the enrolled host"
    );
    assert!(
        collection_values(&host, "AgentToolCall", "tool_call_id")
            .await
            .is_empty(),
        "the caller's tool rows must not replicate to the enrolled host"
    );
    coordinator.shutdown().await.expect("node shutdown");
    host.shutdown().await.expect("node shutdown");
}
