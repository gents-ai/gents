use super::fixtures::{collection_values, runtime_test_node, wait_for_active_peer, wait_for_peer_info};
use super::seeding::seed_agent_request;
use super::*;

/// The return projection of a routed Agent target is application data. P2P
/// carries only discovery and enrollment, so neither peer accepts a replicator
/// or a selected push for it, and no return artifact reaches the caller.
#[tokio::test]
async fn embedded_agent_target_return_projection_is_refused_before_transport() {
    assert!(
        resolve_template(AGENT_TARGET_HOST_TEMPLATE).is_none(),
        "the agent-target-host template must not resolve to a replication scope"
    );
    let sender_test = runtime_test_node().await;
    let receiver_test = runtime_test_node().await;
    let sender = Arc::clone(&sender_test.node);
    let receiver = Arc::clone(&receiver_test.node);
    let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&sender));
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&receiver));
    let sender_addresses = wait_for_peer_info(&sender_admin).await;
    let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
    seed_agent_request(&sender, "return-match-response", "did:key:coord").await;
    sender_admin
        .connect(&receiver_addresses)
        .await
        .expect("connect sender to receiver");
    wait_for_active_peer(&sender_admin).await;
    wait_for_active_peer(&receiver_admin).await;

    let mut filters = PairingFilters::new();
    filters.insert(
        "AgentRequest".to_owned(),
        equality_filter("requester_did", "did:key:coord"),
    );
    for collection in [
        "AgentRequest",
        "AgentMessage",
        "AgentOutputSegment",
        "AgentSession",
    ] {
        for (admin, addresses) in [
            (&receiver_admin, &sender_addresses),
            (&sender_admin, &receiver_addresses),
        ] {
            let refused = admin
                .add_replicator(addresses, &[collection.to_owned()], &filters)
                .await;
            assert!(
                matches!(
                    refused,
                    Err(RemoteP2pAdminError::RuntimeLocalCollection(ref name)) if name == collection
                ),
                "{collection}: {refused:?}"
            );
        }
    }
    let request_doc_id = sender
        .execute(r#"{ AgentRequest(filter: { request_id: { _eq: "return-match-response" } }) { _docID } }"#)
        .await
        .data
        .as_ref()
        .and_then(|data| data["AgentRequest"][0]["_docID"].as_str())
        .expect("seeded return request")
        .to_owned();
    let receiver_peer = receiver.p2p().unwrap().local_peer_id().await.unwrap();
    let pushed = push_documents_to_peer(
        &sender,
        &receiver_peer,
        vec![P2pDocumentRequest {
            collection: "AgentRequest".into(),
            doc_id: request_doc_id,
        }],
    )
    .await;
    assert!(pushed.is_err(), "a return request reached the P2P transport");
    for admin in [&sender_admin, &receiver_admin] {
        assert!(admin
            .list_p2p_collections()
            .await
            .expect("list P2P subscriptions")
            .is_empty());
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        collection_values(&receiver, "AgentRequest", "request_id")
            .await
            .is_empty(),
        "the routed return request must not replicate"
    );
    sender.shutdown().await.expect("node shutdown");
    receiver.shutdown().await.expect("node shutdown");
}
