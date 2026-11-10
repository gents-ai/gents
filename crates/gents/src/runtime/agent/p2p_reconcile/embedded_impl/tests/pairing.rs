use super::fixtures::{
    runtime_test_node_with_signing_key, wait_for_active_peer, wait_for_peer_info,
};
use super::*;

#[tokio::test]
async fn applied_pairing_state_remains_local_to_each_runtime() -> anyhow::Result<()> {
    use crate::runtime::agent::p2p_reconcile::{
        GraphqlPairingStateStore, LoadedPairingApplied, PairingApplied, PairingStateStore,
        RemoteP2pAdminError,
    };
    let directory = tempfile::tempdir()?;
    let left_path = directory.path().join("left");
    let right_path = directory.path().join("right");
    let left = Arc::new(KeyIdentity::load_or_create(&left_path, None)?);
    let right = Arc::new(KeyIdentity::load_or_create(&right_path, None)?);
    let left_key = crate::store::identity::load_file_identity(&left_path)?.private_key_bytes();
    let right_key = crate::store::identity::load_file_identity(&right_path)?.private_key_bytes();
    let left_test = runtime_test_node_with_signing_key(left.as_ref(), left_key).await;
    let right_test = runtime_test_node_with_signing_key(right.as_ref(), right_key).await;
    let left_store = GraphqlPairingStateStore::for_explicit_desired(left_test.node.clone(), left);
    let right_store =
        GraphqlPairingStateStore::for_explicit_desired(right_test.node.clone(), right);
    for (store, address) in [(&left_store, "left"), (&right_store, "right")] {
        let state = LoadedPairingApplied {
            state: PairingApplied {
                replicator_addresses: BTreeSet::from([address.to_owned()]),
                ..Default::default()
            },
            ..Default::default()
        };
        store.persist_applied("peer", &state).await?;
    }
    let left_admin = EmbeddedRemoteP2pAdmin::new(left_test.node.clone());
    let right_admin = EmbeddedRemoteP2pAdmin::new(right_test.node.clone());
    let right_addresses = wait_for_peer_info(&right_admin).await;
    left_admin.connect(&right_addresses).await?;
    wait_for_active_peer(&left_admin).await;
    let denied = left_admin
        .add_replicator(
            &right_addresses,
            &["PeerPairingApplied".to_owned()],
            &PairingFilters::default(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(denied, RemoteP2pAdminError::RuntimeLocalCollection(ref collection)
        if collection == "PeerPairingApplied")
    );
    assert_eq!(
        left_store
            .load_applied("peer")
            .await?
            .state
            .replicator_addresses,
        BTreeSet::from(["left".to_owned()])
    );
    assert_eq!(
        right_store
            .load_applied("peer")
            .await?
            .state
            .replicator_addresses,
        BTreeSet::from(["right".to_owned()])
    );
    left_test.node.shutdown().await?;
    right_test.node.shutdown().await?;
    Ok(())
}
