#[test]
fn self_pairing_row_is_not_materialized() {
    let base = desired_from_pairing_row(
        PairingStateRow {
            node_did: Some("did:key:self".to_string()),
            collections: None,
            replicator_addresses: Some(vec!["iroh-ticket".to_string()]),
            template: Some("machine".to_string()),
            ..Default::default()
        },
        "did:key:self",
    )
    .expect("pairing row parses");

    assert!(merge_layered_desired("did:key:self", "did:key:self", base, None).is_none());
}

#[test]
fn explicit_client_route_keeps_requester_and_owner_across_remote_admin() {
    let desired = desired_from_pairing_row(
        PairingStateRow {
            peer_id: Some("directory-a:runtime-to-client".to_string()),
            node_did: Some("did:key:mandrake".to_string()),
            replicator_addresses: Some(vec!["iroh-ticket".to_string()]),
            template: Some("client".to_string()),
            ..Default::default()
        },
        "did:key:phone",
    )
    .expect("explicit route parses")
    .expect("route materializes");
    let encoded = serde_json::to_string(
        desired
            .replicator_filter
            .get("AgentRequest")
            .expect("request filter"),
    )
    .unwrap();
    assert!(encoded.contains("did:key:phone"));
    assert!(encoded.contains("did:key:mandrake"));
}

#[test]
fn merge_desired_unions_control_and_data_plane_state() {
    let control = PairingDesired {
        collections: set(&["ControlA", "ControlB"]),
        replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
        replicator_collections: set(&["ControlA", "ControlB"]),
        replicator_filter: PairingFilters::new(),
        template_ids: BTreeSet::new(),
    };
    let data = PairingDesired {
        collections: set(&["AgentRequest"]),
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
        merged.collections,
        set(&["ControlA", "ControlB"]),
        "data-plane collections must not expand the subscription set"
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
        collections: set(&["AgentRequest"]),
        replicator_addresses: set(&["/ip4/1/tcp/1/p2p/peer-a"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: one_filter("AgentRequest", "node_did", "did:key:a"),
        template_ids: BTreeSet::new(),
    };

    let merged = merge_desired(None, Some(data)).expect("data-plane desired");
    assert!(
        merged.collections.is_empty(),
        "data-plane-only desired must not subscribe to conversation collections"
    );
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
            collections: None,
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
            collections: None,
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
