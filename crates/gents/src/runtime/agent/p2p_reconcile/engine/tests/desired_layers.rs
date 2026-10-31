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
fn protocol_collection_in_app_data_plane_is_rejected_without_stalling_control_pairing() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-app".to_string(),
        node_did: "did:key:app".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-app".to_string(),
        desired_id: "peer-app".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let data_plane = data_plane_desired_from_pairing_row(
        PairingStateRow {
            collections: Some(vec![
                "ChangeProposed".to_string(),
                "AgentRequest".to_string(),
            ]),
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("app-collections".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect("invalid app layer is a soft rejection");
    assert!(data_plane.is_none());

    let control = PairingDesired {
        replicator_addresses: set(&[signed_endpoint.address.as_str()]),
        replicator_collections: set(&["Network"]),
        template_ids: set(&["enrollment-base"]),
        ..Default::default()
    };
    assert_eq!(
        merge_desired(Some(control.clone()), data_plane),
        Some(control),
        "a rejected app layer must not stall or weaken the control route"
    );
}

#[test]
fn data_plane_agent_target_caller_uses_signed_peer_for_addressed_requests() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        node_did: "did:key:host".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            node_did: Some("did:key:coord".to_string()),
            collections: None,
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("agent-target-caller".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:coord",
    )
    .expect("data-plane coordinator desired")
    .expect("some data-plane layer");

    assert_eq!(desired.replicator_collections, set(&["AgentRequest"]));
    assert_eq!(
        desired
            .replicator_filter
            .get("AgentRequest")
            .and_then(single_string_eq),
        Some(("node_did", "did:key:host"))
    );
}

#[test]
fn data_plane_agent_target_host_scopes_return_projection_to_signed_requester() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-a".to_string(),
        node_did: "did:key:coord".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-a".to_string(),
        desired_id: "peer-a".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let desired = data_plane_desired_from_pairing_row(
        PairingStateRow {
            node_did: Some("did:key:host".to_string()),
            collections: None,
            replicator_addresses: Some(vec![signed_endpoint.address.clone()]),
            template: Some("agent-target-host".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:host",
    )
    .expect("data-plane host desired")
    .expect("some data-plane layer");

    assert!(
        !desired.replicator_filter.is_empty(),
        "the host projection must keep requester-scoped filters through the signed-endpoint merge"
    );
    for predicate in desired.replicator_filter.values() {
        assert_eq!(
            single_string_eq(predicate),
            Some(("requester_did", "did:key:coord"))
        );
    }
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
            template: Some("conversation".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect_err("foreign data-plane scope should be rejected");

    assert!(error.to_string().contains("foreign DID"));
}

#[test]
fn data_plane_desired_machine_scopes_session_set_and_owned_directory() {
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

    assert!(desired
        .replicator_collections
        .contains(crate::runtime::agent::p2p_reconcile::templates::NODE_DIRECTORY_COLLECTION));
    for col in [
        "AgentRequest",
        "AgentOutputSegment",
        "AgentMessage",
        "AgentToolCall",
        "AgentSession",
        "CompactionEntry",
    ] {
        assert_eq!(
            desired
                .replicator_filter
                .get(col)
                .and_then(single_string_eq),
            Some(("requester_did", "did:key:peer-b")),
            "conversation collection {col} must be requester-scoped exactly like `conversation`"
        );
    }
    assert_eq!(
        desired
            .replicator_filter
            .get(crate::runtime::agent::p2p_reconcile::templates::NODE_DIRECTORY_COLLECTION)
            .and_then(single_string_eq),
        Some(("source_did", "did:key:self"))
    );
}
