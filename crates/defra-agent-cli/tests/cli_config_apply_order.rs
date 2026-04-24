/// Anchors the desired-state write order to the topological rank declared by
/// `Collection::apply_order`.
///
/// This is a source-grep test: it reads the source of `config_import.rs` at
/// compile time and asserts that the centralized `CONFIG_APPLY_ORDER` table
/// appears in the expected order. No live DefraDB node is required.
#[test]
fn apply_desired_state_changes_order_matches_collection_apply_order() {
    let src = include_str!("../src/config_import.rs");
    let body_start = src.find("const CONFIG_APPLY_ORDER").unwrap();
    let body_end = src[body_start..].find("];").unwrap() + body_start;
    let body = &src[body_start..body_end];

    let re = regex::Regex::new(r"Collection::([A-Za-z]+)").unwrap();
    let found: Vec<&str> = re
        .captures_iter(body)
        .map(|c| c.get(1).unwrap().as_str())
        .collect();

    // Expected order preserves the existing manual order within each apply rank:
    //   rank 0: InferenceBackend, InferenceProfile, ToolServiceRegistry, ToolSelection
    //   rank 1: AgentBehavior
    //   rank 2: Task, Schedule
    //   rank 3: EventTrigger, AgentPrincipal
    let expected = vec![
        "InferenceBackend",
        "InferenceProfile",
        "ToolServiceRegistry",
        "ToolSelection",
        "AgentBehavior",
        "Task",
        "Schedule",
        "EventTrigger",
        "AgentPrincipal",
    ];
    assert_eq!(
        found, expected,
        "CONFIG_APPLY_ORDER does not match Collection::apply_order ranks"
    );
}
