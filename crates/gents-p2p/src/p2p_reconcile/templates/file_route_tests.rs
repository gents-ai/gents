use serde::Deserialize;

#[test]
fn generated_routes_follow_executable_scope_filters() {
    use super::super::policy::{
        client_route_collections, resolve_template_filters, PairingDirection,
    };
    use super::super::templates::{filter_conditions, resolve_template, CLIENT_TEMPLATE};
    use serde_json::{json, Value};

    #[derive(Deserialize)]
    struct Clause {
        field: String,
        operator: String,
        value: String,
    }
    #[derive(Deserialize)]
    struct Predicate {
        collection: String,
        clauses: Vec<Clause>,
    }
    #[derive(Deserialize)]
    struct Case {
        direction: String,
        requester: String,
        owner: String,
        demand_collection: String,
        image_collection: String,
        expected: Option<Predicate>,
        images_granted: bool,
        image_filter_installed: bool,
    }
    fn sorted_conjunction(mut value: Value) -> Value {
        if let Some(clauses) = value.get_mut("_and").and_then(Value::as_array_mut) {
            clauses.sort_by_key(Value::to_string);
        }
        value
    }
    let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases: Vec<Case> =
        serde_json::from_value(snapshot["file_review_route_cases"].clone()).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let direction = PairingDirection::parse(&case.direction).unwrap();
        let template = resolve_template(CLIENT_TEMPLATE).unwrap();
        let actual = resolve_template_filters(template, direction, &case.requester, &case.owner);
        match case.expected {
            // The model emits null when a route has no demand filter for the collection.
            None => assert!(
                !actual.contains_key(&case.demand_collection),
                "{}: unexpected demand filter for {}",
                case.direction,
                case.demand_collection
            ),
            Some(expected_predicate) => {
                assert_eq!(expected_predicate.collection, case.demand_collection);
                let mut clauses: Vec<Value> = expected_predicate
                    .clauses
                    .into_iter()
                    .map(|clause| json!({(clause.field): {(clause.operator): clause.value}}))
                    .collect();
                let expected = if clauses.len() == 1 {
                    clauses.remove(0)
                } else {
                    json!({"_and": clauses})
                };
                let predicate = actual.get(&case.demand_collection).unwrap();
                assert_eq!(
                    sorted_conjunction(Value::Object(filter_conditions(predicate).unwrap())),
                    sorted_conjunction(expected),
                    "{}",
                    case.direction
                );
            }
        }
        assert_eq!(
            client_route_collections(direction).contains(&case.image_collection.as_str()),
            case.images_granted
        );
        assert_eq!(
            actual.contains_key(&case.image_collection),
            case.image_filter_installed
        );
    }
}
