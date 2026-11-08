use crate::runtime::agent::p2p_reconcile::engine::{
    data_plane_desired_from_pairing_row, EnrollmentEndpointEntry, PairingStateRow,
};

pub(super) fn desired_row(template: Option<&str>, node_did: Option<&str>) -> PairingStateRow {
    PairingStateRow {
        node_did: node_did.map(str::to_string),
        replicator_addresses: Some(vec!["addr1".into()]),
        template: template.map(str::to_string),
        ..Default::default()
    }
}

fn signed_endpoint() -> EnrollmentEndpointEntry {
    EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        node_did: "did:key:bob".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    }
}

#[test]
fn missing_and_unknown_templates_are_rejected() {
    let missing = data_plane_desired_from_pairing_row(
        desired_row(None, None),
        &signed_endpoint(),
        "did:key:self",
    )
    .expect_err("missing template must not create routing authority");
    assert!(missing.to_string().contains("missing its scope template"));

    let unknown = data_plane_desired_from_pairing_row(
        desired_row(Some("not-a-template"), None),
        &signed_endpoint(),
        "did:key:self",
    )
    .expect_err("unknown template must not create routing authority");
    assert!(unknown
        .to_string()
        .contains("unknown data-plane pairing scope template"));
}

#[test]
fn foreign_node_did_still_hard_fails_whole_peer_load() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        node_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let err = data_plane_desired_from_pairing_row(
        PairingStateRow {
            node_did: Some("did:key:someone-else".to_string()),
            replicator_addresses: None,
            template: Some("machine".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect_err("foreign node_did must hard-fail, not soft-skip");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("foreign") || msg.contains("someone-else") || msg.contains("refusing"),
        "error should name the refusal: {msg}"
    );
}
