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
    let case = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration contract");
    for item in &case.tool_operation_cases {
        let flag = |name: &str| item[name].as_bool().expect("modeled tool flag");
        let mut surface = ToolPolicySurface::secure_minimal();
        surface.schema_management = flag("schema");
        surface.session_history = flag("history");
        surface.goal_tools = flag("goal");
        surface.goal_create = flag("create");
        surface.self_config = flag("self_config");
        if flag("query") {
            surface.query_tools = EndpointScope::only_map(std::collections::BTreeMap::from([(
                ("tool".to_owned(), "collection".to_owned()),
                Default::default(),
            )]));
        }
        if flag("write") {
            surface.write_tools = EndpointScope::only_map(std::collections::BTreeMap::from([(
                ("tool".to_owned(), "collection".to_owned()),
                Default::default(),
            )]));
        }
        let category = item["category"].as_str().expect("modeled category scope");
        surface.self_config_categories = match category {
            "all" => EndpointScope::all(),
            "none" => EndpointScope::none(),
            category => EndpointScope::only_units([category.to_owned()]),
        };
        assert_eq!(
            projection(&surface, flag("pack")),
            item["sources"],
            "{}",
            item["name"]
        );

        if category == "none" {
            surface.self_config_categories = EndpointScope::only_units(Vec::<String>::new());
            assert_eq!(
                projection(&surface, flag("pack")),
                item["sources"],
                "{} empty category scope",
                item["name"]
            );
        }
    }
}
