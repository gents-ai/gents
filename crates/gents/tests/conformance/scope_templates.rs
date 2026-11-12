use std::collections::BTreeSet;

use gents_p2p::p2p_reconcile::templates::{
    builtin_templates, resolve_template, single_string_eq, Delivery,
};
use gents_p2p::p2p_reconcile::{
    client_route_collections, resolve_template_filters, PairingDirection, CLIENT_TEMPLATE,
};

#[test]
fn node_replication_matches_executable_scope_model() {
    let snapshot = gents_lean_vocab::lean_contract_snapshot();
    let template = resolve_template(CLIENT_TEMPLATE).expect("client template");
    for case in &snapshot.scope_node_route_cases {
        let direction = PairingDirection::parse(case.direction.as_str()).unwrap();
        assert_eq!(
            client_route_collections(direction).contains(&"Node"),
            case.allowed,
        );
        let filters = resolve_template_filters(
            template,
            direction,
            "did:key:reader",
            case.owner.as_str(),
        );
        assert_eq!(
            filters
                .get("Node")
                .and_then(single_string_eq)
                .map(|(_, value)| value),
            case.node_did.as_ref().and_then(|value| value.as_str())
        );
    }
}

#[test]
fn protocol_routes_match_model_actions() {
    let snapshot = gents_lean_vocab::lean_contract_snapshot();
    let mut modeled = BTreeSet::new();
    for case in &snapshot.scope_template_cases {
        let id = case.id.as_str();
        modeled.insert(id);
        let template = resolve_template(id).expect("modeled protocol route");
        assert_eq!(
            template
                .collections
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            case.collections
                .iter()
                .map(|value| value.as_str())
                .collect()
        );
        assert_eq!(
            match template.delivery {
                Delivery::Push => "push",
            },
            case.delivery.as_str()
        );
        let direction = PairingDirection::parse(case.direction.as_str()).unwrap();
        let filters =
            resolve_template_filters(template, direction, "did:key:reader", "did:key:owner");
        let expected = &case.filters;
        assert_eq!(filters.len(), expected.len());
        for expected in expected {
            let clause = &expected.clauses[0];
            assert_eq!(expected.clauses.len(), 1);
            assert_eq!(
                single_string_eq(
                    filters
                        .get(expected.collection.as_str())
                        .unwrap()
                ),
                Some((
                    clause.field.as_str(),
                    clause.value.as_str()
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

