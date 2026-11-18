use serde_json::{json, Value};

use super::{EndpointScope, ToolPolicySurface};

fn projection(surface: &ToolPolicySurface, pack: bool) -> Value {
    Value::Array(
        surface
            .covered_node_operation_sources(pack)
            .into_iter()
            .map(|source| json!({"purpose": source.purpose, "permission": source.permission.as_str()}))
            .collect(),
    )
}

#[test]
fn covered_tool_operations_follow_executable_identity_model() {
    gents_lean_vocab::families::household_tool_operation_source_projection::replay(|case| {
        let mut surface = ToolPolicySurface::secure_minimal();
        surface.schema_management = case.schema;
        surface.session_history = case.history;
        surface.goal_tools = case.goal;
        surface.goal_create = case.create;
        surface.self_config = case.self_config;
        if case.query {
            surface.query_tools = EndpointScope::only_map(std::collections::BTreeMap::from([(
                ("tool".to_owned(), "collection".to_owned()),
                Default::default(),
            )]));
        }
        if case.write {
            surface.write_tools = EndpointScope::only_map(std::collections::BTreeMap::from([(
                ("tool".to_owned(), "collection".to_owned()),
                Default::default(),
            )]));
        }
        surface.self_config_categories = match case.category.as_str() {
            "all" => EndpointScope::all(),
            "none" => EndpointScope::none(),
            category => EndpointScope::<String, ()>::only_units([category.to_owned()]),
        };
        let projected = projection(&surface, case.pack);
        if case.category == "none" {
            let mut empty = surface.clone();
            empty.self_config_categories =
                EndpointScope::<String, ()>::only_units(Vec::<String>::new());
            assert_eq!(
                projection(&empty, case.pack),
                projected,
                "{} empty category scope",
                case.name
            );
        }
        serde_json::from_value::<Vec<gents_lean_vocab::families::household_tool_operation_source_projection::OperationSource>>(projected)
            .expect("projected operation sources")
    });
}

#[test]
fn p2p_grants_install_the_modeled_pairing_sources() {
    for case in gents_lean_vocab::families::application_pairing_operation_source_cases::cases() {
        let mut surface = ToolPolicySurface::secure_minimal();
        surface.p2p_read = case.input.p2p_read;
        surface.p2p_mutate = case.input.p2p_mutate;
        case.check(serde_json::from_value(projection(&surface, false)).expect("modeled sources"));
    }
}
