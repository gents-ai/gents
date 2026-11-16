#[test]
fn generated_routes_follow_executable_scope_filters() {
    use super::super::policy::{
        client_route_collections, resolve_template_filters, PairingDirection,
    };
    use super::super::templates::{resolve_template, single_string_eq, CLIENT_TEMPLATE};
    use gents_lean_vocab::families::file_review_routes::{
        CollectionPredicate, FileReviewRouteOutput, FilterClause,
    };

    gents_lean_vocab::families::file_review_routes::replay::<PairingDirection>(|input| {
        let direction = input.direction;
        let template = resolve_template(CLIENT_TEMPLATE).unwrap();
        let actual = resolve_template_filters(template, direction, &input.requester, &input.owner);
        FileReviewRouteOutput {
            expected: actual
                .get_key_value(&input.demand_collection)
                .map(|(collection, filter)| {
                    let (field, value) =
                        single_string_eq(filter).expect("a demand filter is one string equality");
                    CollectionPredicate {
                        collection: collection.clone(),
                        clauses: vec![FilterClause {
                            field: field.to_owned(),
                            operator: "_eq".to_owned(),
                            value: value.to_owned(),
                        }],
                    }
                }),
            images_granted: client_route_collections(direction)
                .contains(&input.image_collection.as_str()),
            image_filter_installed: actual.contains_key(&input.image_collection),
        }
    });
}
