use crate::support::enrollment::wait_for_peer_identity;
use crate::support::test_p2p_db;

#[tokio::test]
async fn p2p_crash_reopens_same_durable_peer_identity() {
    let mut db = test_p2p_db("p2p-crash-peer-identity").await;
    let (before_peer, _) = wait_for_peer_identity(db.node.as_ref()).await;
    db.simulate_process_crash()
        .await
        .expect("reopen P2P store after crash");
    let (after_peer, _) = wait_for_peer_identity(db.node.as_ref()).await;
    assert_eq!(
        before_peer, after_peer,
        "durable peer identity changed on restart"
    );
    assert_eq!(db.process_generation, 1);
}
