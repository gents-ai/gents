#[test]
fn self_pairing_row_is_not_materialized() {
    let base = PairingDesired {
        replicator_addresses: set(&["iroh-ticket"]),
        template_ids: set(&["machine"]),
        ..Default::default()
    };

    assert!(merge_layered_desired("did:key:self", "did:key:self", Some(base), None).is_none());
}

#[test]
fn merge_desired_unions_control_and_data_plane_state() {
    let control = PairingDesired {
        replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
        replicator_collections: set(&["ControlA", "ControlB"]),
        replicator_filter: PairingFilters::new(),
        template_ids: BTreeSet::new(),
    };
    let data = PairingDesired {
        replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: one_filter("AgentRequest", "node_did", "did:key:a"),
        template_ids: BTreeSet::new(),
    };

    let merged = merge_desired(Some(control), Some(data)).expect("merged desired");
    assert_eq!(
        merged.replicator_collections,
        set(&["ControlA", "ControlB", "AgentRequest"])
    );
    assert_eq!(
        merged.replicator_addresses,
        set(&["/ip4/1/tcp/1/p2p/peer-a"])
    );
    assert_eq!(
        merged
            .replicator_filter
            .get("AgentRequest")
            .and_then(single_string_eq),
        Some(("node_did", "did:key:a"))
    );
    assert!(!merged.replicator_filter.contains_key("ControlA"));
}

#[test]
fn data_plane_only_desired_is_replicator_only() {
    let data = PairingDesired {
        replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: one_filter("AgentRequest", "node_did", "did:key:a"),
        template_ids: BTreeSet::new(),
    };

    let merged = merge_desired(None, Some(data)).expect("data-plane desired");
    assert_eq!(merged.replicator_collections, set(&["AgentRequest"]));
    assert!(merged.replicator_filter.contains_key("AgentRequest"));
}

#[test]
fn data_plane_desired_rejects_foreign_node_did_scope() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        node_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let error = data_plane_desired_from_pairing_row(
        PairingStateRow {
            node_did: Some("did:key:someone-else".to_string()),
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("machine".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect_err("foreign data-plane scope should be rejected");

    assert!(error.to_string().contains("foreign DID"));
}

#[test]
fn data_plane_desired_machine_scopes_owned_directory() {
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
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("machine".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect("data-plane desired")
    .expect("some data-plane layer");

    let directory = crate::runtime::agent::p2p_reconcile::templates::NODE_DIRECTORY_COLLECTION;
    assert_eq!(desired.replicator_collections, set(&[directory]));
    assert_eq!(
        desired.replicator_filter.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![directory],
        "the machine template scopes only the owned directory"
    );
    assert_eq!(
        desired
            .replicator_filter
            .get(directory)
            .and_then(single_string_eq),
        Some(("source_did", "did:key:self"))
    );
}
