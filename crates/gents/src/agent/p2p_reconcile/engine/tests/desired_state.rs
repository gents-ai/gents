use super::set;
use crate::agent::p2p_reconcile::{
    engine::{
        data_plane_desired_from_pairing_row, desired_from_pairing_row, EnrollmentEndpointEntry,
        PairingStateRow,
    },
    single_string_eq,
};

pub(super) fn desired_row(template: Option<&str>, agent_did: Option<&str>) -> PairingStateRow {
    PairingStateRow {
        agent_did: agent_did.map(str::to_string),
        collections: None,
        replicator_addresses: Some(vec!["addr1".into()]),
        template: template.map(str::to_string),
        ..Default::default()
    }
}

/// A `Push` template (conversation) resolves to NO subscription collections
/// (no gossip leak) and a per-peer scope filter over the template set.
#[test]
fn push_template_resolves_to_filter_without_subscription() {
    let desired = desired_from_pairing_row(
        desired_row(Some("conversation"), Some("did:key:bob")),
        "did:key:self",
    )
    .expect("template resolves")
    .expect("some desired layer");

    assert!(
        desired.collections.is_empty(),
        "Push templates must not subscribe"
    );
    assert!(desired.replicator_collections.contains("AgentRequest"));
    let pred = desired
        .replicator_filter
        .get("AgentRequest")
        .expect("AgentRequest filter");
    assert_eq!(
        single_string_eq(pred),
        Some(("requester_did", "did:key:bob"))
    );
}

/// A `Replicate` template (agent-config) subscribes to its collection set
/// and carries an EMPTY (unfiltered) replicator filter.
#[test]
fn replicate_template_resolves_to_subscription_without_filter() {
    let desired = desired_from_pairing_row(
        desired_row(Some("agent-config"), Some("did:key:bob")),
        "did:key:self",
    )
    .expect("template resolves")
    .expect("some desired layer");

    assert!(desired.collections.contains("AgentBehavior"));
    assert_eq!(desired.collections, desired.replicator_collections);
    assert!(
        desired.replicator_filter.is_empty(),
        "Replicate templates are unfiltered"
    );
}

/// Rows without a template default to `conversation` (matches the migration
/// backfill), and an unknown template also falls back to the default.
#[test]
fn missing_and_unknown_template_default_to_conversation() {
    let missing = desired_from_pairing_row(desired_row(None, Some("did:key:bob")), "did:key:self")
        .expect("default resolves")
        .expect("some desired layer");
    assert!(missing.collections.is_empty());
    assert!(missing.replicator_filter.contains_key("AgentRequest"));

    let unknown = desired_from_pairing_row(
        desired_row(Some("not-a-template"), Some("did:key:bob")),
        "did:key:self",
    )
    .expect("default resolves")
    .expect("some desired layer");
    assert_eq!(
        unknown.replicator_collections,
        missing.replicator_collections
    );
    assert!(unknown.replicator_filter.contains_key("AgentRequest"));
}

#[test]
fn subagent_coordinator_template_filters_only_targeted_bridge() {
    let desired = desired_from_pairing_row(
        desired_row(Some("subagent-coordinator"), Some("did:key:host")),
        "did:key:coord",
    )
    .expect("subagent coordinator template resolves")
    .expect("some desired layer");

    assert!(desired.collections.is_empty());
    assert_eq!(desired.replicator_collections, set(&["AgentToolCall"]));
    assert!(!desired.replicator_filter.contains_key("AgentRequest"));
    assert_eq!(
        desired
            .replicator_filter
            .get("AgentToolCall")
            .and_then(single_string_eq),
        Some(("spawn_target_did", "did:key:host"))
    );
}

#[test]
fn subagent_host_template_filters_return_projection_to_requester() {
    let desired = desired_from_pairing_row(
        desired_row(Some("subagent-host"), Some("did:key:coord")),
        "did:key:host",
    )
    .expect("subagent host template resolves")
    .expect("some desired layer");

    assert!(desired.collections.is_empty());
    assert_eq!(
        desired.replicator_collections,
        set(&[
            "AgentRequest",
            "AgentResponse",
            "AgentMessage",
            "AgentToolCall"
        ])
    );
    assert_eq!(desired.replicator_filter.len(), 4);
    for predicate in desired.replicator_filter.values() {
        assert_eq!(
            single_string_eq(predicate),
            Some(("requester_did", "did:key:coord"))
        );
    }
}

#[test]
fn app_collections_on_control_plane_path_soft_skips() {
    // A base/PeerPairingDesired row naming app-collections has no way to
    // supply row collections; it must resolve to no wiring (soft-skip),
    // never an empty-collection replicator.
    let out = desired_from_pairing_row(
        PairingStateRow {
            agent_did: Some("did:key:peer".to_string()),
            collections: None,
            replicator_addresses: Some(vec!["addr-b".to_string()]),
            template: Some("app-collections".to_string()),
            ..Default::default()
        },
        "did:key:self",
    )
    .expect("resolve ok");
    assert!(
        out.is_none(),
        "app-collections is invalid for a control-plane row"
    );
}

#[test]
fn app_collections_row_resolves_row_collections_as_subscription_and_replicator() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        agent_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let layer = data_plane_desired_from_pairing_row(
        PairingStateRow {
            agent_did: Some("did:key:self".to_string()),
            collections: Some(vec!["ChangeProposed".to_string()]),
            replicator_addresses: None,
            template: Some("app-collections".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect("resolve ok")
    .expect("some layer");
    assert!(layer.replicator_collections.contains("ChangeProposed"));
    assert!(
        layer.collections.contains("ChangeProposed"),
        "app-collections must subscribe (Replicate)"
    );
    assert!(layer.replicator_filter.is_empty(), "unscoped => no filter");
    assert!(layer.template_ids.contains("app-collections"));
}

#[test]
fn app_collections_empty_collections_soft_skips() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        agent_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let out = data_plane_desired_from_pairing_row(
        PairingStateRow {
            agent_did: Some("did:key:self".to_string()),
            collections: Some(vec!["   ".to_string()]),
            replicator_addresses: None,
            template: Some("app-collections".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect("resolve ok (soft-skip is Ok(None), not Err)");
    assert!(
        out.is_none(),
        "empty/blank app-collections set must soft-skip to None"
    );
}

/// Residual (documented, not softened in #657): a foreign `agent_did` on a
/// data-plane row still hard-fails the whole peer load (`desired_read_failed`),
/// including a co-existing control pairing. Security refusal, not soft-skip.
#[test]
fn foreign_agent_did_still_hard_fails_whole_peer_load() {
    let signed_endpoint = EnrollmentEndpointEntry {
        peer_id: "peer-b".to_string(),
        agent_did: "did:key:peer-b".to_string(),
        address: "/ip4/127.0.0.1/tcp/4001/p2p/peer-b".to_string(),
        desired_id: "peer-b".to_string(),
        request_digest: "digest".to_string(),
        authorization_sequence: 1,
        authorization_expires_at: "2099-09-29T00:00:00Z".to_string(),
    };
    let err = data_plane_desired_from_pairing_row(
        PairingStateRow {
            agent_did: Some("did:key:someone-else".to_string()),
            collections: Some(vec!["ChangeProposed".to_string()]),
            replicator_addresses: None,
            template: Some("app-collections".to_string()),
            ..Default::default()
        },
        &signed_endpoint,
        "did:key:self",
    )
    .expect_err("foreign agent_did must hard-fail, not soft-skip");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("foreign") || msg.contains("someone-else") || msg.contains("refusing"),
        "error should name the refusal: {msg}"
    );
}
