fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

const TEST_TRANSPORT_PEER_ID: &str =
    "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
const TEST_TRANSPORT_ADDRESS_A: &str =
    "127.0.0.1:56091/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";

fn one_filter(collection: &str, field: &str, value: &str) -> PairingFilters {
    let mut filters = PairingFilters::new();
    filters.insert(
        collection.to_string(),
        crate::p2p_reconcile::equality_filter(field, value),
    );
    filters
}

fn enrolled_live_route_store(
    authority_checks: impl IntoIterator<Item = Result<bool, String>>,
) -> EnrollmentFenceStore {
    let generation = EnrollmentRouteGeneration {
        member_did: "did:key:member".into(),
        member_peer: "peer-a".into(),
        member_ticket: "addr1".into(),
        request_digest: "digest-1".into(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".into(),
    };
    EnrollmentFenceStore {
        desired: PairingDesired {
            replicator_addresses: set(&["addr1"]),
            replicator_collections: set(&["AgentRequest"]),
            ..Default::default()
        },
        materialization: Mutex::new(EnrollmentMaterialization::from(&generation)),
        generation,
        saved: Mutex::new(Vec::new()),
        applied: Mutex::new(PairingApplied {
            replicator_addresses: set(&["addr1"]),
            ..Default::default()
        }),
        authority_checks: Mutex::new(authority_checks.into_iter().collect()),
    }
}

fn admin_with_live_enrolled_route() -> MockAdmin {
    let admin = MockAdmin::default();
    admin.active.lock().unwrap().push("peer-a".into());
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("route-1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(Default::default()),
        },
    );
    admin
}
