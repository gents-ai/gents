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
//! (`BearerPairingReady` is written by the reconcile engine on the issuer,
//! and the `AgentDirectoryEntry` rule filters on the home DID), but they
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
//! - Control-plane claim collections a claimant device bootstrap-pushes
//!   before any template applies: `PairingBearerClaim` and `PeerEndpoint`,
//!   mirroring `gents-desktop-core`'s `BEARER_CONTROL_PLANE_COLLECTIONS`
//!   (the subset test in `bearer_pairing.rs` ties that constant to the
//!   guard list, so the mirror cannot drift silently).
//!
//! Not derived here: bring-your-own data-plane pairings —
//! `DataPlanePairingDesired` rows can name arbitrary collections for app
//! templates (`p2p_reconcile/engine.rs`) with no protocol-collection
//! exclusion; closing that hole is #1137.

use std::collections::BTreeSet;

use gents::agent::p2p_reconcile::templates::{resolve_template, Scope, MACHINE_TEMPLATE};
use gents_migration::CLIENT_AUTHORED_COLLECTIONS;

const CONTROL_PLANE_CLAIM_COLLECTIONS: &[&str] = &[
    gents_protocol::schemas::PAIRING_BEARER_CLAIM_NAME,
    gents_protocol::schemas::PEER_ENDPOINT_NAME,
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
    actual.extend(CONTROL_PLANE_CLAIM_COLLECTIONS.iter().copied());

    let expected: BTreeSet<&str> = CLIENT_AUTHORED_COLLECTIONS.iter().copied().collect();

    assert_eq!(
        actual, expected,
        "gents's client push surface (machine template per-collection rules + control-plane \
         claim collections) drifted from gents_migration::CLIENT_AUTHORED_COLLECTIONS — update \
         CLIENT_AUTHORED_COLLECTIONS in crates/gents-migration/src/registry.rs so \
         fresh_apply_parity.rs guards the new collection too (see #1123/#1125)"
    );
}
