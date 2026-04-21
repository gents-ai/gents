/// Anchors the write order in `apply_desired_state_changes` to the topological
/// rank declared by `Collection::apply_order`.
///
/// This is a source-grep test: it reads the source of `config_import.rs` at
/// compile time and asserts that the `apply_import_collection` assignments
/// inside the `Ok(ConfigApplyCounts { ... })` literal appear in the expected
/// order.  No live DefraDB node is required.
#[test]
fn apply_desired_state_changes_order_matches_collection_apply_order() {
    let src = include_str!("../src/config_import.rs");
    let body_start = src.find("Ok(ConfigApplyCounts {").unwrap();
    let body_end = src[body_start..].find("})").unwrap() + body_start;
    let body = &src[body_start..body_end];

    // Extract assignment keys in source order.
    let re = regex::Regex::new(
        r"(?m)^\s*(agent_principal|agent_behaviors|tool_selections|inference_backends|inference_profiles|tool_service_registries|scheduled_tasks):\s*apply_import_collection",
    )
    .unwrap();
    let found: Vec<&str> = re
        .captures_iter(body)
        .map(|c| c.get(1).unwrap().as_str())
        .collect();

    // Expected order = Collection::ALL sorted by (apply_order, graphql_type):
    //   rank 0: InferenceBackend, InferenceProfile, ToolServiceRegistry, ToolSelection
    //   rank 1: AgentPrincipal
    //   rank 2: AgentBehavior
    //   rank 3: ScheduledTask
    let expected = vec![
        "inference_backends",
        "inference_profiles",
        "tool_service_registries",
        "tool_selections",
        "agent_principal",
        "agent_behaviors",
        "scheduled_tasks",
    ];
    assert_eq!(
        found, expected,
        "apply_desired_state_changes write order does not match Collection::apply_order ranks"
    );
}
