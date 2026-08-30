//! List-sync fence for #1123/#1125.
//!
//! `gents_migration::CLIENT_AUTHORED_COLLECTIONS` names every collection a
//! paired client fresh-applies its bundled SDL into and then syncs documents
//! through — `fresh_apply_parity.rs` (in `gents-migration`) guards those
//! collections against chained schema evolution. That guard is only as good
//! as its coverage: this test asserts the list matches the client push
//! surface gents itself configures, so extending that surface without
//! extending the guard fails a build instead of silently shipping an
//! un-guarded collection.
//!
//! The load-bearing property is "a client store fresh-applies this
//! collection's SDL and replicates documents in it", not strict client
//! authorship: some rows in these collections are minted server-side
//! (`AgentBehaviorReadiness` is written by the runtime owner, and the
//! `AgentDirectoryEntry` rule filters on the home DID), but they
//! replicate through client stores all the same, and a client store can
//! only merge a collection whose fresh-applied version identity matches the
//! server's.
//!
//! The surface this fence derives is two parts:
//! - The `machine` `ScopeTemplate`'s `PerCollection` rules
//!   (`crates/gents/src/agent/p2p_reconcile/templates.rs`): the collections
//!   replicated per-pairing to client devices. Collections the template
//!   lists but leaves unfiltered (`AgentBehavior`, `ToolSelection`, ...) are
//!   deliberately shared config pushed identically to every peer, not part
//!   of the client-authored plane, and excluded.
//! - Enrollment protocol collections use their own exact owner-scoped direct
//!   push path and are cataloged separately from broad client replication.
//! - Authenticated-enrollment documents exchanged by exact owner-scoped
//!   direct push. They deliberately stay outside the broad machine template,
//!   but require the same collection-version parity at both stores.
//!
//! Bring-your-own data-plane pairings are fenced separately by
//! `templates::admit_app_collections`: any overlap with the full protocol
//! catalog rejects that app layer before it reaches this migration surface.

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
        !rules
            .iter()
            .any(|rule| rule.collection == gents_protocol::schemas::PERSONA_CONFIG_REQUEST_NAME),
        "PersonaConfigRequest must stay off the broad machine template"
    );
    assert!(
        client_route_collections(PairingDirection::ClientToRuntime)
            .contains(&gents_protocol::schemas::PERSONA_CONFIG_REQUEST_NAME),
        "PersonaConfigRequest must use the exact enrolled client route"
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
