#[tokio::test]
async fn applied_state_persist_collapses_duplicates_and_recovers_a_missing_row() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_collection(
        r#"
        type PeerPairingApplied {
            peer_id: String
            collections: [String!]
            replicator_addresses: [String!]
            replicator_filter: String
            enrollment_request_digest: String
            enrollment_authorization_sequence: Int
            enrollment_authorization_expires_at: DateTime
            created_at: DateTime
            updated_at: DateTime
        }
        "#,
    )
    .await
    .unwrap();
    for (address, timestamp) in [
        ("old-a", "2026-08-19T00:00:00Z"),
        ("old-b", "2026-08-19T00:01:00Z"),
    ] {
        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_PeerPairingApplied(input: {{
                        peer_id: "peer-a",
                        replicator_addresses: ["{address}"],
                        created_at: "{timestamp}",
                        updated_at: "{timestamp}"
                    }}) {{ _docID }}
                }}"#
            ))
            .await;
        assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
    }

    let tempdir = tempfile::tempdir().unwrap();
    let identity = Arc::new(
        crate::identity::KeyIdentity::load_or_create(tempdir.path().join("agent.key"), None)
            .unwrap(),
    );
    let store = GraphqlPairingStateStore::for_explicit_desired(node.clone(), identity);
    let mut applied = store.load_applied("peer-a").await.expect("load duplicates");
    applied.state = PairingApplied {
        replicator_addresses: set(&["fresh"]),
        ..Default::default()
    };
    store
        .persist_applied("peer-a", &applied)
        .await
        .expect("duplicate-tolerant save");

    let response = node
        .execute(
            r#"{
                PeerPairingApplied(filter: { peer_id: { _eq: "peer-a" } }) {
                    _docID
                    replicator_addresses
                }
            }"#,
        )
        .await;
    let mut persisted_rows = rows::<AppliedStateRow>(&response, "PeerPairingApplied").unwrap();
    persisted_rows.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
    assert_eq!(persisted_rows.len(), 1);
    assert_eq!(
        persisted_rows[0].replicator_addresses,
        Some(vec!["fresh".into()])
    );

    let mut applied = store.load_applied("peer-a").await.expect("reload applied");
    let response = node
        .execute(
            r#"mutation {
                delete_PeerPairingApplied(filter: { peer_id: { _eq: "peer-a" } }) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "delete failed: {:?}",
        response.errors
    );
    applied.state.replicator_addresses = set(&["recovered"]);
    store
        .persist_applied("peer-a", &applied)
        .await
        .expect("save after concurrent delete");

    let response = node
        .execute(
            r#"{
                PeerPairingApplied(filter: { peer_id: { _eq: "peer-a" } }) {
                    _docID
                    replicator_addresses
                }
            }"#,
        )
        .await;
    let rows = rows::<AppliedStateRow>(&response, "PeerPairingApplied").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].replicator_addresses, Some(vec!["recovered".into()]));
}

#[test]
fn nullable_graphql_arrays_emit_null_when_empty() {
    assert_eq!(graphql_nullable_string_array(&BTreeSet::new()), "null");
    assert_eq!(
        graphql_nullable_string_array(&set(&["a", "b"])),
        r#"["a", "b"]"#
    );
}
