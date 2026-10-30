use super::fixtures::{
    collection_id, collection_values, runtime_test_node, test_node, wait_for_active_peer,
    wait_for_peer_info, wait_for_value,
};
use super::seeding::{seed_agent_request, seed_agent_tool_call};
use super::*;

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
