use std::collections::BTreeSet;

use gents::runtime::agent::p2p_reconcile::templates::{
    builtin_templates, resolve_template, single_string_eq, Delivery,
};
use gents::runtime::agent::p2p_reconcile::{
    client_route_collections, resolve_template_filters, PairingDirection, CLIENT_TEMPLATE,
};

#[test]
fn node_replication_matches_executable_scope_model() {
    let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
    let template = resolve_template(CLIENT_TEMPLATE).expect("client template");
    for case in &snapshot.scope_node_route_cases {
        let direction = PairingDirection::parse(case["direction"].as_str().unwrap()).unwrap();
        assert_eq!(
            client_route_collections(direction).contains(&"Node"),
            case["allowed"].as_bool().unwrap(),
        );
        let filters = resolve_template_filters(
            template,
            direction,
            "did:key:reader",
            case["owner"].as_str().unwrap(),
        );
        assert_eq!(
            filters
                .get("Node")
                .and_then(single_string_eq)
                .map(|(_, value)| value),
            case["node_did"].as_str()
        );
    }
}

#[test]
fn protocol_routes_match_model_actions() {
    let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
    let mut modeled = BTreeSet::new();
    for case in &snapshot.scope_template_cases {
        let id = case["id"].as_str().unwrap();
        modeled.insert(id);
        let template = resolve_template(id).expect("modeled protocol route");
        assert_eq!(
            template
                .collections
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            case["collections"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect()
        );
        assert_eq!(
            match template.delivery {
                Delivery::Push => "push",
            },
            case["delivery"].as_str().unwrap()
        );
        let direction = PairingDirection::parse(case["direction"].as_str().unwrap()).unwrap();
        let filters =
            resolve_template_filters(template, direction, "did:key:reader", "did:key:owner");
        let expected = case["filters"].as_array().unwrap();
        assert_eq!(filters.len(), expected.len());
        for expected in expected {
            let clause = &expected["clauses"][0];
            assert_eq!(expected["clauses"].as_array().unwrap().len(), 1);
            assert_eq!(
                single_string_eq(
                    filters
                        .get(expected["collection"].as_str().unwrap())
                        .unwrap()
                ),
                Some((
                    clause["field"].as_str().unwrap(),
                    clause["value"].as_str().unwrap()
                ))
            );
        }
    }
    assert_eq!(
        builtin_templates()
            .iter()
            .map(|template| template.id)
            .collect::<BTreeSet<_>>(),
        modeled
    );
}

/// Refusal strings come from the model; the Rust owner only decides over facts
/// the database reports.
#[test]
fn app_collection_admission_matches_executable_model() {
    use gents_protocol::peer_schema::{
        decide_app_collection, AppCollectionFacts, AppCollectionRefusal,
    };
    let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
    assert_eq!(snapshot.scope_app_collection_cases.len(), 8);
    for case in &snapshot.scope_app_collection_cases {
        let facts = AppCollectionFacts {
            managed: case["managed"].as_bool().unwrap(),
            has_policy: case["has_policy"].as_bool().unwrap(),
            is_view: case["is_view"].as_bool().unwrap(),
        };
        let refusal = decide_app_collection(facts)
            .err()
            .map(|refusal| match refusal {
                AppCollectionRefusal::Managed => "managed",
                AppCollectionRefusal::PolicyProtected => "policy_protected",
                AppCollectionRefusal::View => "view",
            });
        assert_eq!(refusal, case["refusal"].as_str(), "{case}");
    }
    let template = resolve_template("app-collections").expect("app-collections route");
    assert!(template.collections.is_empty());
}

#[test]
fn managed_catalogs_are_refused_as_application_collections() {
    for name in gents_protocol::schemas::ALL_COLLECTION_NAMES
        .iter()
        .chain(gents_protocol::schemas::RUNTIME_COLLECTION_NAMES)
    {
        assert!(
            gents_protocol::peer_schema::is_managed_collection(name),
            "{name}"
        );
    }
    assert!(!gents_protocol::peer_schema::is_managed_collection(
        "DeploymentNote"
    ));
}

#[test]
fn runtime_data_has_no_replication_template_or_app_escape() {
    let contracts = crate::lean_vocab_test::lean_identity_contracts();
    let allowed = &contracts[0].replicated_protocol_collections;
    assert_eq!(
        gents_protocol::schemas::REPLICATED_PROTOCOL_COLLECTION_NAMES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        allowed.iter().map(String::as_str).collect()
    );
    for collection in query::parse_sdl(&gents_protocol::schemas::ALL.join("\n")).unwrap() {
        assert_eq!(
            collection.is_branchable,
            allowed.contains(&collection.name),
            "{} locality",
            collection.name
        );
    }
    for collection in gents_protocol::schemas::ALL_COLLECTION_NAMES {
        assert_eq!(
            gents_protocol::schemas::is_replicated_protocol_collection(collection),
            allowed.iter().any(|name| name == collection)
        );
    }
}

/// Binding currency is the model's: cases carry revision histories, bound and
/// current generations, and the executable `bindingCurrent` verdict.
#[test]
fn overlay_binding_matches_executable_model() {
    use gents::runtime::agent::p2p_reconcile::overlay_binding::{
        binding_is_current, OverlayBinding, RevisionFact,
    };
    let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
    assert!(snapshot.scope_overlay_binding_cases.len() >= 10);
    for case in &snapshot.scope_overlay_binding_cases {
        let history = case["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|revision| RevisionFact {
                sequence: revision["sequence"].as_u64().unwrap(),
                active: revision["active"].as_bool().unwrap(),
                issued_at: revision["issued_at"].as_i64().unwrap(),
                expires_at: revision["expires_at"].as_i64().unwrap(),
            })
            .collect::<Vec<_>>();
        let binding = OverlayBinding {
            member_did: case["binding_member"].as_str().unwrap().to_owned(),
            sequence: case["binding_sequence"].as_u64().unwrap(),
        };
        assert_eq!(
            binding_is_current(
                &history,
                &binding,
                case["current_member"].as_str().unwrap(),
                case["current_sequence"].as_u64().unwrap(),
            ),
            case["current"].as_bool().unwrap(),
            "{case}"
        );
    }
}

/// The order of preconditions and the reason reported are the model's.
#[test]
fn overlay_contribution_matches_executable_model() {
    use gents::runtime::agent::p2p_reconcile::overlay_binding::{decide_overlay, OverlayFacts};
    let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
    assert_eq!(snapshot.scope_overlay_facts_cases.len(), 32);
    for case in &snapshot.scope_overlay_facts_cases {
        let flag = |name: &str| case[name].as_bool().unwrap();
        let facts = OverlayFacts {
            enrollment_fresh: flag("enrollment_fresh"),
            binding_current: flag("binding_current"),
            author_live_agent: flag("author_live_agent"),
            author_holds_grant: flag("author_holds_grant"),
            collections_admitted: flag("collections_admitted"),
        };
        let reason = decide_overlay(facts)
            .err()
            .and_then(|reason| reason.model_name());
        assert_eq!(reason, case["inactive"].as_str(), "{case}");
    }
}
