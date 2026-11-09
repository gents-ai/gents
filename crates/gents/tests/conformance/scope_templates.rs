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
