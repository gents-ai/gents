//! Conformance fence for `Proofs/ScopeTemplates/`.
//!
//! Bridges the Lean resolution model to the Rust scope-template catalog and the
//! template-driven pairing reconciler. Each test names the Lean theorem it
//! mirrors and calls the REAL `resolve_template` / `scope_filter` (never a
//! reimpl), so the resolution the reconciler uses is fenced against the spec.

use defra_agent::agent::p2p_reconcile::templates::{
    builtin_templates, resolve_template, scope_filter, Delivery, Scope,
};

/// Mirrors Lean `resolveTemplate_isSome_iff` / `resolveTemplate_id_eq`: every
/// catalog id resolves to `some` carrying exactly that id, and an absent id
/// resolves to `none`.
#[test]
fn resolve_template_is_total_over_catalog_and_id_faithful() {
    for t in builtin_templates() {
        let resolved = resolve_template(t.id).expect("catalog id resolves");
        assert_eq!(resolved.id, t.id, "resolution must not alias ids");
    }
    assert!(
        resolve_template("definitely-not-a-template").is_none(),
        "unknown id resolves to none"
    );
}

/// Mirrors Lean `scopeFilter_peerDid` + `push_template_has_filter`: a `PeerDid`
/// scope (the conversation/Push template) resolves to a per-collection equality
/// filter keyed on the field and the peer DID — push is never silently
/// unfiltered.
#[test]
fn peer_did_scope_resolves_to_per_collection_filter() {
    let t = resolve_template("conversation").expect("conversation in catalog");
    assert_eq!(t.delivery, Delivery::Push);
    assert!(matches!(t.scope, Scope::PeerDid { field } if field == "agent_did"));

    let filter = scope_filter(&t.scope, t.collections, "did:key:bob");
    assert_eq!(filter.len(), t.collections.len());
    for col in t.collections {
        let pred = filter.get(*col).expect("filter for every collection");
        assert_eq!(pred.field, "agent_did");
        assert_eq!(pred.value, "did:key:bob");
    }
}

/// Mirrors Lean `scopeFilter_unscoped` + `scopeFilter_isSome_iff`: an `Unscoped`
/// scope (the agent-config / backup Replicate templates) yields NO filter, i.e.
/// whole-collection replication.
#[test]
fn unscoped_scope_resolves_to_no_filter() {
    for id in ["agent-config", "backup"] {
        let t = resolve_template(id).expect("template in catalog");
        assert!(matches!(t.scope, Scope::Unscoped));
        assert!(
            scope_filter(&t.scope, t.collections, "did:key:bob").is_empty(),
            "{id} must be unfiltered"
        );
    }
}
