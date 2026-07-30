use crate::support::test_db;

#[tokio::test]
async fn peer_pairing_desired_round_trip() {
    let db = test_db("peer-pairing-desired").await;
    let node = db.node;

    let create = r#"mutation {
        create_PeerPairingDesired(input: {
            peer_id: "p1",
            agent_did: "did:test:p1",
            collections: ["c1", "c2"],
            replicator_addresses: ["/ip4/1/p2p/p1"],
            created_at: "2026-05-13T00:00:00Z",
            updated_at: "2026-05-13T00:00:00Z"
        }) { peer_id collections }
    }"#;
    let result = node.execute(create).await;
    assert!(!result.has_errors(), "create errors: {:?}", result.errors);

    let query = r#"query {
        PeerPairingDesired(filter: { peer_id: { _eq: "p1" } }) {
            peer_id
            agent_did
            collections
            replicator_addresses
        }
    }"#;
    let result = node.execute(query).await;
    assert!(!result.has_errors(), "query errors: {:?}", result.errors);
    let data = result.data.expect("data");
    assert!(data.to_string().contains("c1"));
}
