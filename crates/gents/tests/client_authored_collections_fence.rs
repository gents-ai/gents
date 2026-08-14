//! List-sync fence for #1123/#1125.
//!
//! `gents_migration::CLIENT_AUTHORED_COLLECTIONS` names every collection a
//! paired client fresh-applies its bundled SDL into and authors documents
//! into directly — `fresh_apply_parity.rs` (in `gents-migration`) guards
//! those collections against chained schema evolution. That guard is only as
//! good as its coverage: this test asserts the list exactly matches gents's
//! actual client push surface, so adding a collection to the client surface
//! without adding it to the guard fails a build instead of silently shipping
//! an un-guarded collection.
//!
//! The client push surface is two parts:
//! - The `machine` `ScopeTemplate`'s `PerCollection` rules
//!   (`crates/gents/src/agent/p2p_reconcile/templates.rs`): each rule filters
//!   a collection on a client-owned DID field (requester/claimant/source),
//!   which is exactly the set of collections scoped to one client's own
//!   documents. Collections the template declares but leaves unfiltered
//!   (`AgentBehavior`, `ToolSelection`, ...) are deliberately shared config
//!   pushed identically to every peer, not client-authored, and excluded.
//! - Control-plane claim collections a client authors directly, outside any
//!   `ScopeTemplate`'s collection list: `PairingBearerClaim` (a claimant
//!   device's self-signed redemption row —
//!   `crates/gents/src/agent/p2p_reconcile/bearer_claim.rs`) and
//!   `PeerEndpoint` (a client's signed heartbeat —
//!   `crates/gents/src/agent/p2p_reconcile/endpoint.rs`).

use std::collections::BTreeSet;

use gents::agent::p2p_reconcile::templates::{resolve_template, Scope, MACHINE_TEMPLATE};
use gents_migration::CLIENT_AUTHORED_COLLECTIONS;

const CONTROL_PLANE_CLAIM_COLLECTIONS: &[&str] = &["PairingBearerClaim", "PeerEndpoint"];

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
