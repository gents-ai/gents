#[test]
fn retired_reserved_id_predicate_matches_both_namespaces() {
    let graph_trigger = format!("graph-trigger-{}-{}", "a".repeat(64), "b".repeat(16));
    assert!(crate::canonical::is_retired_reserved_id(&graph_trigger));
    assert!(crate::canonical::is_retired_reserved_id("pkg-x"));
    assert!(!crate::canonical::is_retired_reserved_id("ordinary"));
}
