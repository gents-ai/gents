//! Fence tying `gents_migration::CLIENT_AUTHORED_COLLECTIONS` (guarded against
//! chained schema evolution by `fresh_apply_parity.rs` in `gents-migration`)
//! to the client push surface gents itself configures, so extending that
//! surface without extending the guard fails here.
//!
//! The load-bearing property is "a client store fresh-applies this
//! collection's SDL and replicates documents in it", not strict client
//! authorship: some rows are minted server-side but replicate through client
//! stores all the same, and a client store can only merge a collection whose
//! fresh-applied version identity matches the server's.
//!
//! The derived surface is the `machine` `ScopeTemplate`'s `PerCollection`
//! rules plus the exact owner-scoped enrollment direct-push collections;
//! template collections listed unfiltered are shared config, excluded.

use std::collections::BTreeSet;

use gents::agent::p2p_reconcile::templates::{resolve_template, Scope, MACHINE_TEMPLATE};
use gents::agent::p2p_reconcile::{client_route_collections, PairingDirection};
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
