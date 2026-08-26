use std::collections::BTreeSet;

use gents::agent::p2p_reconcile::templates::{
    admit_app_collections, builtin_templates, equality_filter, filter_conditions, resolve_template,
    scope_filter, single_string_eq, Delivery, FilterPredicate, Scope, AGENT_DIRECTORY_COLLECTION,
};
use gents::agent::p2p_reconcile::{
    client_route_collections, resolve_template_filters, PairingDirection, CLIENT_COLLECTIONS,
    CLIENT_TEMPLATE, CLIENT_TO_RUNTIME_COLLECTIONS,
};

const LEAN_SCOPE_STATE: &str = include_str!("../../proofs/Proofs/ScopeTemplates/State.lean");

fn lean_string_list(definition: &str) -> Vec<String> {
    let marker = format!("def {definition} : List String :=");
    let body = LEAN_SCOPE_STATE
        .split_once(&marker)
        .unwrap_or_else(|| panic!("Lean scope model omitted {definition}"))
        .1
        .split("\n\n")
        .next()
        .expect("Lean list body");
    body.lines()
        .flat_map(|line| {
            line.split('"')
                .enumerate()
                .filter_map(|(index, value)| (index % 2 == 1).then_some(value.to_string()))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn assert_eq_filter(predicate: &FilterPredicate, field: &str, value: &str) {
    assert_eq!(single_string_eq(predicate), Some((field, value)));
}

fn assert_and_eq_filter(predicate: &FilterPredicate, expected: &[(&str, &str)]) {
    let conditions = filter_conditions(predicate).expect("predicate filter");
    let clauses = conditions
        .get("_and")
        .and_then(serde_json::Value::as_array)
        .expect("conjunctive _and filter");
    assert_eq!(clauses.len(), expected.len());
    for (field, value) in expected {
        assert!(
            clauses.iter().any(|clause| {
                clause
                    .get(*field)
                    .and_then(|operators| operators.get("_eq"))
                    .and_then(serde_json::Value::as_str)
                    == Some(*value)
            }),
            "missing {field} = {value:?} in {conditions:?}"
        );
    }
}

#[test]
fn resolve_template_is_total_over_catalog_and_id_faithful() {
    assert_eq!(
        builtin_templates()
            .iter()
            .map(|template| template.id)
            .collect::<Vec<_>>(),
        vec![
            "conversation",
            "machine",
            "client",
            "agent-config",
            "backup",
            "discovery",
            "network-control",
            "subagent-coordinator",
            "subagent-host",
            "app-collections",
            "client-index",
        ],
        "the Rust catalog must remain identical to Lean builtinCatalog"
    );
    for t in builtin_templates() {
        let resolved = resolve_template(t.id).expect("catalog id resolves");
        assert_eq!(resolved.id, t.id, "resolution must not alias ids");
    }
    assert!(
        resolve_template("definitely-not-a-template").is_none(),
        "unknown id resolves to none"
    );
}

/// Mirrors Lean `client_request_filter_conjoins_requester_and_destination`,
/// `client_transcript_destination_scoped`, and the exact directional
/// collection theorems.
#[test]
fn client_route_is_directional_destination_scoped_and_control_plane_bounded() {
    const TRANSCRIPT: &[&str] = &[
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
        "MailboxItem",
    ];
    const CLIENT_TO_RUNTIME: &[&str] = &[
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
        "MailboxItem",
        "BearerPairingReady",
        "PeerEndpoint",
        "SessionHydrationRequest",
    ];
    const RETURN_CONTROL_PLANE: &[&str] = &[
        "AgentBehavior",
        "ToolSelection",
        "InferenceProfile",
        "ToolServiceRegistry",
        "Skill",
        "DatastoreToolSurface",
        "Task",
        "Schedule",
        "EventTrigger",
    ];
    const RUNTIME_TO_CLIENT: &[&str] = &[
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
        "MailboxItem",
        "BearerPairingReady",
        "PeerEndpoint",
        "SessionHydrationRequest",
        "AgentBehavior",
        "ToolSelection",
        "InferenceProfile",
        "ToolServiceRegistry",
        "Skill",
        "DatastoreToolSurface",
        "Task",
        "Schedule",
        "EventTrigger",
    ];

    assert_eq!(
        lean_string_list("clientTranscriptCollections"),
        TRANSCRIPT,
        "Rust transcript policy must conform to the checked Lean model source"
    );
    assert_eq!(
        lean_string_list("clientControlPlaneCollections"),
        RETURN_CONTROL_PLANE,
        "Rust return control plane must conform to the checked Lean model source"
    );

    let template = resolve_template(CLIENT_TEMPLATE).expect("client in catalog");
    assert_eq!(template.delivery, Delivery::Push);
    assert!(matches!(template.scope, Scope::ClientRoute));
    assert_eq!(CLIENT_TO_RUNTIME_COLLECTIONS, CLIENT_TO_RUNTIME);
    assert_eq!(CLIENT_COLLECTIONS, RUNTIME_TO_CLIENT);
    assert_eq!(template.collections, RUNTIME_TO_CLIENT);
    assert_eq!(
        client_route_collections(PairingDirection::ClientToRuntime),
        CLIENT_TO_RUNTIME
    );
    assert_eq!(
        client_route_collections(PairingDirection::RuntimeToClient),
        RUNTIME_TO_CLIENT
    );

    let requester = "did:key:phone";
    let owner = "did:key:mandrake";
    let non_owner = "did:key:amy";
    let outbound = resolve_template_filters(
        template,
        PairingDirection::ClientToRuntime,
        requester,
        owner,
    );
    assert_eq!(outbound.len(), CLIENT_TO_RUNTIME.len());
    for collection in TRANSCRIPT {
        assert_and_eq_filter(
            outbound.get(*collection).expect("transcript filter"),
            &[("requester_did", requester), ("agent_did", owner)],
        );
        let encoded = serde_json::to_string(outbound.get(*collection).unwrap()).unwrap();
        assert!(
            !encoded.contains(non_owner),
            "{collection} must not admit the non-owning destination"
        );
    }
    assert_and_eq_filter(
        outbound
            .get("BearerPairingReady")
            .expect("readiness filter"),
        &[("claimant_did", requester), ("issuer_did", owner)],
    );
    assert_eq_filter(
        outbound
            .get("PeerEndpoint")
            .expect("outbound endpoint filter"),
        "did",
        requester,
    );
    assert_and_eq_filter(
        outbound
            .get("SessionHydrationRequest")
            .expect("hydration request filter"),
        &[("requester_did", requester), ("agent_did", owner)],
    );
    for collection in RETURN_CONTROL_PLANE {
        assert!(!outbound.contains_key(*collection));
        assert!(!CLIENT_TO_RUNTIME.contains(collection));
    }

    let returning = resolve_template_filters(
        template,
        PairingDirection::RuntimeToClient,
        requester,
        owner,
    );
    assert_eq!(returning.len(), CLIENT_TO_RUNTIME.len());
    assert_eq_filter(
        returning
            .get("PeerEndpoint")
            .expect("return endpoint filter"),
        "did",
        owner,
    );
    for collection in RETURN_CONTROL_PLANE {
        assert!(template.collections.contains(collection));
        assert!(
            !returning.contains_key(*collection),
            "bounded return control-plane collection {collection} is deliberately unfiltered"
        );
    }
    for excluded in [
        "InferenceBackend",
        "PeerPairingDesired",
        "DataPlanePairingDesired",
    ] {
        assert!(
            !template.collections.contains(&excluded),
            "client route must exclude {excluded}"
        );
    }
}

#[test]
fn sensitive_local_audit_payloads_have_no_builtin_replication_route() {
    for template in builtin_templates() {
        for collection in ["RenderedRequest", "ProviderContextReduction"] {
            assert!(
                !template.collections.contains(&collection),
                "{} must not replicate local audit collection {collection}",
                template.id
            );
        }
    }
}

#[test]
fn conversation_scope_filters_transcript_and_grants_unfiltered_config() {
    let t = resolve_template("conversation").expect("conversation in catalog");
    assert_eq!(t.delivery, Delivery::Push);
    assert!(matches!(t.scope, Scope::PerCollection(_)));

    let filter = scope_filter(&t.scope, t.collections, "did:key:bob", "did:key:alice");
    for col in [
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
        "BearerPairingReady",
    ] {
        let pred = filter.get(col).expect("transcript collection filter");
        let expected_field = if col == "BearerPairingReady" {
            "claimant_did"
        } else {
            "requester_did"
        };
        assert_eq_filter(pred, expected_field, "did:key:bob");
    }
    for col in [
        "AgentBehavior",
        "ToolSelection",
        "InferenceBackend",
        "InferenceProfile",
        "ToolServiceRegistry",
        "Skill",
    ] {
        assert!(t.collections.contains(&col));
        assert!(!filter.contains_key(col), "config {col} must be unfiltered");
    }
}

#[test]
fn conversation_scope_excludes_another_requester_on_the_same_agent() {
    let t = resolve_template("conversation").expect("conversation in catalog");
    let phone_filter = scope_filter(&t.scope, t.collections, "did:key:phone", "did:key:amy");
    let predicate = phone_filter
        .get("AgentRequest")
        .expect("request filter is present");

    let phone_request = ("did:key:amy", Some("did:key:phone"));
    let classifier_request = ("did:key:amy", None);
    assert_eq_filter(predicate, "requester_did", "did:key:phone");
    assert_eq!(phone_request.1, Some("did:key:phone"));
    assert_ne!(classifier_request.1, Some("did:key:phone"));
}

/// Mirrors Lean `clientIndex_filter_eq` and
/// `clientIndex_filters_requester_lineage`.
#[test]
fn client_index_scope_is_exactly_the_requester_scoped_literal_index() {
    let template = resolve_template("client-index").expect("client-index in catalog");
    assert_eq!(template.delivery, Delivery::Push);
    assert_eq!(
        template.collections,
        &["AgentConversation", "AgentSession", "MailboxItem"]
    );

    let phone = scope_filter(
        &template.scope,
        template.collections,
        "did:key:phone",
        "did:key:home",
    );
    assert_eq!(phone.len(), 3);
    for collection in template.collections {
        let predicate = phone.get(*collection).expect("collection filtered");
        assert_eq_filter(predicate, "requester_did", "did:key:phone");
    }

    let laptop = scope_filter(
        &template.scope,
        template.collections,
        "did:key:laptop",
        "did:key:home",
    );
    assert_ne!(
        phone.get("AgentSession").unwrap(),
        laptop.get("AgentSession").unwrap()
    );
}

#[test]
fn machine_scope_covers_conversation_and_home_owned_directory() {
    let template = resolve_template("machine").expect("machine in catalog");
    let filters = scope_filter(
        &template.scope,
        template.collections,
        "did:key:phone",
        "did:key:issuer",
    );

    for collection in [
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
        "BearerPairingReady",
    ] {
        let predicate = filters.get(collection).expect("conversation filter");
        let expected_field = if collection == "BearerPairingReady" {
            "claimant_did"
        } else {
            "requester_did"
        };
        assert_eq_filter(predicate, expected_field, "did:key:phone");
    }
    assert_eq!(
        filters.get(AGENT_DIRECTORY_COLLECTION),
        Some(&equality_filter("source_did", "did:key:issuer"))
    );
}

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

#[test]
fn subagent_templates_resolve_to_exact_directional_filters() {
    const RETURN_PROJECTION: &[&str] = &[
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
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
        Some(&equality_filter("spawn_target_did", "did:key:host"))
    );

    let host = resolve_template("subagent-host").expect("host template");
    assert_eq!(host.delivery, Delivery::Push);
    assert_eq!(host.collections, RETURN_PROJECTION);
    let host_filter = scope_filter(
        &host.scope,
        host.collections,
        "did:key:coord",
        "did:key:host",
    );
    assert_eq!(host_filter.len(), RETURN_PROJECTION.len());
    assert_eq!(
        host_filter.get("AgentRequest"),
        Some(&equality_filter("requester_did", "did:key:coord"))
    );
    for collection in RETURN_PROJECTION {
        assert_eq!(
            host_filter.get(*collection),
            Some(&equality_filter("requester_did", "did:key:coord")),
            "unexpected host filter for {collection}"
        );
    }
    for local_collection in [
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry",
    ] {
        assert!(!host.collections.contains(&local_collection));
        assert!(!host_filter.contains_key(local_collection));
    }

    for predicate in coord_filter.values().chain(host_filter.values()) {
        let (_, value) = single_string_eq(predicate).expect("single equality filter");
        assert!(value == "did:key:coord" || value == "did:key:host");
    }
}

#[test]
fn subagent_host_message_filter_excludes_unrelated_host_history() {
    let host = resolve_template("subagent-host").expect("host template");
    let filter = scope_filter(
        &host.scope,
        host.collections,
        "did:key:coord",
        "did:key:host",
    );
    let predicate = filter.get("AgentMessage").expect("message filter");

    let child_requester_did = Some("did:key:coord");
    let unrelated_requester_did: Option<&str> = None;
    assert_eq_filter(predicate, "requester_did", "did:key:coord");
    assert_eq!(child_requester_did, Some("did:key:coord"));
    assert_ne!(unrelated_requester_did, Some("did:key:coord"));
}

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
                    single_string_eq(predicate) == Some(("requester_did", requester_did))
                })
        })
        .count();
    assert_eq!(routed_child_request_matches, 1);
}

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
    let f = scope_filter(
        &t.scope,
        &["ChangeProposed"],
        "did:key:bob",
        "did:key:alice",
    );
    assert!(f.is_empty(), "unscoped app-collections must not filter");
}

#[test]
fn app_collection_admission_matches_lean_protocol_disjointness_contract() {
    assert!(admit_app_collections(BTreeSet::new()).is_none());

    let custom = BTreeSet::from(["ChangeProposed".to_string()]);
    assert_eq!(admit_app_collections(custom.clone()), Some(custom));

    for protocol in gents_protocol::schemas::ALL_COLLECTION_NAMES
        .iter()
        .chain(gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.iter())
    {
        let requested = BTreeSet::from(["ChangeProposed".to_string(), (*protocol).to_string()]);
        assert!(
            admit_app_collections(requested).is_none(),
            "Lean appCollections_protocol_overlap_rejected violated by {protocol}"
        );
    }
}
