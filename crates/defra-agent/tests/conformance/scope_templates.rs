//! Conformance fence for `Proofs/ScopeTemplates/`.
//!
//! Bridges the Lean resolution model to the Rust scope-template catalog and the
//! template-driven pairing reconciler. Each test names the Lean theorem it
//! mirrors and calls the REAL `resolve_template` / `scope_filter` (never a
//! reimpl), so the resolution the reconciler uses is fenced against the spec.

use defra_agent::agent::p2p_reconcile::templates::{
    builtin_templates, resolve_template, scope_filter, Delivery, FilterPredicate, Scope,
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

    let filter = scope_filter(&t.scope, t.collections, "did:key:bob", "did:key:alice");
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
            scope_filter(&t.scope, t.collections, "did:key:bob", "did:key:alice").is_empty(),
            "{id} must be unfiltered"
        );
    }
}

/// Mirrors Lean `subagentCoordinator_filter_eq` / `subagentHost_filter_eq` /
/// `subagentRequest_crossing_is_peer_scoped`: coordinator parent requests do
/// not fan out to hosts, and host-owned child requests return only to the
/// paired requester DID.
#[test]
fn subagent_templates_resolve_to_exact_directional_filters() {
    const CONVERSATION: &[&str] = &[
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
    ];

    let coord = resolve_template("subagent-coordinator").expect("coordinator template");
    assert_eq!(coord.delivery, Delivery::Push);
    assert_eq!(coord.collections, &["AgentToolCall"]);
    let coord_filter = scope_filter(
        &coord.scope,
        coord.collections,
        "did:key:host",
        "did:key:coord",
    );
    assert_eq!(coord_filter.len(), 1);
    assert!(!coord_filter.contains_key("AgentRequest"));
    assert_eq!(
        coord_filter.get("AgentToolCall"),
        Some(&FilterPredicate {
            field: "spawn_target_did".to_string(),
            value: "did:key:host".to_string(),
        })
    );

    let host = resolve_template("subagent-host").expect("host template");
    assert_eq!(host.delivery, Delivery::Push);
    assert_eq!(host.collections, CONVERSATION);
    let host_filter = scope_filter(
        &host.scope,
        host.collections,
        "did:key:coord",
        "did:key:host",
    );
    assert_eq!(host_filter.len(), CONVERSATION.len());
    assert_eq!(
        host_filter.get("AgentRequest"),
        Some(&FilterPredicate {
            field: "requester_did".to_string(),
            value: "did:key:coord".to_string(),
        })
    );
    for collection in CONVERSATION
        .iter()
        .copied()
        .filter(|collection| *collection != "AgentRequest")
    {
        assert_eq!(
            host_filter.get(collection),
            Some(&FilterPredicate {
                field: "agent_did".to_string(),
                value: "did:key:host".to_string(),
            }),
            "unexpected host filter for {collection}"
        );
    }

    for predicate in coord_filter.values().chain(host_filter.values()) {
        assert!(
            predicate.value == "did:key:coord" || predicate.value == "did:key:host",
            "subagent filters must not include third-party DIDs"
        );
    }
}

/// Regression/measurement for #683. Under the former coordinator rule, one
/// owner request matched all 16 host pairings because every pairing filtered
/// `AgentRequest.agent_did` by the same coordinator DID. The request-party
/// route key makes the return leg match exactly one requesting coordinator,
/// while the coordinator leg carries no parent request at all.
#[test]
fn sixteen_peer_request_wave_is_reduced_to_one_target() {
    let coordinator = resolve_template("subagent-coordinator").expect("coordinator template");
    let host = resolve_template("subagent-host").expect("host template");
    let requester_did = "did:key:coordinator-07";

    let current_parent_request_matches = (0..16)
        .filter(|index| {
            let host_did = format!("did:key:host-{index:02}");
            scope_filter(
                &coordinator.scope,
                coordinator.collections,
                &host_did,
                requester_did,
            )
            .contains_key("AgentRequest")
        })
        .count();
    assert_eq!(current_parent_request_matches, 0);

    let routed_child_request_matches = (0..16)
        .filter(|index| {
            let peer_did = format!("did:key:coordinator-{index:02}");
            scope_filter(&host.scope, host.collections, &peer_did, "did:key:host")
                .get("AgentRequest")
                .is_some_and(|predicate| {
                    predicate.field == "requester_did" && predicate.value == requester_did
                })
        })
        .count();
    assert_eq!(routed_child_request_matches, 1);
}

/// Mirrors Lean `appCollections_in_catalog` / `appCollections_collections_empty`
/// / `appCollections_unscoped_no_filter`: the app-collections "bring-your-own"
/// template resolves, is Unscoped + Replicate, and carries no fixed collections
/// (the DataPlanePairingDesired row supplies them).
#[test]
fn app_collections_template_is_unscoped_replicate_byo() {
    let t = resolve_template("app-collections").expect("app-collections in catalog");
    assert_eq!(t.id, "app-collections");
    assert!(matches!(t.delivery, Delivery::Replicate));
    assert!(matches!(t.scope, Scope::Unscoped));
    assert!(
        t.collections.is_empty(),
        "app-collections carries no fixed collections; the row supplies them"
    );
    // Unscoped yields no filters even over a supplied collection list.
    let f = scope_filter(
        &t.scope,
        &["ChangeProposed"],
        "did:key:bob",
        "did:key:alice",
    );
    assert!(f.is_empty(), "unscoped app-collections must not filter");
}
