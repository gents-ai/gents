use std::collections::BTreeSet;

use gents::runtime::agent::p2p_reconcile::templates::{resolve_template, Scope, MACHINE_TEMPLATE};
use gents::runtime::agent::p2p_reconcile::{client_route_collections, PairingDirection};
use gents_migration::CLIENT_AUTHORED_COLLECTIONS;

const CONTROL_PLANE_CLAIM_COLLECTIONS: &[&str] = &[gents_protocol::schemas::PEER_ENDPOINT_NAME];

const ENROLLMENT_EXACT_PUSH_COLLECTIONS: &[&str] = &[
    gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION_NAME,
    gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
];

#[test]
fn machine_template_push_set_matches_client_authored_collections_guard() {
    let machine = resolve_template(MACHINE_TEMPLATE).expect("machine template registered");
    let Scope::PerCollection(rules) = &machine.scope else {
        panic!(
            "machine template scope must be PerCollection to derive the per-client-owned \
             collection set"
        );
    };

    let mut actual: BTreeSet<&str> = rules.iter().map(|rule| rule.collection).collect();
    actual.extend(
        client_route_collections(PairingDirection::ClientToRuntime)
            .iter()
            .copied(),
    );
    actual.extend(CONTROL_PLANE_CLAIM_COLLECTIONS.iter().copied());
    actual.extend(ENROLLMENT_EXACT_PUSH_COLLECTIONS.iter().copied());

    let expected: BTreeSet<&str> = CLIENT_AUTHORED_COLLECTIONS.iter().copied().collect();

    assert!(
        !actual.contains("PersonaConfigRequest"),
        "retired persona requests must not replicate"
    );

    assert_eq!(
        actual, expected,
        "gents's client push surface (machine template per-collection rules + control-plane \
         claim collections + exact enrollment push) drifted from \
         gents_migration::CLIENT_AUTHORED_COLLECTIONS — update \
         CLIENT_AUTHORED_COLLECTIONS in crates/gents-migration/src/registry.rs so \
         fresh_apply_parity.rs guards the new collection too (see #1123/#1125)"
    );
}
