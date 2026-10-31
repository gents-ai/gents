use super::fixtures::{exec, runtime_test_node, wait_for_active_peer, wait_for_peer_info};
use super::*;

#[tokio::test]
async fn applied_pairing_load_finds_the_twin_a_merge_left_out_of_the_unique_index() {
    use crate::runtime::agent::p2p_reconcile::{GraphqlPairingStateStore, PairingStateStore};

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

        let store = GraphqlPairingStateStore::for_explicit_desired(Arc::clone(&receiver), identity);
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
