#[test]
fn data_plane_gate_accepts_current_enrollment_endpoint() {
    let enrollment_entries = vec![EnrollmentEndpointEntry {
        peer_id: "peer-network".to_string(),
        node_did: "did:key:network".to_string(),
        address: "/ticket/network".to_string(),
        desired_id: "peer-network".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    }];

    let entry = materialized_enrollment_entry(&enrollment_entries, "peer-network", "did:key:self")
        .expect("enrollment endpoint should pass gate");

    assert_eq!(entry.address, "/ticket/network");
}

#[test]
fn enrollment_base_and_local_data_plane_have_disjoint_owners() {
    let enrollment_entries = vec![EnrollmentEndpointEntry {
        peer_id: "peer-network".to_string(),
        node_did: "did:key:network".to_string(),
        address: "/ticket/network".to_string(),
        desired_id: "peer-network".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    }];
    let entry = materialized_enrollment_entry(&enrollment_entries, "peer-network", "did:key:self")
        .expect("enrollment endpoint should pass gate");
    assert!(enrollment_base_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            enrollment_request_digest: Some("digest".to_string()),
            enrollment_authorization_sequence: Some(1),
            enrollment_authorization_expires_at: Some("2099-09-29T00:00:00Z".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_some());
    assert!(enrollment_base_row(
        PairingStateRow {
            source: Some("operator".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_none());
    assert!(enrollment_base_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_none());
    assert!(enrollment_base_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            enrollment_request_digest: Some("digest".to_string()),
            enrollment_authorization_sequence: Some(2),
            enrollment_authorization_expires_at: Some("2099-09-29T00:00:00Z".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_none());
    assert!(local_data_plane_row(
        PairingStateRow {
            source: Some("operator".to_string()),
            template: Some("app-collections".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_some());
    assert!(local_data_plane_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .is_none());

    let canonical = enrollment_base_row(
        PairingStateRow {
            source: Some("enrollment".to_string()),
            enrollment_request_digest: Some("digest".to_string()),
            enrollment_authorization_sequence: Some(1),
            enrollment_authorization_expires_at: Some("2099-09-29T00:00:00Z".to_string()),
            node_did: Some("did:key:attacker".to_string()),
            template: Some("app-collections".to_string()),
            collections: Some(vec!["HostileCollection".to_string()]),
            replicator_addresses: Some(vec!["/ticket/attacker".to_string()]),
            ..Default::default()
        },
        &entry,
        "did:key:self",
    )
    .expect("the enrollment owner matches");
    assert_eq!(canonical.node_did.as_deref(), Some("did:key:self"));
    assert_eq!(canonical.template.as_deref(), Some("client"));
    assert_eq!(canonical.collections, None);
    assert_eq!(
        canonical.replicator_addresses,
        Some(vec!["/ticket/network".to_string()])
    );
}

#[test]
fn data_plane_gate_rejects_self_endpoint_from_both_sources() {
    let enrollment_entries = vec![EnrollmentEndpointEntry {
        peer_id: "peer-self".to_string(),
        node_did: "did:key:self".to_string(),
        address: "/ticket/self".to_string(),
        desired_id: "peer-self".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    }];

    let entry = materialized_enrollment_entry(&enrollment_entries, "peer-self", "did:key:self");

    assert!(entry.is_none());
}

#[test]
fn data_plane_desired_uses_signed_endpoint_address_and_requester_did() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        node_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            node_did: None,
            collections: None,
            replicator_addresses: Some(vec!["/ip4/192.0.2.1/tcp/9999/p2p/forged".to_string()]),
            template: Some("conversation".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect("data-plane desired")
    .expect("some data-plane layer");

    assert_eq!(
        desired.replicator_addresses,
        set(&["/ip4/127.0.0.1/tcp/4001/p2p/peer-b"])
    );
    assert_eq!(
        desired
            .replicator_filter
            .get("AgentRequest")
            .and_then(single_string_eq),
        Some(("requester_did", "did:key:peer-b"))
    );
}

#[test]
fn enrolled_client_outbound_route_preserves_directional_authority() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "runtime-peer".to_string(),
        node_did: "did:key:runtime".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/runtime-peer".to_string(),
        desired_id: "runtime-peer:client-to-runtime".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            peer_id: Some("runtime-peer:client-to-runtime".to_string()),
            node_did: Some("did:key:phone".to_string()),
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("client".to_string()),
            source: Some("enrollment".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:phone",
    )
    .expect("client data-plane desired")
    .expect("some client data-plane layer");

    assert!(desired.replicator_collections.contains("AgentRequest"));
    assert!(!desired.replicator_collections.contains("Agent"));
    let encoded = serde_json::to_string(
        desired
            .replicator_filter
            .get("AgentRequest")
            .expect("request filter"),
    )
    .unwrap();
    assert!(encoded.contains("requester_did"));
    assert!(encoded.contains("did:key:phone"));
    assert!(encoded.contains("node_did"));
    assert!(encoded.contains("did:key:runtime"));
}

#[test]
fn enrollment_owned_base_route_has_explicit_runtime_to_client_direction() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "phone-peer".to_string(),
        node_did: "did:key:phone".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/phone-peer".to_string(),
        desired_id: "phone-peer".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            peer_id: Some("phone-peer".to_string()),
            node_did: Some("did:key:runtime".to_string()),
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("client".to_string()),
            source: Some("enrollment".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:runtime",
    )
    .expect("enrollment-owned client route")
    .expect("some enrollment base layer");

    assert!(desired.replicator_collections.contains("Agent"));
    let encoded = serde_json::to_string(
        desired
            .replicator_filter
            .get("AgentRequest")
            .expect("request filter"),
    )
    .unwrap();
    assert!(encoded.contains("requester_did"));
    assert!(encoded.contains("did:key:phone"));
    assert!(encoded.contains("node_did"));
    assert!(encoded.contains("did:key:runtime"));
}

#[tokio::test]
async fn committed_revocation_between_desired_load_and_apply_tears_down_stale_route() {
    let store = enrolled_live_route_store([Ok(true), Ok(false)]);
    let admin = admin_with_live_enrolled_route();

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("revoked route reconciles fail closed");

    assert_eq!(
        outcome.ops_applied,
        vec![DiffOp::TeardownReplicator("addr1".into())]
    );
    assert!(admin.connects.lock().unwrap().is_empty());
    assert!(admin.replicators.lock().unwrap().is_empty());
    assert!(store.applied.lock().unwrap().is_empty());
    let saved = store.saved.lock().unwrap();
    assert!(!saved[0].state.is_empty());
    assert_eq!(saved[0].enrollment, EnrollmentMaterialization::default());
}

#[tokio::test]
async fn enrollment_projection_read_failure_tears_down_instead_of_preserving_live_route() {
    let store = enrolled_live_route_store([Err("projection unavailable".into())]);
    let admin = admin_with_live_enrolled_route();

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("unknown enrollment authority closes the route");

    assert!(!outcome.desired_read_failed);
    assert_eq!(
        outcome.ops_applied,
        vec![DiffOp::TeardownReplicator("addr1".into())]
    );
    assert!(admin.connects.lock().unwrap().is_empty());
    assert!(admin.replicators.lock().unwrap().is_empty());
    assert!(store.applied.lock().unwrap().is_empty());
    let saved = store.saved.lock().unwrap();
    assert!(!saved[0].state.is_empty());
    assert_eq!(saved[0].enrollment, EnrollmentMaterialization::default());
}

#[tokio::test]
async fn converged_route_records_current_enrollment_without_reinstalling() {
    let store = enrolled_live_route_store([Ok(true), Ok(true), Ok(true), Ok(true)]);
    *store.materialization.lock().unwrap() = Default::default();
    let admin = admin_with_live_enrolled_route();
    let outcome = reconcile_peer_tick(&admin, &store, "peer-a").await.unwrap();
    assert!(outcome.ops_applied.is_empty());
    assert_eq!(
        *store.materialization.lock().unwrap(),
        EnrollmentMaterialization::from(&store.generation)
    );
    assert_eq!(store.saved.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn converged_route_cannot_record_an_authorization_lost_before_publication() {
    let store = enrolled_live_route_store([Ok(true), Ok(true), Ok(true), Ok(false)]);
    let admin = admin_with_live_enrolled_route();
    let outcome = reconcile_peer_tick(&admin, &store, "peer-a").await.unwrap();
    assert!(outcome.ops_applied.is_empty());
    assert_eq!(
        *store.materialization.lock().unwrap(),
        EnrollmentMaterialization::default()
    );
}
